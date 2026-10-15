//! The agent's own CPU use. A load generator that runs out of CPU starts calls late and reads
//! answers late, and its figures then describe itself rather than the gateway; the tester
//! warns when an agent comes close.

use std::time::{Duration, Instant};

/// CPU time used by this process between two samples.
pub struct ProcessCpu {
    last: Option<(Instant, Duration)>,
}

impl ProcessCpu {
    pub fn new() -> ProcessCpu {
        ProcessCpu { last: cpu_time().map(|c| (Instant::now(), c)) }
    }

    /// Percent of one CPU since the previous sample (400 is four CPUs fully used), or `None`
    /// where the figure is not available.
    pub fn sample(&mut self) -> Option<f64> {
        let now = (Instant::now(), cpu_time()?);
        let (wall, cpu) = self.last.replace(now)?;
        let wall = now.0.duration_since(wall).as_secs_f64();
        (wall > 0.0).then(|| now.1.saturating_sub(cpu).as_secs_f64() / wall * 100.0)
    }
}

impl Default for ProcessCpu {
    fn default() -> Self {
        Self::new()
    }
}

fn cpu_time() -> Option<Duration> {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes a whole `rusage` into the pointer it is given and reads nothing;
    // the value is used only when it reports success.
    let usage = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) != 0 {
            return None;
        }
        usage.assume_init()
    };
    let tv = |t: libc::timeval| Duration::new(t.tv_sec as u64, (t.tv_usec as u32).saturating_mul(1000));
    Some(tv(usage.ru_utime) + tv(usage.ru_stime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn busy_work_shows_as_cpu_use() {
        let mut cpu = ProcessCpu::new();
        let until = Instant::now() + Duration::from_millis(100);
        let mut x = 0u64;
        while Instant::now() < until {
            x = std::hint::black_box(x.wrapping_mul(31).wrapping_add(7));
        }
        let pct = cpu.sample().expect("getrusage works on the test platforms");
        assert!(pct > 20.0, "{pct}");
    }
}
