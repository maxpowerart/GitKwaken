//! Pure helpers of the sidebar: '/'-trees of ref names, version-aware
//! ordering of tags, and parsers for `git submodule status` and
//! `git worktree list --porcelain`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Names grouped by '/' into folders. Leaves are indices into the caller's list.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Tree {
    /// Sorted case-insensitively.
    pub folders: Vec<(String, Tree)>,
    /// Sorted case-insensitively by name.
    pub leaves: Vec<u32>,
}

#[derive(Default)]
struct Node<'a> {
    folders: BTreeMap<&'a str, Node<'a>>,
    leaves: Vec<(&'a str, u32)>,
}

impl Node<'_> {
    fn finish(self) -> Tree {
        let mut folders: Vec<(String, Tree)> = self
            .folders
            .into_iter()
            .map(|(k, n)| (k.to_string(), n.finish()))
            .collect();
        folders.sort_by(|a, b| cmp_ci(&a.0, &b.0));
        let mut leaves = self.leaves;
        leaves.sort_by(|a, b| cmp_ci(a.0, b.0));
        Tree {
            folders,
            leaves: leaves.into_iter().map(|(_, i)| i).collect(),
        }
    }
}

fn cmp_ci(a: &str, b: &str) -> Ordering {
    let l = a.chars().flat_map(char::to_lowercase);
    let r = b.chars().flat_map(char::to_lowercase);
    l.cmp(r).then_with(|| a.cmp(b))
}

/// Builds a folder tree from `(index, name)` pairs, splitting names on '/'.
pub fn build_tree<'a>(items: impl IntoIterator<Item = (u32, &'a str)>) -> Tree {
    let mut root = Node::default();
    for (i, name) in items {
        let mut node = &mut root;
        let mut parts = name.split('/').filter(|p| !p.is_empty()).peekable();
        let mut leaf = name;
        while let Some(p) = parts.next() {
            if parts.peek().is_none() {
                leaf = p;
            } else {
                node = node.folders.entry(p).or_default();
            }
        }
        node.leaves.push((leaf, i));
    }
    root.finish()
}

impl Tree {
    #[cfg(test)]
    pub fn leaf_count(&self) -> usize {
        self.leaves.len()
            + self
                .folders
                .iter()
                .map(|(_, t)| t.leaf_count())
                .sum::<usize>()
    }
}

/// Last path segment of a ref name.
pub fn leaf_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Case-insensitive substring match; `needle` must already be lowercase.
pub fn matches(hay: &str, needle_lower: &str) -> bool {
    needle_lower.is_empty() || hay.to_lowercase().contains(needle_lower)
}

// ------------------------------------------------------------ version sort

enum Chunk<'a> {
    Num(&'a str),
    Text(&'a str),
}

fn chunks(s: &str) -> impl Iterator<Item = Chunk<'_>> {
    let b = s.as_bytes();
    let mut i = 0;
    std::iter::from_fn(move || {
        if i >= b.len() {
            return None;
        }
        let start = i;
        let digit = b[i].is_ascii_digit();
        while i < b.len() && b[i].is_ascii_digit() == digit {
            i += 1;
        }
        let part = &s[start..i];
        Some(if digit {
            Chunk::Num(part)
        } else {
            Chunk::Text(part)
        })
    })
}

