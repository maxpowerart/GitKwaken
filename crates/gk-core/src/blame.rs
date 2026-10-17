//! `git blame`: the blocking `--porcelain` form ([`Repo::blame`]) and the
//! streaming `--incremental` form ([`Repo::blame_streaming`]) that fills a
//! shared [`BlameFeed`] as git finds each commit, so a view can show the lines
//! that are already known while the rest is still being computed.

use crate::Repo;
use anyhow::{Context, Result};
use gk_cli::{CancelToken, Line};
use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Commit id git reports for lines that are not committed yet.
pub const UNCOMMITTED_ID: &str = "0000000000000000000000000000000000000000";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameCommit {
    pub id: String,
    pub author: String,
    pub author_time: i64,
    pub summary: String,
    /// The commit's parent and the path there, for "blame previous revision".
    pub previous: Option<(String, String)>,
    /// The commit is a boundary of the blamed range (a root commit, or the
    /// limit of a `rev..` range).
    pub boundary: bool,
}

impl BlameCommit {
    fn new(id: &str) -> Self {
        Self {
            id: id.to_string(),
            author: String::new(),
            author_time: 0,
            summary: String::new(),
            previous: None,
            boundary: false,
        }
    }

    /// Applies one `key value` header line of porcelain / incremental output.
    fn apply_field(&mut self, key: &str, value: &str) {
        match key {
            "author" => self.author = value.to_string(),
            "author-time" => self.author_time = value.parse().unwrap_or(0),
            "summary" => self.summary = value.to_string(),
            "boundary" => self.boundary = true,
            "previous" => {
                if let Some((id, path)) = value.split_once(' ') {
                    self.previous = Some((id.to_string(), unquote_c(path)));
                }
            }
            _ => {}
        }
    }
}

/// One line of a blamed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameLine {
    /// Index into [`Blame::commits`].
    pub commit: usize,
    /// 1-based line number in the commit that introduced it.
    pub orig_line: u32,
}

#[derive(Debug, Clone, Default)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    pub lines: Vec<BlameLine>,
    pub text: Vec<String>,
}

/// `<sha> <orig> <final> [<count>]`: the first line of a porcelain or
/// incremental record. SHA-1 and SHA-256 ids.
fn parse_header(line: &str) -> Option<(&str, u32, u32, Option<u32>)> {
    let mut parts = line.split(' ');
    let id = parts.next()?;
    if !(id.len() == 40 || id.len() == 64) || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let orig = parts.next()?.parse().ok()?;
    let fin = parts.next()?.parse().ok()?;
    let count = parts.next().and_then(|n| n.parse().ok());
    Some((id, orig, fin, count))
}

/// Parses `git blame --porcelain` output.
pub fn parse_blame_porcelain(data: &str) -> Blame {
    let mut blame = Blame::default();
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut current: Option<usize> = None;
    let mut orig = 0u32;
    for line in data.lines() {
        if let Some(text) = line.strip_prefix('\t') {
            if let Some(c) = current {
                blame.lines.push(BlameLine {
                    commit: c,
                    orig_line: orig,
                });
                blame.text.push(text.to_string());
            }
            continue;
        }
        if let Some((id, o, _, _)) = parse_header(line) {
            orig = o;
            let idx = *index.entry(id.to_string()).or_insert_with(|| {
                blame.commits.push(BlameCommit::new(id));
                blame.commits.len() - 1
            });
            current = Some(idx);
            continue;
        }
        let Some(c) = current else { continue };
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        blame.commits[c].apply_field(key, rest);
    }
    blame
}

// ------------------------------------------------------------- incremental

/// A run of consecutive lines of the blamed file attributed to one commit, as
/// one `git blame --incremental` record reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlameGroup {
    /// Index into the parser's (or feed's) commit list.
    pub commit: usize,
    /// 1-based first line in the commit that introduced the lines.
    pub orig_start: u32,
    /// 1-based first line in the blamed file.
    pub final_start: u32,
    pub count: u32,
}

