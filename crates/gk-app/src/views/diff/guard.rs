//! Size guard for large files (`settings.diff.max_file_kb`): the sizes of
//! both sides are probed in the background (object headers, worktree file
//! size) before the diff loads; bigger files show "Show anyway" instead.
//! Plus detection of Git LFS pointer files.

use gk_core::Repo;
use gk_core::content::DiffTarget;
use gk_diff::{FileDiff, LineKind};
use parking_lot::Mutex;
use std::sync::Arc;

/// Largest side of `target` in bytes; `None` when unknown.
pub fn probe_size(repo: &Repo, target: &DiffTarget) -> Option<u64> {
    let g = repo.gix();
    let header = |id: gk_core::ObjectId| g.find_header(id).ok().map(|h| h.size());
    let index = |path: &str| {
        let idx = g.index_or_empty().ok()?;
        let e = idx.entry_by_path(path.as_bytes().into())?;
        header(e.id)
    };
    let head = |path: &str| {
        let tree = g.head_commit().ok()?.tree().ok()?;
        let e = tree.lookup_entry_by_path(path).ok()??;
        header(e.object_id())
    };
    let sizes: Vec<Option<u64>> = match target {
        DiffTarget::Unstaged { path } => vec![
            std::fs::metadata(repo.abs(path)).ok().map(|m| m.len()),
            index(path),
        ],
        DiffTarget::Staged { path, old_path } => {
            vec![index(path), head(old_path.as_deref().unwrap_or(path))]
        }
        DiffTarget::Blobs { old, new, .. } => vec![old.and_then(header), new.and_then(header)],
    };
    sizes.into_iter().flatten().max()
}

pub enum Gate {
    /// Within the limit (or allowed): load the diff.
    Open,
    /// Still measuring.
    Pending,
    /// Larger than the limit: the size in bytes.
    TooLarge(u64),
}

type Probe = (DiffTarget, u64, Arc<Mutex<Option<Option<u64>>>>);

#[derive(Default)]
pub struct SizeGuard {
    pending: Option<Probe>,
    known: Option<(DiffTarget, u64, Option<u64>)>,
    /// "Show anyway" was clicked for this target.
    allowed: Option<DiffTarget>,
}

impl SizeGuard {
    pub fn allow(&mut self, target: &DiffTarget) {
        self.allowed = Some(target.clone());
    }

    pub fn check(
        &mut self,
        ctx: &egui::Context,
        repo: &Repo,
        target: &DiffTarget,
        generation: u64,
        limit: u64,
    ) -> Gate {
        if self.allowed.as_ref().is_some_and(|t| t != target) {
            self.allowed = None;
        }
        if self.allowed.as_ref() == Some(target) {
            return Gate::Open;
        }
        let done = self
            .pending
            .as_ref()
            .and_then(|(t, g, slot)| slot.lock().take().map(|size| (t.clone(), *g, size)));
        if let Some(k) = done {
            self.known = Some(k);
            self.pending = None;
        }
        if let Some((t, g, size)) = &self.known
            && t == target
            && *g == generation
        {
            return match size {
                Some(n) if *n > limit => Gate::TooLarge(*n),
                _ => Gate::Open,
            };
        }
        let running = self
            .pending
            .as_ref()
            .is_some_and(|(t, g, _)| t == target && *g == generation);
        if !running {
            let slot: Arc<Mutex<Option<Option<u64>>>> = Arc::default();
            let (s2, repo, t2, c) = (slot.clone(), repo.clone(), target.clone(), ctx.clone());
            let _ = std::thread::Builder::new()
                .name("gk-diff-size".into())
                .spawn(move || {
                    *s2.lock() = Some(probe_size(&repo, &t2));
                    c.request_repaint();
                });
            self.pending = Some((target.clone(), generation, slot));
        }
        Gate::Pending
    }
}

/// A parsed Git LFS pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LfsPointer {
    pub oid: String,
    pub size: Option<u64>,
}

const LFS_VERSION: &str = "version https://git-lfs.github.com/spec/v1";

