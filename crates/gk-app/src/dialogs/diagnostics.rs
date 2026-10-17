//! Optimize › Diagnostics: measures how fast git writes loose objects on the
//! repository's volume (200 blobs with `hash-object -w` into a throw-away
//! repository next to it). Slow writes on Windows are almost always real-time
//! antivirus scanning; the hint names the exact Defender exclusion and Dev
//! Drive commands (never run by the app).

use gk_cli::{CommandLog, Git};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Objects written by the measurement.
pub const OBJECTS: usize = 200;
/// Above this many milliseconds per object, writes count as slow.
pub const SLOW_MS: f64 = 2.0;

#[derive(Debug, Clone)]
pub struct WriteSpeed {
    pub per_object_ms: f64,
    pub total_ms: f64,
    /// `Some(true)` on a Dev Drive (from `fsutil devdrv query`), `None` when unknown.
    pub dev_drive: Option<bool>,
}

impl WriteSpeed {
    pub fn slow(&self) -> bool {
        self.per_object_ms > SLOW_MS
    }
}

/// A scratch folder on the same volume as `workdir`: next to it, else in its `.git`.
fn scratch_dir(workdir: &Path, git_dir: &Path) -> Result<PathBuf, String> {
    let name = format!(".gitkwaken-diag-{}", std::process::id());
    let candidates = [
        workdir.parent().map(|p| p.join(&name)),
        Some(git_dir.join(&name)),
    ];
    for c in candidates.into_iter().flatten() {
        if std::fs::create_dir_all(&c).is_ok() {
            return Ok(c);
        }
    }
    Err("Cannot create a scratch folder on the repository's volume.".into())
}

/// Runs the measurement (blocking; call from a worker thread).
pub fn measure(workdir: &Path, git_dir: &Path, log: &CommandLog) -> Result<WriteSpeed, String> {
    let dir = scratch_dir(workdir, git_dir)?;
    let r = measure_in(&dir, log);
    let _ = std::fs::remove_dir_all(&dir);
    let (per_object_ms, total_ms) = r?;
    Ok(WriteSpeed {
        per_object_ms,
        total_ms,
        dev_drive: dev_drive(workdir, log),
    })
}

fn measure_in(dir: &Path, log: &CommandLog) -> Result<(f64, f64), String> {
    let e = |e: std::io::Error| e.to_string();
    let git = Git::new(dir, log.clone());
    git.run(&["init", "-q", "."])
        .map_err(|e| format!("{e:#}"))?;
    let blobs = dir.join("blobs");
    std::fs::create_dir_all(&blobs).map_err(e)?;
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut list = String::new();
    for i in 0..OBJECTS {
        let p = blobs.join(format!("b{i:03}"));
        // Unique content: every object is a new file in .git/objects.
        let body = format!("gitkwaken write test {seed} {i}\n{}\n", "x".repeat(64 + i));
        std::fs::write(&p, body).map_err(e)?;
        list.push_str(&p.to_string_lossy());
        list.push('\n');
    }
    let start = Instant::now();
    let out = git
        .run_raw(
            &["hash-object", "-w", "--stdin-paths"],
            Some(list.as_bytes()),
        )
        .map_err(|e| format!("{e:#}"))?;
    let total = start.elapsed().as_secs_f64() * 1000.0;
    if !out.success() {
        return Err(out.stderr_str().trim().to_string());
    }
    let written = out.stdout_str().lines().count();
    if written != OBJECTS {
        return Err(format!("git wrote {written} of {OBJECTS} objects"));
    }
    Ok((total / OBJECTS as f64, total))
}

/// `fsutil devdrv query X:` (Windows 11): whether the volume is a Dev Drive.
fn dev_drive(path: &Path, log: &CommandLog) -> Option<bool> {
    if !cfg!(windows) {
        return None;
    }
    let _ = log;
    let abs = std::path::absolute(path).ok()?;
    let s = abs.to_string_lossy();
    let drive = s.get(..2).filter(|d| d.ends_with(':'))?;
    let out = std::process::Command::new("fsutil")
        .args(["devdrv", "query", drive])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
    if text.contains("not a developer volume") || text.contains("not a dev drive") {
        Some(false)
    } else if text.contains("developer volume") || text.contains("dev drive") {
        Some(true)
    } else {
        None
    }
}

/// PowerShell commands (elevated) that exclude the repository and git from
/// Defender real-time scanning.
pub fn defender_commands(workdir: &Path, git_exe: &Path) -> String {
    let repo = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    format!(
        "Add-MpPreference -ExclusionPath '{}'\nAdd-MpPreference -ExclusionProcess '{}'",
        repo.display(),
        git_exe.display()
    )
}

/// Dev Drive hint: how to check a volume and where to create one.
pub fn dev_drive_hint(workdir: &Path) -> String {
    let abs = std::path::absolute(workdir).unwrap_or_else(|_| workdir.to_path_buf());
    let s = abs.to_string_lossy().into_owned();
    let drive = s.get(..2).filter(|d| d.ends_with(':')).unwrap_or("D:");
    format!(
        "fsutil devdrv query {drive}\n# Create one: Settings › System › Storage › Advanced storage settings › Disks & volumes › Create Dev Drive"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_writes_and_cleans_up() {
        let root = std::env::temp_dir().join(format!("gk-diag-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let s = measure(&repo, &repo.join(".git"), &CommandLog::default()).unwrap();
        assert!(s.per_object_ms > 0.0 && s.total_ms >= s.per_object_ms);
        let left: Vec<_> = std::fs::read_dir(&root)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            left,
            vec![std::ffi::OsString::from("repo")],
            "scratch removed"
        );
        let cmd = defender_commands(Path::new("C:/r"), Path::new("C:/Git/cmd/git.exe"));
        assert!(cmd.starts_with("Add-MpPreference -ExclusionPath '"));
        assert!(cmd.contains("-ExclusionProcess 'C:/Git/cmd/git.exe'"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
