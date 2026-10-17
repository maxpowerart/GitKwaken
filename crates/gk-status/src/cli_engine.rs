use crate::{Conflict, Kind, Status, StatusBuilder, Untracked};
use anyhow::{Result, bail};
use bstr::{BString, ByteSlice};

/// Status via `git status --porcelain=v2 -z`; the reference implementation.
///
/// fsmonitor: nothing to do here. When the repository sets `core.fsmonitor`
/// (the Optimize dialog turns it on, together with `core.untrackedCache`),
/// this `git status` asks the fsmonitor daemon which paths changed since its
/// last token and skips stat-ing the rest, so a full scan of a 1M-file tree
/// costs about as much as the changes. [`crate::fsmonitor_state`] reports
/// whether the daemon runs. gitoxide has no fsmonitor client, so
/// [`crate::compute`] routes full scans of such repositories here even with
/// the gix engine selected; the watcher fast path does not need the daemon
/// (it only stats the paths the OS reported).
pub fn compute(git: &gk_cli::Git, patterns: &[BString], untracked: Untracked) -> Result<Status> {
    let mode = match untracked {
        Untracked::All => "--untracked-files=all",
        Untracked::Normal => "--untracked-files=normal",
        Untracked::No => "--untracked-files=no",
    };
    let mut args: Vec<String> = ["status", "--porcelain=v2", "-z", "--find-renames", mode]
        .map(String::from)
        .to_vec();
    // `git status` has no --pathspec-from-file; past the Windows command-line
    // limit a full run is used and the caller's merge narrows it to its scope.
    let total: usize = patterns.iter().map(|p| p.len() + 12).sum();
    if !patterns.is_empty() && total < MAX_ARGS_LEN {
        args.push("--".into());
        args.extend(patterns.iter().map(|p| format!(":(literal){p}")));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = git.run(&args)?;
    parse_porcelain_v2(&out.stdout)
}

const MAX_ARGS_LEN: usize = 24_000;

fn kind_from(c: u8) -> Option<Kind> {
    Some(match c {
        b'M' => Kind::Modified,
        b'A' => Kind::Added,
        b'D' => Kind::Deleted,
        b'R' => Kind::Renamed,
        b'C' => Kind::Copied,
        b'T' => Kind::TypeChange,
        _ => return None,
    })
}

pub fn parse_porcelain_v2(data: &[u8]) -> Result<Status> {
    let mut b = StatusBuilder::new();
    let mut records = data.split(|b| *b == 0).filter(|r| !r.is_empty());
    while let Some(rec) = records.next() {
        match rec[0] {
            b'1' => {
                // 1 XY sub mH mI mW hH hI path
                let fields: Vec<&[u8]> = rec.splitn(9, |b| *b == b' ').collect();
                if fields.len() < 9 {
                    bail!("malformed status record: {}", rec.as_bstr());
                }
                push_xy(&mut b, fields[1], fields[8], None, fields[2]);
            }
            b'2' => {
                // 2 XY sub mH mI mW hH hI Xscore path\0origPath
                let fields: Vec<&[u8]> = rec.splitn(10, |b| *b == b' ').collect();
                if fields.len() < 10 {
                    bail!("malformed rename record: {}", rec.as_bstr());
                }
                let orig = records.next();
                push_xy(&mut b, fields[1], fields[9], orig, fields[2]);
            }
            b'u' => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let fields: Vec<&[u8]> = rec.splitn(11, |b| *b == b' ').collect();
                if fields.len() < 11 {
                    bail!("malformed unmerged record: {}", rec.as_bstr());
                }
                let c = match fields[1] {
                    b"DD" => Conflict::BothDeleted,
                    b"AU" => Conflict::AddedByUs,
                    b"UD" => Conflict::DeletedByThem,
                    b"UA" => Conflict::AddedByThem,
                    b"DU" => Conflict::DeletedByUs,
                    b"AA" => Conflict::BothAdded,
                    _ => Conflict::BothModified,
                };
                b.unstaged(fields[10], Kind::Conflict(c));
            }
            b'?' => b.unstaged(&rec[2..], Kind::Untracked),
            b'!' | b'#' => {}
            _ => bail!("unknown status record: {}", rec.as_bstr()),
        }
    }
    Ok(b.finish())
}

fn push_xy(b: &mut StatusBuilder, xy: &[u8], path: &[u8], orig: Option<&[u8]>, sub: &[u8]) {
    let (x, y) = (xy[0], xy[1]);
    if let Some(kind) = kind_from(x) {
        b.staged(path, kind, orig);
    }
    // Submodules report worktree changes in `sub` (e.g. "SC.." / "S.M." / "S..U").
    let sub_dirty =
        sub.len() == 4 && sub[0] == b'S' && (sub[1] == b'C' || sub[2] == b'M' || sub[3] == b'U');
    if let Some(kind) = kind_from(y) {
        let kind = if x == b'A' && y == b'D' {
            Kind::Deleted
        } else {
            kind
        };
        b.unstaged(path, kind);
    } else if y == b'.' && x == b'.' && sub_dirty {
        b.unstaged(path, Kind::Modified);
    }
    if x == b'.' && y == b'A' {
        // intent-to-add shows as ".A"
        b.unstaged(path, Kind::IntentToAdd);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_records() {
        let data = b"1 .M N... 100644 100644 100644 aaa aaa src/a.rs\0\
2 R. N... 100644 100644 100644 aaa aaa R100 new.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 a b c conflict.txt\0\
? untracked file.txt\0";
        let s = parse_porcelain_v2(data).unwrap();
        assert_eq!(s.unstaged.len(), 3);
        assert_eq!(s.staged.len(), 1);
        assert_eq!(s.staged[0].kind, Kind::Renamed);
        assert_eq!(s.staged[0].orig_path.as_ref().unwrap(), "old.rs");
        assert_eq!(s.unstaged[2].path, "untracked file.txt");
        assert!(matches!(
            s.unstaged[1].kind,
            Kind::Conflict(Conflict::BothModified)
        ));
    }
}
