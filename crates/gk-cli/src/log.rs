use crate::Output;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

const MAX_ENTRIES: usize = 500;
const MAX_OUTPUT: usize = 16 * 1024;

/// One executed git command, as shown in the Output panel.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub id: u64,
    pub at: SystemTime,
    pub cwd: String,
    pub command: String,
    pub duration: Duration,
    pub code: Option<i32>,
    /// Truncated combined output.
    pub output: String,
}

#[derive(Default)]
struct Inner {
    next_id: u64,
    entries: VecDeque<LogEntry>,
}

/// Bounded, shared log of executed git commands.
#[derive(Clone, Default)]
pub struct CommandLog(Arc<Mutex<Inner>>);

impl CommandLog {
    pub(crate) fn push(&self, cwd: &Path, args: &[&str], duration: Duration, out: &Output) {
        let mut output = String::new();
        for part in [&out.stdout, &out.stderr] {
            if part.is_empty() || output.len() >= MAX_OUTPUT {
                continue;
            }
            let take = part.len().min(MAX_OUTPUT - output.len());
            output.push_str(&String::from_utf8_lossy(&part[..take]));
            if !output.ends_with('\n') {
                output.push('\n');
            }
        }
        let command = std::iter::once("git")
            .chain(args.iter().copied())
            .map(|a| {
                if a.contains(' ') {
                    format!("\"{a}\"")
                } else {
                    a.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let mut inner = self.0.lock();
        inner.next_id += 1;
        let id = inner.next_id;
        inner.entries.push_back(LogEntry {
            id,
            at: SystemTime::now(),
            cwd: cwd.display().to_string(),
            command,
            duration,
            code: out.code,
            output,
        });
        while inner.entries.len() > MAX_ENTRIES {
            inner.entries.pop_front();
        }
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.0.lock().entries.iter().cloned().collect()
    }

    /// Id of the newest entry, to detect changes cheaply.
    pub fn last_id(&self) -> u64 {
        self.0.lock().next_id
    }

    pub fn clear(&self) {
        self.0.lock().entries.clear();
    }
}
