use crate::{FileDiff, Line, LineKind};

/// How selected lines are applied when rebuilding a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectMode {
    /// Start from the old side and apply the selected changes
    /// (stage lines: old = index, new = worktree; result = new index).
    Apply,
    /// Start from the new side and revert the selected changes
    /// (unstage: old = HEAD, new = index; result = new index;
    /// discard: old = index, new = worktree; result = new worktree).
    Revert,
}

/// Rebuilds file content from a full-context diff (see [`crate::Options::full`]),
/// applying or reverting only the lines for which `selected` is true.
///
/// Line terminators are preserved byte-for-byte.
pub fn apply_selection(
    d: &FileDiff,
    selected: impl Fn(&Line) -> bool,
    mode: SelectMode,
) -> Vec<u8> {
    assert!(d.is_full(), "apply_selection needs a full-context diff");
    let mut out = Vec::with_capacity(d.text.len());
    // A line may only lack its newline at the very end of the file; when more
    // lines follow a former last line, its newline is restored.
    let mut missing_eol = false;
    let mut emit = |bytes: &[u8], no_eol: bool| {
        if missing_eol {
            out.push(b'\n');
        }
        out.extend_from_slice(bytes);
        missing_eol = no_eol;
        if !no_eol {
            out.push(b'\n');
        }
    };
    for l in &d.lines {
        let sel = selected(l);
        match (l.kind, mode) {
            (LineKind::Context, SelectMode::Apply) => emit(d.old_line_bytes(l), l.old_no_eol),
            (LineKind::Context, SelectMode::Revert) => emit(d.line_bytes(l), l.new_no_eol),
            (LineKind::Removed, SelectMode::Apply) => {
                if !sel {
                    emit(d.line_bytes(l), l.old_no_eol)
                }
            }
            (LineKind::Added, SelectMode::Apply) => {
                if sel {
                    emit(d.line_bytes(l), l.new_no_eol)
                }
            }
            (LineKind::Removed, SelectMode::Revert) => {
                if sel {
                    emit(d.line_bytes(l), l.old_no_eol)
                }
            }
            (LineKind::Added, SelectMode::Revert) => {
                if !sel {
                    emit(d.line_bytes(l), l.new_no_eol)
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Options, diff};

    fn run(old: &[u8], new: &[u8], pick: &[u32], mode: SelectMode) -> String {
        let d = diff(old, new, Options::full());
        // select by line number on the side that carries the change
        let out = apply_selection(
            &d,
            |l| match l.kind {
                LineKind::Added => pick.contains(&(1000 + l.new_no)),
                LineKind::Removed => pick.contains(&l.old_no),
                LineKind::Context => false,
            },
            mode,
        );
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn stage_some_lines() {
        // old: a b c   new: a X c Y   (b removed, X and Y added)
        let r = run(b"a\nb\nc\n", b"a\nX\nc\nY\n", &[2, 1002], SelectMode::Apply);
        assert_eq!(r, "a\nX\nc\n");
        let r = run(b"a\nb\nc\n", b"a\nX\nc\nY\n", &[1004], SelectMode::Apply);
        assert_eq!(r, "a\nb\nc\nY\n");
    }

    #[test]
    fn revert_some_lines() {
        let r = run(b"a\nb\nc\n", b"a\nX\nc\nY\n", &[1004], SelectMode::Revert);
        assert_eq!(r, "a\nX\nc\n");
        let r = run(
            b"a\nb\nc\n",
            b"a\nX\nc\nY\n",
            &[2, 1002],
            SelectMode::Revert,
        );
        assert_eq!(r, "a\nb\nc\nY\n");
    }

    #[test]
    fn crlf_roundtrip() {
        let r = run(b"a\r\nb\r\n", b"a\r\nB\r\n", &[2, 1002], SelectMode::Apply);
        assert_eq!(r, "a\r\nB\r\n");
    }

    #[test]
    fn select_everything_equals_new() {
        let old = b"1\n2\n3\n4\n5\n";
        let new = b"1\nzwei\n3\nvier\n5\nsechs";
        let d = diff(old, new, Options::full());
        assert_eq!(apply_selection(&d, |_| true, SelectMode::Apply), new);
        assert_eq!(apply_selection(&d, |_| true, SelectMode::Revert), old);
        assert_eq!(apply_selection(&d, |_| false, SelectMode::Apply), old);
        assert_eq!(apply_selection(&d, |_| false, SelectMode::Revert), new);
    }
}