/// Incremental parser for `git blame --incremental`. Each record is a header
/// `<sha> <orig> <final> <count>`, the commit's details (`author`, `summary`,
/// `boundary`, ... only the first time a commit appears), an optional
/// `previous <sha> <path>`, and a closing `filename <path>` line.
#[derive(Debug, Default)]
pub struct IncrementalParser {
    index: HashMap<String, usize>,
    commits: Vec<BlameCommit>,
    /// Record being read: (commit, orig, final, count).
    current: Option<(usize, u32, u32, u32)>,
}

impl IncrementalParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Commits seen so far, in order of first appearance.
    pub fn commits(&self) -> &[BlameCommit] {
        &self.commits
    }

    /// Feeds one output line (without its line terminator). Returns the
    /// record's group when its closing `filename` line arrives.
    pub fn push_line(&mut self, line: &str) -> Option<BlameGroup> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some((id, orig, fin, count)) = parse_header(line) {
            let commit = match self.index.get(id) {
                Some(&c) => c,
                None => {
                    self.commits.push(BlameCommit::new(id));
                    self.index.insert(id.to_string(), self.commits.len() - 1);
                    self.commits.len() - 1
                }
            };
            self.current = Some((commit, orig, fin, count.unwrap_or(1)));
            return None;
        }
        let (commit, orig, fin, count) = self.current?;
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        if key == "filename" {
            self.current = None;
            return Some(BlameGroup {
                commit,
                orig_start: orig,
                final_start: fin,
                count,
            });
        }
        self.commits[commit].apply_field(key, rest);
        None
    }

    /// Parses complete output at once (tests, small files).
    pub fn parse_all(data: &str) -> (Vec<BlameCommit>, Vec<BlameGroup>) {
        let mut p = Self::new();
        let groups = data.lines().filter_map(|l| p.push_line(l)).collect();
        (p.commits, groups)
    }
}

