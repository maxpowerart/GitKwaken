//! "Undo last operation": reads the reflog to find what happened last and
//! how to revert it, previews the effect, and refuses when uncommitted
//! changes would be lost.
//!
//! * commit / amend: `git reset --soft <branch>@{1}` (the changes stay staged)
//! * merge, rebase, reset, pull, cherry-pick, revert…: `git reset --keep <branch>@{1}`
//! * checkout: switch back to the previous branch / commit

use super::TabDialog;
use super::common::*;
use crate::app::Shared;
use crate::tab::RepoTab;
use egui::{Context, RichText};
use gk_core::Snapshot;

/// How the last operation is undone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    /// `git reset --soft <target>`.
    Soft(String),
    /// `git reset --keep <target>`.
    Keep(String),
    /// `git checkout <rev>`.
    Checkout(String),
}

/// The reflog entry to undo and the planned fix (or why it cannot be undone).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// Reflog subject of the operation, e.g. `commit: Add x`.
    pub what: String,
    pub fix: Result<Fix, String>,
    /// Commits leaving the branch ("abcd1234 Summary").
    pub removed: Vec<String>,
    /// Commits coming back.
    pub restored: Vec<String>,
}

/// Picks the fix for the newest reflog entry `subject` of a branch (or of
/// HEAD when detached); `previous` is the entry before it (the target).
pub fn plan_for(subject: &str, previous: Option<&str>) -> Result<Fix, String> {
    let action = subject.split(": ").next().unwrap_or(subject);
    let verb = action.split([' ', '(']).next().unwrap_or(action);
    if action == "commit (initial)" {
        return Err("The first commit of a branch cannot be undone.".into());
    }
    if verb == "branch" || action.starts_with("Branch") {
        return Err("Nothing to undo: the branch was just created.".into());
    }
    let Some(prev) = previous else {
        return Err("Nothing to undo: the reflog has no earlier state.".into());
    };
    Ok(match action {
        "commit" | "commit (amend)" => Fix::Soft(prev.to_string()),
        _ => Fix::Keep(prev.to_string()),
    })
}

/// `checkout: moving from <a> to <b>` -> `a`.
pub fn checkout_from(subject: &str) -> Option<&str> {
    let rest = subject.strip_prefix("checkout: moving from ")?;
    let (from, _) = rest.rsplit_once(" to ")?;
    Some(from)
}

/// Reflog entries (`hash`, `subject`) of `rev`, newest first.
fn reflog(git: &gk_cli::Git, rev: &str, n: usize) -> Vec<(String, String)> {
    let n = format!("-n{n}");
    git.run_raw(
        &[
            "reflog",
            "show",
            n.as_str(),
            "--format=%H%x00%gs",
            rev,
            "--",
        ],
        None,
    )
    .ok()
    .filter(|o| o.code == Some(0))
    .map(|o| {
        o.stdout_str()
            .lines()
            .filter_map(|l| l.split_once('\0'))
            .map(|(h, s)| (h.to_string(), s.to_string()))
            .collect()
    })
    .unwrap_or_default()
}

