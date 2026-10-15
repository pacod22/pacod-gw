//! Latency histograms that add up.
//!
//! Percentiles cannot be combined: the p99 of two agents is not a function of their two p99s.
//! Counts per bucket can. Every agent therefore reports, for each second, how many calls fell
//! into each bucket, and the tester adds the buckets of all agents and all seconds before it
//! takes a percentile.
//!
//! The buckets are integer log-linear over microseconds, so that the agent (Rust) and the
//! tester (Python) agree on them without floating-point logarithms, which can differ by one
//! bucket at an edge:
//!
//! - a value below 128 µs is its own bucket (index = value);
//! - above, each power of two is split into 64 equal buckets: for a value `v` with highest set
//!   bit `m` (`m >= 7`), `shift = m - 6`, `mantissa = v >> shift` (64..=127), and the index is
//!   `128 + (shift - 1) * 64 + (mantissa - 64)`.
//!
//! A bucket is at most 1/64 of its lower bound wide, so its midpoint is within 0.8% of every
//! value in it. Ten minutes (6e8 µs) is index 1543. Only the agent maps values to indexes; the
//! tester needs [`bounds`] alone. `tests/contract/histogram.json` holds the vectors both
//! sides are tested against.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Values below this have a bucket of their own.
const LINEAR: u64 = 128;
/// Buckets per power of two above [`LINEAR`].
const PER_OCTAVE: u32 = 64;

/// The bucket of a value in microseconds.
pub fn index(us: u64) -> u32 {
    if us < LINEAR {
        return us as u32;
    }
    let msb = 63 - us.leading_zeros();
    let shift = msb - 6;
    let mantissa = (us >> shift) as u32;
    LINEAR as u32 + (shift - 1) * PER_OCTAVE + (mantissa - PER_OCTAVE)
}

/// The smallest and largest value of a bucket, both included.
pub fn bounds(index: u32) -> (u64, u64) {
    if u64::from(index) < LINEAR {
        return (u64::from(index), u64::from(index));
    }
    let k = index - LINEAR as u32;
    let shift = k / PER_OCTAVE + 1;
    let mantissa = u128::from(k % PER_OCTAVE + PER_OCTAVE);
    // The top bucket's upper end is 2^64, one past u64::MAX.
    let lo = (mantissa << shift).min(u128::from(u64::MAX)) as u64;
    let hi = (((mantissa + 1) << shift) - 1).min(u128::from(u64::MAX)) as u64;
    (lo, hi)
}

/// The value a bucket stands for: its midpoint.
pub fn value(index: u32) -> f64 {
    let (lo, hi) = bounds(index);
    (lo as f64 + hi as f64) / 2.0
}

/// Calls counted by bucket, with the exact count, sum, smallest and largest value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Histogram {
    pub n: u64,
    pub min: u64,
    pub max: u64,
    /// Microseconds; saturates instead of wrapping.
    pub sum: u64,
    /// Sparse: `[[index, count], …]` in index order, only buckets with a count.
    #[serde(serialize_with = "sparse_out", deserialize_with = "sparse_in")]
    pub b: BTreeMap<u32, u64>,
}

impl Histogram {
    pub fn record(&mut self, us: u64) {
        if self.n == 0 {
            self.min = us;
            self.max = us;
        } else {
            self.min = self.min.min(us);
            self.max = self.max.max(us);
        }
        self.n += 1;
        self.sum = self.sum.saturating_add(us);
        *self.b.entry(index(us)).or_default() += 1;
    }

