//! Directory tree of changed files, flattened into display rows, plus the
//! filter matcher. Pure and allocation-light: built once per status change.

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

#[derive(Debug, Default, Clone)]
pub struct Node {
    /// Last path component.
    pub name: String,
    /// Full directory path, no trailing slash (empty for the root).
    pub path: String,
    pub dirs: Vec<usize>,
    /// Caller item ids of the files directly in this directory.
    pub files: Vec<u32>,
    /// All files below this directory, as a range of [`Tree::order`].
    pub range: Range<u32>,
}

#[derive(Debug, Default, Clone)]
pub struct Tree {
    /// `nodes[0]` is the root.
    pub nodes: Vec<Node>,
    /// Item ids in display order (directories first, then files).
    pub order: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow {
    /// `node` is the deepest directory of a compacted chain (`a/b/c`).
    Dir {
        node: u32,
        name: String,
        depth: u16,
        collapsed: bool,
    },
    File {
        item: u32,
        depth: u16,
    },
}

/// Builds the tree of `items` (`(id, repo-relative path)`).
pub fn build(items: &[(u32, &str)]) -> Tree {
    let mut nodes = vec![Node::default()];
    let mut map: HashMap<&str, usize> = HashMap::new();
    for &(id, path) in items {
        let dir = path.rfind('/').map(|i| &path[..i]).unwrap_or("");
        let n = ensure(dir, &mut map, &mut nodes);
        nodes[n].files.push(id);
    }
    for i in 0..nodes.len() {
        let mut dirs = std::mem::take(&mut nodes[i].dirs);
        dirs.sort_by(|a, b| cmp_names(&nodes[*a].name, &nodes[*b].name));
        nodes[i].dirs = dirs;
    }
    let mut order = Vec::with_capacity(items.len());
    assign(&mut nodes, 0, &mut order);
    Tree { nodes, order }
}

fn cmp_names(a: &str, b: &str) -> std::cmp::Ordering {
    a.to_lowercase()
        .cmp(&b.to_lowercase())
        .then_with(|| a.cmp(b))
}

fn ensure<'a>(dir: &'a str, map: &mut HashMap<&'a str, usize>, nodes: &mut Vec<Node>) -> usize {
    if dir.is_empty() {
        return 0;
    }
    if let Some(&i) = map.get(dir) {
        return i;
    }
    let (parent, name) = match dir.rfind('/') {
        Some(i) => (&dir[..i], &dir[i + 1..]),
        None => ("", dir),
    };
    let p = ensure(parent, map, nodes);
    let idx = nodes.len();
    nodes.push(Node {
        name: name.to_string(),
        path: dir.to_string(),
        ..Default::default()
    });
    nodes[p].dirs.push(idx);
    map.insert(dir, idx);
    idx
}

fn assign(nodes: &mut [Node], n: usize, order: &mut Vec<u32>) {
    let start = order.len() as u32;
    let dirs = nodes[n].dirs.clone();
    for d in dirs {
        assign(nodes, d, order);
    }
    order.extend_from_slice(&nodes[n].files);
    nodes[n].range = start..order.len() as u32;
}

/// Display rows. Directories in `collapsed` (by full path) hide their content;
/// chains of directories without files are shown as one row.
pub fn flatten(tree: &Tree, collapsed: &HashSet<String>) -> Vec<TreeRow> {
    let mut out = Vec::new();
    if !tree.nodes.is_empty() {
        walk(tree, 0, 0, collapsed, &mut out);
    }
    out
}

fn walk(tree: &Tree, n: usize, depth: u16, collapsed: &HashSet<String>, out: &mut Vec<TreeRow>) {
    for &d in &tree.nodes[n].dirs {
        let mut node = d;
        let mut name = tree.nodes[d].name.clone();
        while tree.nodes[node].files.is_empty() && tree.nodes[node].dirs.len() == 1 {
            node = tree.nodes[node].dirs[0];
            name.push('/');
            name.push_str(&tree.nodes[node].name);
        }
        let is_collapsed = collapsed.contains(&tree.nodes[node].path);
        out.push(TreeRow::Dir {
            node: node as u32,
            name,
            depth,
            collapsed: is_collapsed,
        });
        if !is_collapsed {
            walk(tree, node, depth + 1, collapsed, out);
        }
    }
    for &f in &tree.nodes[n].files {
        out.push(TreeRow::File { item: f, depth });
    }
}