fn commits(git: &gk_cli::Git, range: &str) -> Vec<String> {
    git.run(&["log", "--format=%h  %s", "-n50", range, "--"])
        .map(|o| o.stdout_str().lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Works out the plan (runs git; call on a background thread). `busy`: the
/// label of an operation in progress (merge, rebase…), which blocks undo.
pub fn make_plan(git: &gk_cli::Git, branch: Option<String>, busy: Option<&'static str>) -> Plan {
    let head = reflog(git, "HEAD", 1);
    let mut plan = Plan {
        what: String::new(),
        fix: Err("Nothing to undo: the reflog is empty.".into()),
        removed: Vec::new(),
        restored: Vec::new(),
    };
    if let Some((_, s)) = head.first()
        && let Some(from) = checkout_from(s)
    {
        plan.what = s.clone();
        plan.fix = Ok(Fix::Checkout(from.to_string()));
        return plan;
    }
    let rev = branch
        .as_deref()
        .map(|b| format!("refs/heads/{b}"))
        .unwrap_or_else(|| "HEAD".into());
    let log = reflog(git, &rev, 2);
    let Some((_, subject)) = log.first() else {
        return plan;
    };
    plan.what = subject.clone();
    plan.fix = plan_for(subject, log.get(1).map(|(h, _)| h.as_str()));
    if let Some(state) = busy {
        plan.fix = Err(format!(
            "An operation is in progress ({state}); finish or abort it first."
        ));
        return plan;
    }
    if let Ok(Fix::Soft(t) | Fix::Keep(t)) = &plan.fix {
        plan.removed = commits(git, &format!("{t}..HEAD"));
        plan.restored = commits(git, &format!("HEAD..{t}"));
    }
    if let Ok(Fix::Keep(t)) = &plan.fix {
        // `reset --keep` refuses to touch files with local changes that differ
        // between the commits; say so up front instead of failing later.
        let changed: Vec<String> = git
            .run(&["diff", "--name-only", "-z", t.as_str(), "HEAD", "--"])
            .map(|o| {
                o.stdout_str()
                    .split('\0')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        // Tracked files with staged or unstaged changes (from git, so a
        // status that is still loading cannot hide them).
        let dirty: Vec<String> = git
            .run(&["diff", "--name-only", "-z", "HEAD", "--"])
            .map(|o| {
                o.stdout_str()
                    .split('\0')
                    .filter(|p| !p.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let lost: Vec<&String> = changed.iter().filter(|p| dirty.contains(p)).collect();
        if !lost.is_empty() {
            let list: Vec<&str> = lost.iter().take(5).map(|s| s.as_str()).collect();
            plan.fix = Err(format!(
                "Uncommitted changes in {} file(s) would be lost: {}{}. Commit or stash them first.",
                lost.len(),
                list.join(", "),
                if lost.len() > 5 { ", …" } else { "" }
            ));
        }
    }
    plan
}

pub struct Undo {
    plan: Option<Task<Plan>>,
}

impl Undo {
    pub fn new() -> Self {
        Self { plan: None }
    }
}

impl TabDialog for Undo {
    fn show(
        &mut self,
        ctx: &Context,
        tab: &mut RepoTab,
        snap: &Snapshot,
        _shared: &mut Shared,
    ) -> bool {
        if self.plan.is_none() && refs_loaded(snap) {
            let git = tab.session.repo().git().clone();
            let branch = snap.refs.head.branch().map(str::to_string);
            let busy =
                (snap.refs.state != gk_core::RepoState::Clean).then(|| snap.refs.state.label());
            self.plan = Some(Task::spawn(ctx, move || make_plan(&git, branch, busy)));
        }
        let plan = self.plan.as_mut().and_then(|t| t.poll()).cloned();
        let (out, ()) = modal(
            ctx,
            "undo",
            "Undo last operation",
            560.0,
            "Undo",
            |ui, ctl| {
                let Some(plan) = &plan else {
                    ctl.invalid();
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Reading the reflog…");
                    });
                    return;
                };
                if !plan.what.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label("Last operation:");
                        ui.label(RichText::new(&plan.what).strong());
                    });
                }
                match &plan.fix {
                    Err(e) => {
                        ctl.invalid();
                        ui.add_space(4.0);
                        error(ui, e);
                    }
                    Ok(fix) => {
                        let how = match fix {
                            Fix::Soft(t) => format!(
                                "Moves the branch back to {} (git reset --soft). The undone commit's changes stay staged.",
                                crate::util::short_id(t)
                            ),
                            Fix::Keep(t) => format!(
                                "Moves the branch back to {} (git reset --keep). Uncommitted changes are kept.",
                                crate::util::short_id(t)
                            ),
                            Fix::Checkout(r) => format!("Switches back to {r} (git checkout)."),
                        };
                        ui.label(how);
                        let list = |ui: &mut egui::Ui, title: &str, v: &[String]| {
                            if v.is_empty() {
                                return;
                            }
                            ui.add_space(4.0);
                            ui.label(RichText::new(title).strong());
                            for c in v {
                                ui.label(RichText::new(c).monospace());
                            }
                        };
                        list(ui, "Commits removed from the branch:", &plan.removed);
                        list(ui, "Commits restored:", &plan.restored);
                    }
                }
            },
        );
        finish(out, || {
            let Some(Ok(fix)) = plan.map(|p| p.fix) else {
                return;
            };
            let (label, args) = match fix {
                Fix::Soft(t) => ("Undo commit", vec!["reset".to_string(), "--soft".into(), t]),
                Fix::Keep(t) => ("Undo", vec!["reset".to_string(), "--keep".into(), t]),
                Fix::Checkout(r) => ("Undo checkout", vec!["checkout".to_string(), r]),
            };
            tab.session.run_op(label, move |r, c, p| {
                let a: Vec<&str> = args.iter().map(String::as_str).collect();
                r.git().run_streaming(&a, None, c, p)?.check()?;
                Ok(String::new())
            });
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans() {
        assert_eq!(
            plan_for("commit: Add x", Some("abc")),
            Ok(Fix::Soft("abc".into()))
        );
        assert_eq!(
            plan_for("commit (amend): Add x", Some("abc")),
            Ok(Fix::Soft("abc".into()))
        );
        for s in [
            "merge feature: Merge made by the 'ort' strategy.",
            "rebase (finish): refs/heads/x onto 123",
            "reset: moving to HEAD~2",
            "pull: Fast-forward",
            "commit (merge): Merge branch 'x'",
            "cherry-pick: Fix",
        ] {
            assert_eq!(plan_for(s, Some("abc")), Ok(Fix::Keep("abc".into())), "{s}");
        }
        assert!(plan_for("commit (initial): first", Some("abc")).is_err());
        assert!(plan_for("branch: Created from HEAD", Some("abc")).is_err());
        assert!(plan_for("commit: x", None).is_err());
        assert_eq!(
            checkout_from("checkout: moving from main to feature/x"),
            Some("main")
        );
        assert_eq!(checkout_from("commit: x"), None);
    }
}