/// The (old, new) LFS pointers when every line of `d` is pointer content and
/// at least one side names an object; `None` for ordinary files.
pub fn lfs_pointers(d: &FileDiff) -> Option<(Option<LfsPointer>, Option<LfsPointer>)> {
    if d.binary || d.lines.is_empty() || d.lines.len() > 16 {
        return None;
    }
    let mut old = (None::<String>, None::<u64>);
    let mut new = (None::<String>, None::<u64>);
    let mut saw_version = false;
    for l in &d.lines {
        let text = d.line_text(l);
        let t = text.trim();
        let (oid, size) = if t == LFS_VERSION {
            saw_version = true;
            (None, None)
        } else if let Some(o) = t.strip_prefix("oid sha256:") {
            if o.len() != 64 || !o.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            (Some(o.to_string()), None)
        } else if let Some(s) = t.strip_prefix("size ") {
            (None, Some(s.parse::<u64>().ok()?))
        } else if t.starts_with("ext-") || t.is_empty() {
            (None, None)
        } else {
            return None;
        };
        let (to_old, to_new) = match l.kind {
            LineKind::Removed => (true, false),
            LineKind::Added => (false, true),
            LineKind::Context => (true, true),
        };
        for (on, s) in [(to_old, &mut old), (to_new, &mut new)] {
            if !on {
                continue;
            }
            if let Some(o) = &oid {
                s.0 = Some(o.clone());
            }
            if size.is_some() {
                s.1 = size;
            }
        }
    }
    let ptr = |s: (Option<String>, Option<u64>)| s.0.map(|oid| LfsPointer { oid, size: s.1 });
    let (o, n) = (ptr(old), ptr(new));
    // With little context the version line may be missing; an oid is enough then.
    ((o.is_some() || n.is_some()) && (saw_version || d.context < 3)).then_some((o, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ptr(oid: char, size: u64) -> String {
        format!(
            "{LFS_VERSION}\noid sha256:{}\nsize {size}\n",
            oid.to_string().repeat(64)
        )
    }

    #[test]
    fn detects_lfs_pointer_changes() {
        let opts = gk_diff::Options {
            context: 3,
            ignore_whitespace: false,
        };
        let d = gk_diff::diff(ptr('a', 10).as_bytes(), ptr('b', 2048).as_bytes(), opts);
        let (o, n) = lfs_pointers(&d).expect("pointer diff");
        assert_eq!(o.unwrap().size, Some(10));
        let n = n.unwrap();
        assert_eq!(n.size, Some(2048));
        assert!(n.oid.starts_with("bbbb"));
        // Added pointer file.
        let d = gk_diff::diff(b"", ptr('c', 5).as_bytes(), opts);
        let (o, n) = lfs_pointers(&d).unwrap();
        assert!(o.is_none() && n.is_some());
        // Ordinary text is not a pointer.
        let d = gk_diff::diff(b"size 1\n", b"size 2\n", opts);
        assert!(lfs_pointers(&d).is_none());
        let d = gk_diff::diff(b"hello\n", b"oid sha256:xyz\n", opts);
        assert!(lfs_pointers(&d).is_none());
    }

    #[test]
    fn probes_sizes_and_gates() {
        let dir = std::env::temp_dir().join(format!("gk-size-guard-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let git = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(&dir)
                    .output()
                    .unwrap()
                    .status
                    .success()
            )
        };
        git(&["init", "-q"]);
        git(&["config", "user.name", "t"]);
        git(&["config", "user.email", "t@t"]);
        std::fs::write(dir.join("big.txt"), "x".repeat(5000)).unwrap();
        git(&["add", "big.txt"]);
        git(&["commit", "-qm", "init"]);
        std::fs::write(dir.join("big.txt"), "y".repeat(100)).unwrap();
        let repo = Repo::open(&dir, gk_cli::CommandLog::default()).unwrap();
        let un = DiffTarget::Unstaged {
            path: "big.txt".into(),
        };
        assert_eq!(probe_size(&repo, &un), Some(5000), "index side is larger");
        let st = DiffTarget::Staged {
            path: "big.txt".into(),
            old_path: None,
        };
        assert_eq!(probe_size(&repo, &st), Some(5000));

        let ctx = egui::Context::default();
        let mut g = SizeGuard::default();
        let mut gate = Gate::Pending;
        for _ in 0..200 {
            gate = g.check(&ctx, &repo, &un, 1, 4096);
            if !matches!(gate, Gate::Pending) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(matches!(gate, Gate::TooLarge(5000)));
        g.allow(&un);
        assert!(matches!(g.check(&ctx, &repo, &un, 1, 4096), Gate::Open));
        assert!(matches!(
            g.check(&ctx, &repo, &st, 1, 1 << 20),
            Gate::Pending
        ));
        drop(repo);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