    pub fn merge(&mut self, other: &Histogram) {
        if other.n == 0 {
            return;
        }
        if self.n == 0 {
            self.min = other.min;
            self.max = other.max;
        } else {
            self.min = self.min.min(other.min);
            self.max = self.max.max(other.max);
        }
        self.n += other.n;
        self.sum = self.sum.saturating_add(other.sum);
        for (i, c) in &other.b {
            *self.b.entry(*i).or_default() += c;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.n == 0
    }

    /// The value below which `q` of the calls lie, in microseconds: nearest rank, as
    /// `gw-load` has always counted, taken as the bucket's midpoint and kept within the
    /// exact smallest and largest value. 0 when empty.
    pub fn percentile(&self, q: f64) -> f64 {
        if self.n == 0 {
            return 0.0;
        }
        let rank = ((q * self.n as f64).ceil() as u64).clamp(1, self.n);
        let mut seen = 0;
        for (i, c) in &self.b {
            seen += c;
            if seen >= rank {
                return value(*i).clamp(self.min as f64, self.max as f64);
            }
        }
        self.max as f64
    }

    pub fn mean(&self) -> f64 {
        if self.n == 0 { 0.0 } else { self.sum as f64 / self.n as f64 }
    }
}

fn sparse_out<S: Serializer>(b: &BTreeMap<u32, u64>, s: S) -> Result<S::Ok, S::Error> {
    s.collect_seq(b.iter())
}

fn sparse_in<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<u32, u64>, D::Error> {
    let pairs: Vec<(u32, u64)> = Vec::deserialize(d)?;
    let mut b = BTreeMap::new();
    for (i, c) in pairs {
        *b.entry(i).or_default() += c;
    }
    Ok(b)
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn edges_fall_into_the_documented_buckets() {
        for (v, i) in [
            (0, 0),
            (127, 127),
            (128, 128),
            (129, 128),
            (130, 129),
            (191, 159),
            (192, 160),
            (255, 191),
            (256, 192),
            (1_000, 317),
            (1_000_000, 954),
            (600_000_000, 1543),
            (u64::MAX, 3775),
        ] {
            assert_eq!(index(v), i, "value {v}");
        }
        assert_eq!(bounds(128), (128, 129));
        assert_eq!(bounds(191), (254, 255));
        assert_eq!(bounds(192), (256, 259));
        assert_eq!(bounds(3775).1, u64::MAX);
    }

    proptest! {
        #[test]
        fn a_value_lies_in_its_bucket_and_near_its_midpoint(v in 0u64..(1 << 50)) {
            let i = index(v);
            let (lo, hi) = bounds(i);
            prop_assert!(lo <= v && v <= hi);
            if v > 0 {
                prop_assert!((value(i) - v as f64).abs() / v as f64 <= 0.008);
            }
        }

        #[test]
        fn merging_is_the_same_as_recording_everything_once(a in prop::collection::vec(0u64..10_000_000, 0..50),
                                                           b in prop::collection::vec(0u64..10_000_000, 0..50)) {
            let (mut ha, mut hb, mut all) = (Histogram::default(), Histogram::default(), Histogram::default());
            a.iter().for_each(|v| { ha.record(*v); all.record(*v); });
            b.iter().for_each(|v| { hb.record(*v); all.record(*v); });
            ha.merge(&hb);
            prop_assert_eq!(ha, all);
        }

        #[test]
        fn percentiles_are_within_a_bucket_of_the_exact_ones(mut v in prop::collection::vec(1u64..100_000_000, 1..400),
                                                             q in 0.01f64..1.0) {
            let mut h = Histogram::default();
            v.iter().for_each(|x| h.record(*x));
            v.sort_unstable();
            let rank = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len());
            let exact = v[rank - 1] as f64;
            prop_assert!((h.percentile(q) - exact).abs() <= exact * 0.008 + 0.5);
        }
    }

    #[test]
    fn the_sparse_form_round_trips() {
        let mut h = Histogram::default();
        for v in [5, 5, 300, 2_000_000] {
            h.record(v);
        }
        let text = serde_json::to_string(&h).unwrap();
        assert_eq!(text, r#"{"n":4,"min":5,"max":2000000,"sum":2000310,"b":[[5,2],[203,1],[1018,1]]}"#);
        assert_eq!(serde_json::from_str::<Histogram>(&text).unwrap(), h);
        assert_eq!(h.percentile(0.5), 5.0);
        assert_eq!(h.percentile(1.0), 2_000_000.0);
        assert_eq!(Histogram::default().percentile(0.5), 0.0);
    }
}
