use crate::{FileDiff, LineKind};

/// What a partial patch will be used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatchMode {
    /// The diff is index -> worktree; the patch is applied forward to the index
    /// (`git apply --cached`).
    Stage,
    /// The diff is HEAD -> index; the patch is applied in reverse to the index
    /// (`git apply --cached --reverse`).
    Unstage,
    /// The diff is index -> worktree; the patch is applied in reverse to the
    /// worktree (`git apply --reverse`).
    Discard,
}

/// Builds a patch containing only the selected changed lines.
///
/// `selected(i)` is asked for every line index of `d.lines`; context lines are
/// ignored. Returns `None` when nothing selected is a change. Hunk headers are
/// exact, so `git apply` works without `--recount`.
pub fn build_patch(
    path: &str,
    d: &FileDiff,
    selected: impl Fn(usize) -> bool,
    mode: PatchMode,
) -> Option<String> {
    let reverse = mode != PatchMode::Stage;
    let mut out = format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n");
    let mut any = false;
    let mut delta: i64 = 0;
    for h in &d.hunks {
        let has_selection = h.lines.clone().any(|i| d.lines[i].kind != LineKind::Context && selected(i));
        if !has_selection {
            continue;
        }
        let mut body = String::new();
        let (mut old_count, mut new_count) = (0i64, 0i64);
        for i in h.lines.clone() {
            let l = &d.lines[i];
            let sel = selected(i);
            let mark = match (l.kind, reverse, sel) {
                (LineKind::Context, _, _) => ' ',
                (LineKind::Removed, false, true) => '-',
                (LineKind::Removed, false, false) => ' ',
                (LineKind::Added, false, true) => '+',
                (LineKind::Added, false, false) => continue,
                (LineKind::Removed, true, true) => '-',
                (LineKind::Removed, true, false) => continue,
                (LineKind::Added, true, true) => '+',
                (LineKind::Added, true, false) => ' ',
            };
            match mark {
                ' ' => {
                    old_count += 1;
                    new_count += 1;
                }
                '-' => old_count += 1,
                _ => new_count += 1,
            }
            body.push(mark);
            body.push_str(&String::from_utf8_lossy(d.line_bytes(l)));
            body.push('\n');
            if l.no_eol() {
                body.push_str("\\ No newline at end of file\n");
            }
        }
        let (old_start, new_start) = if !reverse {
            // The old side (index) is unchanged: its positions are exact.
            let old_start = h.old_start as i64;
            let base = if old_count == 0 { old_start + 1 } else { old_start };
            let mut new_start = base + delta;
            if new_count == 0 {
                new_start -= 1;
            }
            (old_start, new_start)
        } else {
            // The new side is what the target currently contains.
            let new_start = h.new_start as i64;
            let base = if new_count == 0 { new_start + 1 } else { new_start };
            let mut old_start = base - delta;
            if old_count == 0 {
                old_start -= 1;
            }
            (old_start, new_start)
        };
        delta += new_count - old_count;
        out.push_str(&format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"));
        out.push_str(&body);
        any = true;
    }
    any.then_some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Options, diff};

    #[test]
    fn stage_single_added_line() {
        let d = diff(b"a\nb\n", b"a\nx\ny\nb\n", Options::default());
        // lines: a(ctx) x(+) y(+) b(ctx); select only "x"
        let p = build_patch("f", &d, |i| i == 1, PatchMode::Stage).unwrap();
        assert!(p.contains("@@ -1,2 +1,3 @@\n a\n+x\n b\n"), "{p}");
    }

    #[test]
    fn stage_partial_removal_keeps_unselected_as_context() {
        let d = diff(b"a\nb\nc\n", b"a\n", Options::default());
        // a(ctx) b(-) c(-); select only b
        let p = build_patch("f", &d, |i| i == 1, PatchMode::Stage).unwrap();
        assert!(p.contains("@@ -1,3 +1,2 @@\n a\n-b\n c\n"), "{p}");
    }

    #[test]
    fn unstage_reverse_keeps_unselected_additions() {
        let d = diff(b"a\n", b"a\nx\ny\n", Options::default());
        // a(ctx) x(+) y(+); unstage only y
        let p = build_patch("f", &d, |i| i == 2, PatchMode::Unstage).unwrap();
        assert!(p.contains("@@ -1,2 +1,3 @@\n a\n x\n+y\n"), "{p}");
    }

    #[test]
    fn nothing_selected() {
        let d = diff(b"a\n", b"b\n", Options::default());
        assert!(build_patch("f", &d, |_| false, PatchMode::Stage).is_none());
    }
}
