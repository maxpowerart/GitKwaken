//! Guard against staging conflicted files that still contain conflict
//! markers. Staging a conflicted file marks it resolved, so before doing so
//! the worktree files are scanned (on a background thread) for marker lines;
//! when any are found the user confirms first.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, TryRecvError};

/// Stage request waiting for the marker check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageOp {
    Paths(Vec<String>),
    All,
}

/// True for a conflict marker line: `<<<<<<< ` / `>>>>>>> ` (with a label,
/// or alone), or a separator line that is exactly `=======`. A trailing
/// `\r` is ignored. Longer runs of `=` (Markdown underlines) do not count.
pub fn is_marker_line(line: &[u8]) -> bool {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let fence = |c: u8| {
        line.len() >= 7
            && line[..7].iter().all(|&b| b == c)
            && line.get(7).is_none_or(|&b| b == b' ' || b == b'\t')
    };
    fence(b'<') || fence(b'>') || line == b"======="
}

/// Whether `content` has any conflict marker line.
pub fn has_markers(content: &[u8]) -> bool {
    content.split(|&b| b == b'\n').any(is_marker_line)
}

/// Files (`(repo-relative, absolute)`) that still contain markers. Missing or
/// unreadable files (e.g. deleted on one side) count as clean.
pub fn files_with_markers(files: &[(String, PathBuf)]) -> Vec<String> {
    files
        .iter()
        .filter(|(_, abs)| std::fs::read(abs).is_ok_and(|c| has_markers(&c)))
        .map(|(rel, _)| rel.clone())
        .collect()
}

enum State {
    Checking(Receiver<Vec<String>>),
    /// Confirmation pending: these files still have markers.
    Ask(Vec<String>),
}

struct Job {
    op: StageOp,
    state: State,
}

/// Stage requests whose conflicted files are being checked or confirmed.
#[derive(Default)]
pub struct Guard {
    jobs: Vec<Job>,
}

/// The user's answer to the confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    StageAnyway,
    OpenMergeEditor,
    Cancel,
}

impl Guard {
    /// Scans `files` in the background, then repaints `ctx`.
    pub fn check(&mut self, op: StageOp, files: Vec<(String, PathBuf)>, ctx: &egui::Context) {
        let (tx, rx) = mpsc::channel();
        let (tx2, ctx) = (tx.clone(), ctx.clone());
        let inline = files.clone();
        let spawned = std::thread::Builder::new()
            .name("gk-conflict-check".into())
            .spawn(move || {
                let _ = tx.send(files_with_markers(&files));
                ctx.request_repaint();
            });
        if spawned.is_err() {
            // Out of threads: scan synchronously instead.
            let _ = tx2.send(files_with_markers(&inline));
        }
        self.push(op, rx);
    }

    fn push(&mut self, op: StageOp, rx: Receiver<Vec<String>>) {
        self.jobs.push(Job {
            op,
            state: State::Checking(rx),
        });
    }