fn cmp_num(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Pre-release suffixes sort before the release they belong to.
fn is_prerelease(c: &Chunk) -> bool {
    matches!(c, Chunk::Text(t) if t.starts_with('-') || t.starts_with('~'))
}

/// Natural, version-aware ordering: digit runs compare numerically, text
/// case-insensitively, and `1.0.0-rc1 < 1.0.0 < 1.0.1 < 1.0.10`.
pub fn version_cmp(a: &str, b: &str) -> Ordering {
    let mut x = chunks(a);
    let mut y = chunks(b);
    loop {
        match (x.next(), y.next()) {
            (None, None) => break,
            (None, Some(c)) => {
                return if is_prerelease(&c) {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (Some(c), None) => {
                return if is_prerelease(&c) {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
            (Some(Chunk::Num(p)), Some(Chunk::Num(q))) => match cmp_num(p, q) {
                Ordering::Equal => {}
                o => return o,
            },
            (Some(Chunk::Text(p)), Some(Chunk::Text(q))) => match cmp_ci(p, q) {
                Ordering::Equal => {}
                o => return o,
            },
            // Numbers sort before text at the same position ("1.2" < "1.x").
            (Some(Chunk::Num(_)), Some(Chunk::Text(q))) => {
                return if q.starts_with('-') || q.starts_with('~') {
                    Ordering::Greater
                } else {
                    Ordering::Less
                };
            }
            (Some(Chunk::Text(p)), Some(Chunk::Num(_))) => {
                return if p.starts_with('-') || p.starts_with('~') {
                    Ordering::Less
                } else {
                    Ordering::Greater
                };
            }
        }
    }
    a.cmp(b)
}

// ------------------------------------------------------------- submodules

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubState {
    UpToDate,
    NotInitialized,
    /// Checked-out commit differs from the one recorded in the index.
    Modified,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Submodule {
    /// Relative to the repository root ('/'-separated).
    pub path: String,
    pub id: String,
    pub state: SubState,
    pub describe: Option<String>,
}

/// Parses `git submodule status [--recursive]`.
pub fn parse_submodules(out: &str) -> Vec<Submodule> {
    let mut v = Vec::new();
    for line in out.lines() {
        let mut ch = line.chars();
        let Some(flag) = ch.next() else { continue };
        let state = match flag {
            ' ' => SubState::UpToDate,
            '-' => SubState::NotInitialized,
            '+' => SubState::Modified,
            'U' => SubState::Conflict,
            _ => continue,
        };
        let rest = ch.as_str();
        let Some((id, rest)) = rest.split_once(' ') else {
            continue;
        };
        let (path, describe) = match rest.strip_suffix(')').and_then(|r| r.rsplit_once(" (")) {
            Some((p, d)) => (p, Some(d.to_string())),
            None => (rest, None),
        };
        if path.is_empty() {
            continue;
        }
        v.push(Submodule {
            path: path.to_string(),
            id: id.to_string(),
            state,
            describe,
        });
    }
    v
}

// -------------------------------------------------------------- worktrees

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub head: Option<String>,
    /// Short branch name.
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// Parses `git worktree list --porcelain`. The first entry is the main worktree.
pub fn parse_worktrees(out: &str) -> Vec<Worktree> {
    let mut v: Vec<Worktree> = Vec::new();
    let mut cur: Option<Worktree> = None;
    for line in out.lines() {
        if line.is_empty() {
            v.extend(cur.take());
            continue;
        }
        let (key, val) = line.split_once(' ').unwrap_or((line, ""));
        if key == "worktree" {
            v.extend(cur.take());
            cur = Some(Worktree {
                path: PathBuf::from(val),
                ..Default::default()
            });
            continue;
        }
        let Some(w) = cur.as_mut() else { continue };
        match key {
            "HEAD" => w.head = Some(val.to_string()),
            "branch" => w.branch = Some(val.strip_prefix("refs/heads/").unwrap_or(val).to_string()),
            "detached" => w.detached = true,
            "bare" => w.bare = true,
            "locked" => w.locked = true,
            "prunable" => w.prunable = true,
            _ => {}
        }
    }
    v.extend(cur);
    v
}

/// Normalized form for comparing paths reported by git with ours.
pub fn path_key(p: &std::path::Path) -> String {
    p.to_string_lossy()
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(t: &Tree, all: &[&str]) -> Vec<String> {
        t.leaves
            .iter()
            .map(|&i| all[i as usize].to_string())
            .collect()
    }

    #[test]
    fn tree_grouping() {
        let all = [
            "main",
            "feature/b",
            "feature/A",
            "fix/x/deep",
            "Develop",
            "feature/sub/c",
        ];
        let t = build_tree(all.iter().enumerate().map(|(i, n)| (i as u32, *n)));
        assert_eq!(names(&t, &all), ["Develop", "main"]);
        let folders: Vec<&str> = t.folders.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(folders, ["feature", "fix"]);
        let feature = &t.folders[0].1;
        assert_eq!(names(feature, &all), ["feature/A", "feature/b"]);
        assert_eq!(feature.folders[0].0, "sub");
        assert_eq!(names(&feature.folders[0].1, &all), ["feature/sub/c"]);
        assert_eq!(t.folders[1].1.folders[0].0, "x");
        assert_eq!(t.leaf_count(), all.len());
    }

    #[test]
    fn tree_empty_and_leaf() {
        assert_eq!(build_tree(std::iter::empty()), Tree::default());
        assert_eq!(leaf_name("a/b/c"), "c");
        assert_eq!(leaf_name("main"), "main");
        assert!(matches("Feature/X", "feat"));
        assert!(!matches("main", "dev"));
    }

    #[test]
    fn version_order() {
        let mut v = vec![
            "v1.10.0",
            "v1.2.0",
            "v1.2.0-rc1",
            "v1.9",
            "v2.0.0",
            "v1.2.0-rc10",
            "v1.2.0-rc2",
            "release",
            "v1.02.1",
        ];
        v.sort_by(|a, b| version_cmp(a, b));
        assert_eq!(
            v,
            [
                "release",
                "v1.2.0-rc1",
                "v1.2.0-rc2",
                "v1.2.0-rc10",
                "v1.2.0",
                "v1.02.1",
                "v1.9",
                "v1.10.0",
                "v2.0.0"
            ]
        );
        assert_eq!(version_cmp("1.0", "1.0"), Ordering::Equal);
        assert_eq!(version_cmp("1.0", "1.0.1"), Ordering::Less);
        assert_eq!(version_cmp("V1", "v1"), Ordering::Less); // tie broken bytewise
        assert_eq!(
            version_cmp(
                "build-99999999999999999999999",
                "build-100000000000000000000000"
            ),
            Ordering::Less
        );
    }

    #[test]
    fn submodule_status() {
        let out = " 1234567890abcdef1234567890abcdef12345678 libs/foo (v1.2.3)\n\
                   -abcdefabcdefabcdefabcdefabcdefabcdefabcd vendor/bar\n\
                   +1111111111111111111111111111111111111111 path with space (heads/main)\n\
                   U2222222222222222222222222222222222222222 conflicted\n";
        let s = parse_submodules(out);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].path, "libs/foo");
        assert_eq!(s[0].state, SubState::UpToDate);
        assert_eq!(s[0].describe.as_deref(), Some("v1.2.3"));
        assert_eq!(s[1].state, SubState::NotInitialized);
        assert_eq!(s[1].path, "vendor/bar");
        assert_eq!(s[1].describe, None);
        assert_eq!(s[2].path, "path with space");
        assert_eq!(s[2].state, SubState::Modified);
        assert_eq!(s[3].state, SubState::Conflict);
        assert!(parse_submodules("").is_empty());
    }

    #[test]
    fn worktree_porcelain() {
        let out = "worktree D:/repo\nHEAD 1111\nbranch refs/heads/main\n\n\
                   worktree D:/repo-wt\nHEAD 2222\ndetached\nlocked reason here\n\n\
                   worktree D:/bare.git\nbare\n\n\
                   worktree D:/gone\nHEAD 3333\nbranch refs/heads/feature/x\nprunable gitdir file points to non-existent location\n";
        let w = parse_worktrees(out);
        assert_eq!(w.len(), 4);
        assert_eq!(w[0].path, PathBuf::from("D:/repo"));
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert_eq!(w[0].head.as_deref(), Some("1111"));
        assert!(w[1].detached && w[1].locked && w[1].branch.is_none());
        assert!(w[2].bare);
        assert_eq!(w[3].branch.as_deref(), Some("feature/x"));
        assert!(w[3].prunable);
        assert!(parse_worktrees("").is_empty());
        assert_eq!(path_key(std::path::Path::new("D:\\Repo\\")), "d:/repo");
    }
}
