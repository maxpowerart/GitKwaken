//! History filters (message text, author, date range, path, pickaxe) as
//! `git log --format=%H` arguments. The History view streams the ids on a
//! background thread (`gk-app` `views/history/filter.rs`).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pickaxe {
    /// `-S<text>`: commits changing the number of occurrences of a string.
    #[default]
    String,
    /// `-G<regex>`: commits whose diff has added/removed lines matching.
    Regex,
}

/// What the user typed into the filter fields.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Filter {
    /// Commit message (`--grep`, case-insensitive).
    pub text: String,
    pub author: String,
    /// `--since` / `--until`, any date git understands ("2026-01-01").
    pub since: String,
    pub until: String,
    /// Limits to commits touching this path (or folder).
    pub path: String,
    pub pickaxe: String,
    pub pickaxe_mode: Pickaxe,
}

impl Filter {
    pub fn is_active(&self) -> bool {
        self.count() > 0
    }

    /// Number of non-empty fields.
    pub fn count(&self) -> usize {
        [
            &self.text,
            &self.author,
            &self.since,
            &self.until,
            &self.path,
            &self.pickaxe,
        ]
        .iter()
        .filter(|s| !s.trim().is_empty())
        .count()
    }

    /// Fields besides the message text (shown in the filter row).
    pub fn extra_count(&self) -> usize {
        self.count() - usize::from(!self.text.trim().is_empty())
    }

    pub fn clear(&mut self) {
        let mode = self.pickaxe_mode;
        *self = Self::default();
        self.pickaxe_mode = mode;
    }

    /// `git log` arguments for this filter over `revs` (revision arguments
    /// such as `--branches` or branch names). The output is one commit id
    /// per line, newest first.
    pub fn args(&self, revs: &[String]) -> Vec<String> {
        let mut a: Vec<String> = vec!["log".into(), "--format=%H".into()];
        let t = |s: &str| s.trim().to_string();
        if !self.text.trim().is_empty() || !self.author.trim().is_empty() {
            a.push("--regexp-ignore-case".into());
        }
        if !self.text.trim().is_empty() {
            a.push(format!("--grep={}", t(&self.text)));
        }
        if !self.author.trim().is_empty() {
            a.push(format!("--author={}", t(&self.author)));
        }
        if !self.since.trim().is_empty() {
            a.push(format!("--since={}", t(&self.since)));
        }
        if !self.until.trim().is_empty() {
            a.push(format!("--until={}", t(&self.until)));
        }
        if !self.pickaxe.is_empty() {
            match self.pickaxe_mode {
                Pickaxe::String => a.push(format!("-S{}", self.pickaxe)),
                Pickaxe::Regex => a.push(format!("-G{}", self.pickaxe)),
            }
        }
        a.extend(revs.iter().cloned());
        a.push("--".into());
        if !self.path.trim().is_empty() {
            a.push(t(&self.path).replace('\\', "/"));
        }
        a
    }
}
