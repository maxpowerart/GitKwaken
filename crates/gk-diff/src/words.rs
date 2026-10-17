use imara_diff::{Algorithm, Diff, InternedInput};
use std::ops::Range;

/// Splits into word, whitespace and punctuation tokens, returning byte ranges.
fn tokens(s: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prev: Option<u8> = None;
    let class = |c: char| -> u8 {
        if c.is_alphanumeric() || c == '_' {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    for (i, c) in s.char_indices() {
        let cl = class(c);
        // Punctuation is emitted one char at a time.
        if prev.is_some() && (Some(cl) != prev || cl == 2) {
            out.push(start..i);
            start = i;
        }
        prev = Some(cl);
    }
    if start < s.len() {
        out.push(start..s.len());
    }
    out
}

/// Byte ranges that differ between a removed and an added line, for
/// highlighting changed words inside modified lines.
pub fn inline_changes(old: &str, new: &str) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    if old.len() > 4000 || new.len() > 4000 {
        return (Vec::new(), Vec::new());
    }
    let ta = tokens(old);
    let tb = tokens(new);
    let a: Vec<&str> = ta.iter().map(|r| &old[r.clone()]).collect();
    let b: Vec<&str> = tb.iter().map(|r| &new[r.clone()]).collect();
    let input = InternedInput::new(crate::Seq(&a), crate::Seq(&b));
    let d = Diff::compute(Algorithm::Myers, &input);
    let mut ra = Vec::new();
    let mut rb = Vec::new();
    for h in d.hunks() {
        if h.before.start < h.before.end {
            ra.push(ta[h.before.start as usize].start..ta[h.before.end as usize - 1].end);
        }
        if h.after.start < h.after.end {
            rb.push(tb[h.after.start as usize].start..tb[h.after.end as usize - 1].end);
        }
    }
    // Lines that share almost nothing are better shown without highlights.
    let changed: usize = rb.iter().map(|r| r.len()).sum();
    if !new.is_empty() && changed * 10 > new.len() * 8 {
        return (Vec::new(), Vec::new());
    }
    (ra, rb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_changed_word() {
        let (a, b) = inline_changes("let x = foo(1);", "let x = bar(1);");
        assert_eq!(a, [8..11]);
        assert_eq!(b, [8..11]);
    }
}
