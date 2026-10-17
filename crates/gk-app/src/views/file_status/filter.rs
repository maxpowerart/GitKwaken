//! Path filter of the file lists, fast enough for 1M paths per keystroke:
//!
//! - **parallel**: the entry list is cut into chunks filtered on the rayon
//!   pool (one nucleo matcher per chunk); chunk results are concatenated in
//!   order, so the output is in status order like a sequential pass.
//! - **incremental**: when the new query only narrows the previous one (the
//!   user typed more characters), only the previous matches are filtered.

use super::list::kind_bit;
use super::tree::Filter;
use gk_status::Entry;
use rayon::prelude::*;
use std::ops::Range;

/// Entries per parallel work item.
const CHUNK: usize = 8192;
/// Below this many candidates one thread is faster than fanning out.
const PAR_MIN: usize = 32_768;

/// Matches of one filter pass, kept to narrow the next one.
#[derive(Debug, Default, Clone)]
pub struct Matched {
    pub status_gen: u64,
    pub kinds: u8,
    pub query: String,
    /// Indices into the list's paths, ascending.
    pub idx: Vec<u32>,
}

impl Matched {
    /// The previous matches when they are a superset of what `query` /
    /// `kinds` can match on the same status.
    pub fn reusable(&self, status_gen: u64, kinds: u8, query: &str) -> Option<&[u32]> {
        (self.status_gen == status_gen && self.kinds == kinds && narrows(&self.query, query))
            .then_some(self.idx.as_slice())
    }
}

/// True when every path matching `new` also matches `old` (fzf syntax as
/// parsed by nucleo), so `new` can be run on `old`'s matches only.
///
/// Conservative: whitespace-separated atoms of `old` must reappear in `new`
/// unchanged, except the last one, which `new` may extend. Negations
/// (`!`), anchors at the end (`$`) and escapes (`\`) in `old` change meaning
/// when extended, so they disable reuse; a bare `^` / `'` atom is ambiguous.
pub fn narrows(old: &str, new: &str) -> bool {
    let old_atoms: Vec<&str> = old.split_whitespace().collect();
    let new_atoms: Vec<&str> = new.split_whitespace().collect();
    let Some((last, head)) = old_atoms.split_last() else {
        return true; // the empty query matched everything
    };
    if old.contains(['!', '$', '\\']) || new.contains(['!', '\\']) {
        return false;
    }
    if last.trim_start_matches(['^', '\'']).is_empty() || new_atoms.len() < old_atoms.len() {
        return false;
    }
    // "foo" -> "foo$" is fine too: the suffix form of an atom is narrower
    // than its fuzzy form.
    head.iter().zip(&new_atoms).all(|(a, b)| a == b) && new_atoms[head.len()].starts_with(last)
}

/// Indices of the paths that pass the status chips (`kinds`, 0 = all) and
/// the fuzzy `query`, in ascending order. `within` restricts the search to
/// earlier matches (see [`Matched::reusable`]).
pub fn filter_indices(
    paths: &[String],
    entries: &[Entry],
    kinds: u8,
    query: &str,
    within: Option<&[u32]>,
) -> Vec<u32> {
    let n = within.map_or(paths.len(), <[u32]>::len);
    if n < PAR_MIN {
        return pass(paths, entries, kinds, query, within, 0..n);
    }
    let parts: Vec<Vec<u32>> = (0..n.div_ceil(CHUNK))
        .into_par_iter()
        .map(|c| {
            let r = c * CHUNK..((c + 1) * CHUNK).min(n);
            pass(paths, entries, kinds, query, within, r)
        })
        .collect();
    let mut out = Vec::with_capacity(parts.iter().map(Vec::len).sum());
    for p in parts {
        out.extend_from_slice(&p);
    }
    out
}

