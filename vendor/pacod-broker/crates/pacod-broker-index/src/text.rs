//! Text search: a small inverted index over words, scored with BM25.
//!
//! Words are runs of letters and digits, lower-cased, at least two characters long. A query
//! matches a resource when every query word is a word of the resource or, for words of three
//! characters or more, the prefix of one; so `llam` finds `llama`. Names, titles and tags count
//! three times, so a match there ranks above one deep in a description.

use std::collections::{BTreeMap, HashMap};

/// Splits text into index words.
pub fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 2).map(str::to_lowercase)
}

/// The text index of one generation.
#[derive(Debug, Default)]
pub struct TextIndex {
    /// Word → (entry, weighted term frequency), entries ascending.
    postings: BTreeMap<String, Vec<(u32, u16)>>,
    doc_len: Vec<u32>,
    avg_len: f64,
}

/// The weighted words of one document, built per entry.
#[derive(Debug, Default)]
pub struct Doc(HashMap<String, u16>);

impl Doc {
    pub fn add(&mut self, text: &str, weight: u16) {
        for w in words(text) {
            let e = self.0.entry(w).or_insert(0);
            *e = e.saturating_add(weight);
        }
    }
}

impl TextIndex {
    pub fn build(docs: Vec<Doc>) -> Self {
        let mut postings: BTreeMap<String, Vec<(u32, u16)>> = BTreeMap::new();
        let mut doc_len = Vec::with_capacity(docs.len());
        for (i, d) in docs.into_iter().enumerate() {
            doc_len.push(d.0.values().map(|v| *v as u32).sum());
            for (w, tf) in d.0 {
                postings.entry(w).or_default().push((i as u32, tf));
            }
        }
        let avg_len = if doc_len.is_empty() {
            1.0
        } else {
            doc_len.iter().map(|v| *v as f64).sum::<f64>() / doc_len.len() as f64
        };
        Self { postings, doc_len, avg_len: avg_len.max(1.0) }
    }

    /// The entries that match every word of `q`, with their BM25 score. `None` when the query
    /// has no words, so it filters nothing.
    pub fn search(&self, q: &str) -> Option<HashMap<u32, f64>> {
        let qwords: Vec<String> = words(q).collect();
        if qwords.is_empty() {
            return None;
        }
        let n = self.doc_len.len();
        // Dense arrays, not maps: a common word can match every entry, and adding to an array
        // indexed by entry costs nothing next to hashing.
        let mut total = vec![0f64; n];
        let mut hits = vec![0u16; n];
        for (round, w) in qwords.iter().enumerate() {
            let mut this = vec![0f64; n];
            let mut add = |list: &Vec<(u32, u16)>, exact: bool| {
                let df = list.len() as f64;
                let idf = ((n as f64 - df + 0.5) / (df + 0.5) + 1.0).ln();
                for &(doc, tf) in list {
                    let tf = tf as f64;
                    let len = self.doc_len[doc as usize] as f64;
                    let s = idf * (tf * 2.2) / (tf + 1.2 * (0.25 + 0.75 * len / self.avg_len));
                    // A prefix match counts for less than the whole word.
                    let s = if exact { s } else { s * 0.6 };
                    let slot = &mut this[doc as usize];
                    *slot = slot.max(s);
                }
            };
            if let Some(list) = self.postings.get(w) {
                add(list, true);
            }
            if w.chars().count() >= 3 {
                for (k, list) in self.postings.range(w.clone()..) {
                    if !k.starts_with(w.as_str()) {
                        break;
                    }
                    if k != w {
                        add(list, false);
                    }
                }
            }
            let mut any = false;
            for d in 0..n {
                if this[d] > 0.0 && hits[d] as usize == round {
                    hits[d] += 1;
                    total[d] += this[d];
                    any = true;
                }
            }
            if !any {
                return Some(HashMap::new());
            }
        }
        let want = qwords.len() as u16;
        Some((0..n).filter(|d| hits[*d] == want).map(|d| (d as u32, total[d])).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_word_must_match_and_prefixes_count() {
        let mut a = Doc::default();
        a.add("llama-3.3-70b", 3);
        a.add("Llama on H100s in Sofia", 1);
        let mut b = Doc::default();
        b.add("weather mcp server", 3);
        let idx = TextIndex::build(vec![a, b]);
        let r = idx.search("llam sofia").unwrap();
        assert_eq!(r.keys().copied().collect::<Vec<_>>(), vec![0]);
        assert!(idx.search("llama weather").unwrap().is_empty());
        assert!(idx.search("  - ").is_none());
        assert_eq!(idx.search("WEATHER").unwrap().len(), 1);
    }
}