/// Reverses git's C-style path quoting (`"a\tb\303\244"`); other text is
/// returned unchanged.
pub fn unquote_c(s: &str) -> String {
    let Some(inner) = s.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else {
        return s.to_string();
    };
    let b = inner.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] != b'\\' || i + 1 == b.len() {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let e = b[i + 1];
        i += 2;
        out.push(match e {
            b'a' => 7,
            b'b' => 8,
            b't' => b'\t',
            b'n' => b'\n',
            b'v' => 11,
            b'f' => 12,
            b'r' => b'\r',
            b'0'..=b'7' => {
                let mut v = u32::from(e - b'0');
                for _ in 0..2 {
                    if i < b.len() && (b'0'..=b'7').contains(&b[i]) {
                        v = v * 8 + u32::from(b[i] - b'0');
                        i += 1;
                    }
                }
                v as u8
            }
            other => other,
        });
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Splits file content into display lines the way git counts them.
pub fn split_lines(data: &[u8]) -> Vec<String> {
    let s = String::from_utf8_lossy(data);
    s.lines().map(str::to_string).collect()
}

/// Blame state shared between the thread running [`Repo::blame_streaming`]
/// and the view that polls it. Everything only grows: a reader remembers how
/// many commits and groups it has consumed and takes the rest.
#[derive(Debug, Default)]
pub struct BlameFeed {
    /// File content, one entry per line. Set before the first group.
    pub text: Option<Arc<Vec<String>>>,
    pub commits: Vec<BlameCommit>,
    pub groups: Vec<BlameGroup>,
    /// Lines covered by `groups`.
    pub lines_done: usize,
    /// git finished (successfully or not).
    pub done: bool,
    pub error: Option<String>,
}

pub type SharedBlameFeed = Arc<Mutex<BlameFeed>>;

impl BlameFeed {
    /// The complete blame, or `None` while some line is not attributed.
    pub fn to_blame(&self) -> Option<Blame> {
        let text = self.text.as_ref()?;
        let mut lines: Vec<Option<BlameLine>> = vec![None; text.len()];
        for g in &self.groups {
            for k in 0..g.count {
                let at = (g.final_start + k) as usize - 1;
                if at >= lines.len() {
                    lines.resize(at + 1, None);
                }
                lines[at] = Some(BlameLine {
                    commit: g.commit,
                    orig_line: g.orig_start + k,
                });
            }
        }
        let lines = lines.into_iter().collect::<Option<Vec<_>>>()?;
        let mut text = text.as_ref().clone();
        text.resize(lines.len(), String::new());
        Some(Blame {
            commits: self.commits.clone(),
            lines,
            text,
        })
    }
}

/// Minimum time between two repaint notifications while groups stream in.
const NOTIFY_EVERY: Duration = Duration::from_millis(50);

impl Repo {
    /// Blocking blame (`git blame --porcelain -w`).
    pub fn blame(&self, path: &str, rev: Option<&str>) -> Result<Blame> {
        let mut args = vec!["blame", "--porcelain", "-w"];
        if let Some(r) = rev {
            args.push(r);
        }
        args.extend(["--", path]);
        let out = self.git().run(&args)?;
        Ok(parse_blame_porcelain(&out.stdout_str()))
    }

    /// Content of `path` at `rev` (`None`: the working copy) as display lines.
    pub fn blame_text(&self, path: &str, rev: Option<&str>) -> Result<Vec<String>> {
        let data = match rev {
            None => std::fs::read(self.workdir().join(path))
                .with_context(|| format!("cannot read {path}"))?,
            Some(r) => {
                let spec = format!("{r}:{path}");
                self.git().run(&["cat-file", "blob", &spec])?.stdout
            }
        };
        Ok(split_lines(&data))
    }

    /// Streaming blame (`git blame --incremental -w`): publishes the file text,
    /// then every record as git emits it, into `feed`. Sets `feed.done` (and
    /// `feed.error` on failure) when finished. `notify` is called after the
    /// text, for the first group, at most every 50 ms after that, and at the end.
    /// Stops (killing git) when `cancel` fires.
    pub fn blame_streaming(
        &self,
        path: &str,
        rev: Option<&str>,
        cancel: &CancelToken,
        feed: &SharedBlameFeed,
        notify: &(dyn Fn() + Send + Sync),
    ) -> Result<()> {
        let r = self.blame_streaming_inner(path, rev, cancel, feed, notify);
        {
            let mut f = feed.lock();
            f.done = true;
            if let Err(e) = &r {
                f.error = Some(format!("{e:#}"));
            }
        }
        notify();
        r
    }

    fn blame_streaming_inner(
        &self,
        path: &str,
        rev: Option<&str>,
        cancel: &CancelToken,
        feed: &SharedBlameFeed,
        notify: &(dyn Fn() + Send + Sync),
    ) -> Result<()> {
        let mut args = vec!["blame", "--incremental", "-w"];
        if let Some(r) = rev {
            args.push(r);
        }
        args.extend(["--", path]);
        // Text first: it is cheap, and lets the view lay out the whole file
        // with placeholder gutters before the first record. A failure here is
        // reported after git had its say (its message is the better one).
        let text = self.blame_text(path, rev);
        if let Ok(t) = &text {
            feed.lock().text = Some(Arc::new(t.clone()));
            notify();
        }
        let mut parser = IncrementalParser::new();
        let mut last_notify: Option<Instant> = None;
        let out = self.git().run_streaming(&args, None, cancel, |line| {
            let Line::Stdout(l) = line else { return };
            let Some(g) = parser.push_line(&l) else {
                return;
            };
            {
                let mut f = feed.lock();
                let known = f.commits.len();
                f.commits.extend_from_slice(&parser.commits()[known..]);
                f.groups.push(g);
                f.lines_done += g.count as usize;
            }
            if last_notify.is_none_or(|t| t.elapsed() >= NOTIFY_EVERY) {
                last_notify = Some(Instant::now());
                notify();
            }
        })?;
        out.check()?;
        text.map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blame_porcelain() {
        let data = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 1 1 2\nauthor Alice\nauthor-time 100\nsummary first\nfilename f\n\thello\naaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 2 2\n\tworld\n";
        let b = parse_blame_porcelain(data);
        assert_eq!(b.commits.len(), 1);
        assert_eq!(b.commits[0].author, "Alice");
        assert_eq!(b.text, ["hello", "world"]);
        assert_eq!(b.lines[1].orig_line, 2);
    }

    /// Real `--incremental` shape: details only on a commit's first record,
    /// `previous` and `filename` on every one, `boundary` for root commits.
    #[test]
    fn incremental_records() {
        let a = "a".repeat(40);
        let b = "b".repeat(40);
        let data = format!(
            "{b} 3 3 2\n\
             author Bob\nauthor-mail <b@x>\nauthor-time 200\nauthor-tz +0100\n\
             committer Bob\ncommitter-mail <b@x>\ncommitter-time 200\ncommitter-tz +0100\n\
             summary second change\n\
             previous {a} \"old dir/n\\303\\244me.txt\"\n\
             filename new.txt\n\
             {a} 1 1 2\n\
             author Alice\nauthor-mail <a@x>\nauthor-time 100\nauthor-tz +0000\n\
             committer Alice\ncommitter-mail <a@x>\ncommitter-time 100\ncommitter-tz +0000\n\
             summary filename lookalike: {a} 1 2 3\n\
             boundary\n\
             filename \"old dir/n\\303\\244me.txt\"\n\
             {b} 7 5 1\n\
             previous {a} \"old dir/n\\303\\244me.txt\"\n\
             filename new.txt\n"
        );
        let (commits, groups) = IncrementalParser::parse_all(&data);
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].id, b);
        assert_eq!(commits[0].author, "Bob");
        assert_eq!(commits[0].author_time, 200);
        assert_eq!(commits[0].summary, "second change");
        assert!(!commits[0].boundary);
        assert_eq!(
            commits[0].previous,
            Some((a.clone(), "old dir/näme.txt".to_string()))
        );
        assert_eq!(commits[1].author, "Alice");
        assert_eq!(commits[1].summary, format!("filename lookalike: {a} 1 2 3"));
        assert!(commits[1].boundary);
        assert_eq!(commits[1].previous, None);
        assert_eq!(
            groups,
            [
                BlameGroup {
                    commit: 0,
                    orig_start: 3,
                    final_start: 3,
                    count: 2
                },
                BlameGroup {
                    commit: 1,
                    orig_start: 1,
                    final_start: 1,
                    count: 2
                },
                BlameGroup {
                    commit: 0,
                    orig_start: 7,
                    final_start: 5,
                    count: 1
                },
            ]
        );

        let feed = BlameFeed {
            text: Some(Arc::new(
                ["1", "2", "3", "4", "5"].map(String::from).to_vec(),
            )),
            commits,
            groups: groups.clone(),
            lines_done: 5,
            done: true,
            error: None,
        };
        let blame = feed.to_blame().unwrap();
        let per_line: Vec<_> = blame
            .lines
            .iter()
            .map(|l| (l.commit, l.orig_line))
            .collect();
        assert_eq!(per_line, [(1, 1), (1, 2), (0, 3), (0, 4), (0, 7)]);

        // A missing group leaves a hole.
        let partial = BlameFeed {
            groups: groups[..2].to_vec(),
            ..feed
        };
        assert!(partial.to_blame().is_none());
    }

    /// Lines can arrive split at any point; the parser only completes a group
    /// on `filename`, and ignores stray lines outside a record.
    #[test]
    fn incremental_line_by_line() {
        let a = "c".repeat(64); // SHA-256 repository
        let mut p = IncrementalParser::new();
        assert_eq!(p.push_line("filename stray"), None);
        assert_eq!(p.push_line(&format!("{a} 4 9 3\r")), None);
        assert_eq!(p.push_line("author Zed"), None);
        assert_eq!(p.push_line("summary"), None);
        let g = p.push_line("filename x.rs").unwrap();
        assert_eq!(
            (g.commit, g.orig_start, g.final_start, g.count),
            (0, 4, 9, 3)
        );
        assert_eq!(p.commits()[0].author, "Zed");
        assert_eq!(p.commits()[0].summary, "");
        assert_eq!(p.push_line(""), None);
    }

    #[test]
    fn c_unquote() {
        assert_eq!(unquote_c("plain.txt"), "plain.txt");
        assert_eq!(unquote_c("\"a\\tb\\\"c\\\\d\""), "a\tb\"c\\d");
        assert_eq!(unquote_c("\"\\303\\244\""), "ä");
    }

    #[test]
    fn text_lines() {
        assert_eq!(split_lines(b"a\r\nb\n"), ["a", "b"]);
        assert_eq!(split_lines(b"a\n\nb"), ["a", "", "b"]);
        assert!(split_lines(b"").is_empty());
    }
}