/// Sequential filter over candidates `range` (of `within`, or of all paths).
fn pass(
    paths: &[String],
    entries: &[Entry],
    kinds: u8,
    query: &str,
    within: Option<&[u32]>,
    range: Range<usize>,
) -> Vec<u32> {
    let mut f = Filter::new(query);
    let mut out = Vec::new();
    let mut check = |i: u32| {
        let kind_ok = kinds == 0
            || entries
                .get(i as usize)
                .is_some_and(|e| kinds & kind_bit(e.kind) != 0);
        if kind_ok && paths.get(i as usize).is_some_and(|p| f.matches(p)) {
            out.push(i);
        }
    };
    match within {
        Some(w) => w[range].iter().for_each(|&i| check(i)),
        None => range.for_each(|i| check(i as u32)),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use gk_status::Kind;
    use std::time::Instant;

    fn synthetic(n: usize) -> Vec<String> {
        let mut v: Vec<String> = (0..n)
            .map(|i| {
                format!(
                    "src/module_{:03}/sub_{:02}/file_{i:07}.{}",
                    i % 997,
                    i % 37,
                    ["rs", "txt", "md", "json"][i % 4]
                )
            })
            .collect();
        v.sort();
        v
    }

    fn sequential(paths: &[String], query: &str) -> Vec<u32> {
        let mut f = Filter::new(query);
        (0..paths.len() as u32)
            .filter(|&i| f.matches(&paths[i as usize]))
            .collect()
    }

    #[test]
    fn narrowing_rules() {
        assert!(narrows("", "anything"));
        assert!(narrows("", "!neg"));
        assert!(narrows("mod", "modu"));
        assert!(narrows("mod", "mod rs"));
        assert!(narrows("mod rs", "mod rs$"));
        assert!(narrows("mod", "mod$"));
        assert!(narrows("^src", "^src/m"));
        assert!(narrows("'ule", "'ule_"));
        assert!(!narrows("modu", "mod"), "shorter query widens");
        assert!(!narrows("mod rs", "mod"));
        assert!(!narrows("mod rs", "mox rs"));
        assert!(!narrows("!te", "!tes"), "longer negation widens");
        assert!(!narrows("mod", "mod !x"));
        assert!(!narrows("rs$", "rs$x"));
        assert!(!narrows("^", "^a"), "bare ^ is ambiguous");
        assert!(!narrows("'", "'a"));
        assert!(!narrows("a\\", "a\\ b"));
    }

    #[test]
    fn parallel_and_incremental_match_sequential() {
        let paths = synthetic(100_000);
        let mut prev: Option<Matched> = None;
        for q in [
            "",
            "m",
            "mo",
            "mod",
            "mod 12",
            "mod 123",
            "mod 123 rs",
            "mod 12",
            "'sub_0 json$",
            "!txt",
        ] {
            let full = filter_indices(&paths, &[], 0, q, None);
            assert_eq!(full, sequential(&paths, q), "parallel {q:?}");
            if let Some(m) = &prev
                && let Some(w) = m.reusable(1, 0, q)
            {
                assert_eq!(
                    filter_indices(&paths, &[], 0, q, Some(w)),
                    full,
                    "incremental {q:?}"
                );
            }
            prev = Some(Matched {
                status_gen: 1,
                kinds: 0,
                query: q.into(),
                idx: full,
            });
        }
    }

    #[test]
    fn kinds_combine_with_query_in_parallel() {
        let paths = synthetic(50_000);
        let entries: Vec<Entry> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let k = if i % 3 == 0 {
                    Kind::Untracked
                } else {
                    Kind::Modified
                };
                Entry::new(p.as_str(), k)
            })
            .collect();
        let got = filter_indices(&paths, &entries, 16, "rs$", None);
        assert!(!got.is_empty());
        assert!(got.windows(2).all(|w| w[0] < w[1]), "status order");
        assert!(
            got.iter()
                .all(|&i| i % 3 == 0 && paths[i as usize].ends_with(".rs"))
        );
    }

    /// `cargo test --release -p gk-app -- --ignored --nocapture filter_1m`
    #[test]
    #[ignore]
    fn filter_1m_paths_per_keystroke() {
        let paths = synthetic(1_000_000);
        let typed = "module_12 sub_3 rs";
        let mut prev: Option<Matched> = None;
        let mut worst = 0.0f64;
        // Warm up the rayon pool.
        let _ = filter_indices(&paths, &[], 0, "x", None);
        println!("keystroke                 full(seq)  full(par)  incremental  matches");
        for end in 1..=typed.len() {
            let q = &typed[..end];
            let t = Instant::now();
            let seq = sequential(&paths, q);
            let t_seq = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let par = filter_indices(&paths, &[], 0, q, None);
            let t_par = t.elapsed().as_secs_f64() * 1e3;
            let t = Instant::now();
            let within = prev.as_ref().and_then(|m| m.reusable(1, 0, q));
            let inc = filter_indices(&paths, &[], 0, q, within);
            let t_inc = t.elapsed().as_secs_f64() * 1e3;
            assert_eq!(seq, par);
            assert_eq!(inc, par);
            println!(
                "{q:<24} {t_seq:>8.1}ms {t_par:>8.1}ms {t_inc:>10.1}ms  {}",
                inc.len()
            );
            worst = worst.max(t_inc);
            prev = Some(Matched {
                status_gen: 1,
                kinds: 0,
                query: q.into(),
                idx: inc,
            });
        }
        // Backspace: no reuse, full parallel pass.
        let t = Instant::now();
        let _ = filter_indices(&paths, &[], 0, "module_1", None);
        let t_back = t.elapsed().as_secs_f64() * 1e3;
        println!("backspace (full pass)     {t_back:.1}ms; worst keystroke {worst:.1}ms");
        assert!(worst.max(t_back) < 50.0, "over the 50 ms budget");
    }
}