/// Fuzzy path filter (nucleo-matcher, fzf syntax): whitespace-separated
/// atoms must all match, case-insensitively; `'exact`, `^prefix`,
/// `suffix$` and `!negation` work as in fzf.
pub struct Filter {
    pattern: Option<Pattern>,
    matcher: Matcher,
    buf: Vec<char>,
}

impl Filter {
    pub fn new(query: &str) -> Filter {
        let q = query.trim();
        Filter {
            pattern: (!q.is_empty())
                .then(|| Pattern::parse(q, CaseMatching::Ignore, Normalization::Smart)),
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            buf: Vec::new(),
        }
    }

    pub fn matches(&mut self, path: &str) -> bool {
        match &self.pattern {
            None => true,
            Some(p) => p
                .score(Utf32Str::new(path, &mut self.buf), &mut self.matcher)
                .is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows_text(tree: &Tree, rows: &[TreeRow], paths: &[&str]) -> Vec<String> {
        rows.iter()
            .map(|r| match r {
                TreeRow::Dir {
                    name,
                    depth,
                    collapsed,
                    ..
                } => {
                    format!(
                        "{}{}/{}",
                        "  ".repeat(*depth as usize),
                        name,
                        if *collapsed { "+" } else { "" }
                    )
                }
                TreeRow::File { item, depth } => {
                    let p = paths[*item as usize];
                    let _ = tree;
                    format!(
                        "{}{}",
                        "  ".repeat(*depth as usize),
                        p.rsplit('/').next().unwrap()
                    )
                }
            })
            .collect()
    }

    #[test]
    fn builds_nested_tree_dirs_first() {
        let paths = [
            "README.md",
            "src/a.rs",
            "src/views/b.rs",
            "src/views/c.rs",
            "z/deep/er/x.txt",
        ];
        let items: Vec<(u32, &str)> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| (i as u32, *p))
            .collect();
        let t = build(&items);
        let rows = flatten(&t, &HashSet::new());
        assert_eq!(
            rows_text(&t, &rows, &paths),
            vec![
                "src/",
                "  views/",
                "    b.rs",
                "    c.rs",
                "  a.rs",
                "z/deep/er/",
                "  x.txt",
                "README.md"
            ]
        );
        // Order covers every file once, directory ranges are contiguous.
        assert_eq!(t.order.len(), paths.len());
        let src = t.nodes.iter().find(|n| n.path == "src").unwrap();
        let mut under: Vec<u32> =
            t.order[src.range.start as usize..src.range.end as usize].to_vec();
        under.sort();
        assert_eq!(under, vec![1, 2, 3]);
        assert_eq!(t.nodes[0].range, 0..5);
    }

    #[test]
    fn collapsed_dirs_hide_content() {
        let paths = ["a/x", "a/b/y", "c"];
        let items: Vec<(u32, &str)> = paths
            .iter()
            .enumerate()
            .map(|(i, p)| (i as u32, *p))
            .collect();
        let t = build(&items);
        let collapsed: HashSet<String> = ["a".to_string()].into();
        let rows = flatten(&t, &collapsed);
        assert_eq!(rows_text(&t, &rows, &paths), vec!["a/+", "c"]);
        let collapsed: HashSet<String> = ["a/b".to_string()].into();
        let rows = flatten(&t, &collapsed);
        assert_eq!(
            rows_text(&t, &rows, &paths),
            vec!["a/", "  b/+", "  x", "c"]
        );
    }

    #[test]
    fn compacted_chain_uses_deepest_path() {
        let items = [(0u32, "p/q/r/f")];
        let t = build(&items);
        let rows = flatten(&t, &HashSet::new());
        match &rows[0] {
            TreeRow::Dir { node, name, .. } => {
                assert_eq!(name, "p/q/r");
                assert_eq!(t.nodes[*node as usize].path, "p/q/r");
            }
            r => panic!("unexpected {r:?}"),
        }
    }

    #[test]
    fn fuzzy_filter() {
        let mut f = Filter::new("  VIEWS  rs ");
        assert!(f.matches("src/Views/diff.rs"));
        assert!(!f.matches("src/model.rs"));
        assert!(Filter::new("").matches("anything"));
        // Fuzzy: a subsequence across path components.
        let mut f = Filter::new("fsdiff");
        assert!(f.matches("views/file_status/diff.rs"));
        assert!(!f.matches("views/history.rs"));
        // fzf operators.
        assert!(Filter::new("!test").matches("src/main.rs"));
        assert!(!Filter::new("!test").matches("tests/main.rs"));
        assert!(Filter::new("rs$").matches("a/b.rs"));
        assert!(!Filter::new("rs$").matches("a/rs.txt"));
    }
}
