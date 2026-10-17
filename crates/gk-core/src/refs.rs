use crate::Repo;
use anyhow::Result;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Branch {
    /// Short name, e.g. `feature/x`.
    pub name: String,
    pub target: String,
    /// Short upstream name, e.g. `origin/feature/x`.
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    /// Upstream configured but gone on the remote.
    pub upstream_gone: bool,
    pub is_head: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteBranch {
    pub remote: String,
    /// Name without the remote prefix.
    pub name: String,
    pub target: String,
}

impl RemoteBranch {
    pub fn full(&self) -> String {
        format!("{}/{}", self.remote, self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tag {
    pub name: String,
    /// Commit the tag points to (peeled).
    pub target: String,
    pub annotated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stash {
    pub index: usize,
    pub id: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub fetch_url: String,
    pub push_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Head {
    Branch(String),
    Detached(String),
    /// A branch with no commits yet.
    Unborn(String),
    #[default]
    Unknown,
}

impl Head {
    pub fn branch(&self) -> Option<&str> {
        match self {
            Head::Branch(b) | Head::Unborn(b) => Some(b),
            _ => None,
        }
    }
}

/// In-progress multi-step operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RepoState {
    #[default]
    Clean,
    Merging,
    Rebasing { interactive: bool },
    CherryPicking,
    Reverting,
    Bisecting,
    ApplyingMailbox,
}

impl RepoState {
    pub fn label(self) -> &'static str {
        match self {
            RepoState::Clean => "",
            RepoState::Merging => "MERGING",
            RepoState::Rebasing { interactive: true } => "REBASE-i",
            RepoState::Rebasing { .. } => "REBASING",
            RepoState::CherryPicking => "CHERRY-PICKING",
            RepoState::Reverting => "REVERTING",
            RepoState::Bisecting => "BISECTING",
            RepoState::ApplyingMailbox => "AM",
        }
    }
}

/// Everything shown in the sidebar.
#[derive(Debug, Clone, Default)]
pub struct Refs {
    pub head: Head,
    pub head_id: Option<String>,
    pub state: RepoState,
    pub branches: Vec<Branch>,
    pub remote_branches: Vec<RemoteBranch>,
    pub tags: Vec<Tag>,
    pub stashes: Vec<Stash>,
    pub remotes: Vec<Remote>,
    /// Labels per commit id, for decorating the history.
    pub labels: BTreeMap<String, Vec<Label>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Label {
    Head,
    Branch { name: String, is_head: bool },
    Remote(String),
    Tag(String),
    Stash,
}

impl Refs {
    pub fn branch(&self, name: &str) -> Option<&Branch> {
        self.branches.iter().find(|b| b.name == name)
    }

    pub fn current_branch(&self) -> Option<&Branch> {
        self.branches.iter().find(|b| b.is_head)
    }

    /// All tips to walk for "all branches" history.
    pub fn tips(&self) -> Vec<String> {
        let mut v: Vec<String> = self.branches.iter().map(|b| b.target.clone()).collect();
        v.extend(self.remote_branches.iter().map(|b| b.target.clone()));
        v.extend(self.tags.iter().map(|t| t.target.clone()));
        if let Some(h) = &self.head_id {
            v.push(h.clone());
        }
        v.sort();
        v.dedup();
        v
    }
}

fn parse_track(s: &str) -> (u32, u32, bool) {
    // "ahead 1, behind 2" | "ahead 3" | "behind 1" | "gone" | ""
    if s.trim() == "gone" {
        return (0, 0, true);
    }
    let (mut ahead, mut behind) = (0, 0);
    for part in s.split(',') {
        let mut it = part.split_whitespace();
        match (it.next(), it.next()) {
            (Some("ahead"), Some(n)) => ahead = n.parse().unwrap_or(0),
            (Some("behind"), Some(n)) => behind = n.parse().unwrap_or(0),
            _ => {}
        }
    }
    (ahead, behind, false)
}

pub fn load(repo: &Repo) -> Result<Refs> {
    let git = repo.git();
    let mut refs = Refs::default();
    let out = git.run(&[
        "for-each-ref",
        "--format=%(refname)%00%(objectname)%00%(*objectname)%00%(upstream:short)%00%(upstream:track,nobracket)%00%(HEAD)%00%(symref)%00%(objecttype)",
    ])?;
    for line in out.stdout_str().lines() {
        let f: Vec<&str> = line.split('\0').collect();
        if f.len() < 8 {
            continue;
        }
        let (name, oid, peeled, upstream, track, head, symref, otype) =
            (f[0], f[1], f[2], f[3], f[4], f[5], f[6], f[7]);
        if let Some(b) = name.strip_prefix("refs/heads/") {
            let (ahead, behind, gone) = parse_track(track);
            refs.branches.push(Branch {
                name: b.to_string(),
                target: oid.to_string(),
                upstream: (!upstream.is_empty()).then(|| upstream.to_string()),
                ahead,
                behind,
                upstream_gone: gone,
                is_head: head == "*",
            });
        } else if let Some(r) = name.strip_prefix("refs/remotes/") {
            if !symref.is_empty() {
                continue; // origin/HEAD
            }
            if let Some((remote, b)) = r.split_once('/') {
                refs.remote_branches.push(RemoteBranch {
                    remote: remote.to_string(),
                    name: b.to_string(),
                    target: oid.to_string(),
                });
            }
        } else if let Some(t) = name.strip_prefix("refs/tags/") {
            let annotated = otype == "tag";
            refs.tags.push(Tag {
                name: t.to_string(),
                target: if annotated && !peeled.is_empty() { peeled } else { oid }.to_string(),
                annotated,
            });
        }
    }

    // HEAD via gitoxide (no process needed).
    let g = repo.gix();
    match g.head_name() {
        Ok(Some(name)) => {
            let short = name.shorten().to_string();
            match g.head_id() {
                Ok(id) => {
                    refs.head_id = Some(id.to_string());
                    refs.head = Head::Branch(short);
                }
                Err(_) => refs.head = Head::Unborn(short),
            }
        }
        Ok(None) => {
            if let Ok(id) = g.head_id() {
                refs.head_id = Some(id.to_string());
                refs.head = Head::Detached(id.to_string());
            }
        }
        Err(_) => {}
    }
    refs.state = repo_state(repo);

    let stash = git.run_raw(&["stash", "list", "--format=%H%x00%gs"], None)?;
    if stash.success() {
        for (i, line) in stash.stdout_str().lines().enumerate() {
            if let Some((id, msg)) = line.split_once('\0') {
                refs.stashes.push(Stash { index: i, id: id.to_string(), message: msg.to_string() });
            }
        }
    }

    let remotes = git.run_raw(&["remote", "-v"], None)?;
    let mut by_name: BTreeMap<String, Remote> = BTreeMap::new();
    for line in remotes.stdout_str().lines() {
        let mut it = line.split_whitespace();
        let (Some(name), Some(url), kind) = (it.next(), it.next(), it.next()) else { continue };
        let e = by_name.entry(name.to_string()).or_insert_with(|| Remote {
            name: name.to_string(),
            fetch_url: String::new(),
            push_url: String::new(),
        });
        if kind == Some("(push)") {
            e.push_url = url.to_string();
        } else {
            e.fetch_url = url.to_string();
        }
    }
    refs.remotes = by_name.into_values().collect();

    // Labels for the history view.
    let mut labels: BTreeMap<String, Vec<Label>> = BTreeMap::new();
    if let (Some(id), Head::Detached(_)) = (&refs.head_id, &refs.head) {
        labels.entry(id.clone()).or_default().push(Label::Head);
    }
    for b in &refs.branches {
        labels.entry(b.target.clone()).or_default().push(Label::Branch { name: b.name.clone(), is_head: b.is_head });
    }
    for b in &refs.remote_branches {
        labels.entry(b.target.clone()).or_default().push(Label::Remote(b.full()));
    }
    for t in &refs.tags {
        labels.entry(t.target.clone()).or_default().push(Label::Tag(t.name.clone()));
    }
    if let Some(s) = refs.stashes.first() {
        labels.entry(s.id.clone()).or_default().push(Label::Stash);
    }
    refs.labels = labels;
    Ok(refs)
}

pub fn repo_state(repo: &Repo) -> RepoState {
    let gd = repo.git_dir();
    if gd.join("rebase-merge").is_dir() {
        RepoState::Rebasing { interactive: gd.join("rebase-merge/interactive").exists() }
    } else if gd.join("rebase-apply").is_dir() {
        if gd.join("rebase-apply/applying").exists() {
            RepoState::ApplyingMailbox
        } else {
            RepoState::Rebasing { interactive: false }
        }
    } else if gd.join("MERGE_HEAD").exists() {
        RepoState::Merging
    } else if gd.join("CHERRY_PICK_HEAD").exists() {
        RepoState::CherryPicking
    } else if gd.join("REVERT_HEAD").exists() {
        RepoState::Reverting
    } else if gd.join("BISECT_LOG").exists() {
        RepoState::Bisecting
    } else {
        RepoState::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn track_parsing() {
        assert_eq!(parse_track("ahead 1, behind 2"), (1, 2, false));
        assert_eq!(parse_track("behind 7"), (0, 7, false));
        assert_eq!(parse_track("gone"), (0, 0, true));
        assert_eq!(parse_track(""), (0, 0, false));
    }
}