    pub fn is_busy(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Collects finished checks. Returns the operations that may run right
    /// away (no markers left); the others wait for [`Self::answer`].
    pub fn poll(&mut self) -> Vec<StageOp> {
        let mut ready = Vec::new();
        self.jobs.retain_mut(|job| {
            let State::Checking(rx) = &job.state else {
                return true;
            };
            match rx.try_recv() {
                Ok(marked) if marked.is_empty() => {
                    ready.push(job.op.clone());
                    false
                }
                Ok(marked) => {
                    job.state = State::Ask(marked);
                    true
                }
                Err(TryRecvError::Empty) => true,
                // The checker died: let the user decide.
                Err(TryRecvError::Disconnected) => {
                    job.state = State::Ask(Vec::new());
                    true
                }
            }
        });
        ready
    }

    /// Files of the first request awaiting confirmation.
    pub fn asking(&self) -> Option<&[String]> {
        self.jobs.iter().find_map(|j| match &j.state {
            State::Ask(m) => Some(m.as_slice()),
            State::Checking(_) => None,
        })
    }

    /// Settles the request shown by [`Self::asking`]. Returns the operation
    /// to run (for [`Answer::StageAnyway`]) and the file to open in the merge
    /// editor (for [`Answer::OpenMergeEditor`]).
    pub fn answer(&mut self, a: Answer) -> (Option<StageOp>, Option<String>) {
        let Some(i) = self
            .jobs
            .iter()
            .position(|j| matches!(j.state, State::Ask(_)))
        else {
            return (None, None);
        };
        let job = self.jobs.remove(i);
        let State::Ask(marked) = job.state else {
            unreachable!("position matched Ask")
        };
        match a {
            Answer::StageAnyway => (Some(job.op), None),
            Answer::OpenMergeEditor => (None, marked.into_iter().next()),
            Answer::Cancel => (None, None),
        }
    }
}

/// Confirmation text for `marked` files.
pub fn question(marked: &[String]) -> String {
    match marked {
        [] => "Could not check the files for conflict markers. Stage anyway?".to_string(),
        [one] => format!("{one} still contains conflict markers. Stage anyway?"),
        many => {
            const SHOW: usize = 5;
            let mut list = many[..many.len().min(SHOW)].join(", ");
            if many.len() > SHOW {
                list.push_str(&format!(" and {} more", many.len() - SHOW));
            }
            format!(
                "{} files still contain conflict markers ({list}). Stage anyway?",
                many.len()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_marker_lines() {
        assert!(is_marker_line(b"<<<<<<< HEAD"));
        assert!(is_marker_line(b">>>>>>> feature\r"));
        assert!(is_marker_line(b"======="));
        assert!(is_marker_line(b"=======\r"));
        assert!(is_marker_line(b"<<<<<<<"));
        assert!(!is_marker_line(b"========"), "markdown underline");
        assert!(!is_marker_line(b"<<<<<<<< x"), "eight fences");
        assert!(!is_marker_line(b" <<<<<<< HEAD"), "indented");
        assert!(!is_marker_line(b"<<<<<<<HEAD"));
        assert!(!is_marker_line(b"a <<<<<<< b"));
        assert!(!is_marker_line(b""));
    }

    #[test]
    fn detects_markers_in_content() {
        let conflicted = b"fn a() {}\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> topic\n";
        assert!(has_markers(conflicted));
        assert!(has_markers(b"x\r\n=======\r\ny"));
        assert!(!has_markers(b"fn a() {}\nlet s = \"=======\";\n"));
        assert!(!has_markers(b"Title\n========\n"));
        assert!(!has_markers(b""));
        // Binary content is scanned bytewise without panicking.
        assert!(!has_markers(&[0u8, 159, 146, 150, b'\n', 0xff]));
    }

    #[test]
    fn scans_worktree_files() {
        let dir = std::env::temp_dir().join(format!("gk-markers-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("bad.rs"),
            "<<<<<<< ours\na\n=======\nb\n>>>>>>> theirs\n",
        )
        .unwrap();
        std::fs::write(dir.join("ok.rs"), "resolved\n").unwrap();
        let files = vec![
            ("bad.rs".to_string(), dir.join("bad.rs")),
            ("ok.rs".to_string(), dir.join("ok.rs")),
            ("gone.rs".to_string(), dir.join("gone.rs")),
        ];
        assert_eq!(files_with_markers(&files), vec!["bad.rs".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn guard_runs_clean_ops_and_asks_for_marked_ones() {
        let mut g = Guard::default();
        let (tx1, rx1) = mpsc::channel();
        let (tx2, rx2) = mpsc::channel();
        g.push(StageOp::Paths(vec!["a".into()]), rx1);
        g.push(StageOp::All, rx2);
        assert!(g.poll().is_empty());
        assert!(g.asking().is_none());
        tx1.send(Vec::new()).unwrap();
        tx2.send(vec!["src/lib.rs".to_string()]).unwrap();
        assert_eq!(g.poll(), vec![StageOp::Paths(vec!["a".into()])]);
        assert_eq!(g.asking(), Some(&["src/lib.rs".to_string()][..]));
        assert_eq!(
            g.answer(Answer::OpenMergeEditor),
            (None, Some("src/lib.rs".to_string()))
        );
        assert!(!g.is_busy());

        let (tx, rx) = mpsc::channel();
        g.push(StageOp::All, rx);
        tx.send(vec!["x".to_string()]).unwrap();
        g.poll();
        assert_eq!(g.answer(Answer::StageAnyway), (Some(StageOp::All), None));
        let (tx, rx) = mpsc::channel();
        g.push(StageOp::All, rx);
        tx.send(vec!["x".to_string()]).unwrap();
        g.poll();
        assert_eq!(g.answer(Answer::Cancel), (None, None));
        assert_eq!(g.answer(Answer::Cancel), (None, None), "nothing left");
    }

    #[test]
    fn question_text() {
        assert_eq!(
            question(&["src/lib.rs".to_string()]),
            "src/lib.rs still contains conflict markers. Stage anyway?"
        );
        let many: Vec<String> = (0..7).map(|i| format!("f{i}")).collect();
        assert!(question(&many).starts_with("7 files still contain"));
        assert!(question(&many).contains("and 2 more"));
    }
}
