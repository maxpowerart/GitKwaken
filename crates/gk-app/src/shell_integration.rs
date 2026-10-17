//! Explorer integration for the MSI installer: `gitkwaken.exe
//! --register-shell-integration [--context-menu]` writes the per-user
//! "Open in GitKwaken" folder verbs and the App Paths entry, and
//! `--unregister-shell-integration` removes them.
//!
//! The MSI calls these from deferred, impersonated custom actions instead of
//! writing the keys itself: on some Windows builds per-user `RegistryValue`s
//! under these exact names, written by the msiexec service, never reach the
//! user's hive (other names do). Run as the user, like the Inno installer's
//! writes, they always do. The keys and values match `packaging/gitkwaken.iss`.

const APP_PATHS: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\App Paths\gitkwaken.exe";
const MENU_KEYS: [&str; 2] = [
    r"HKCU\Software\Classes\Directory\shell\GitKwaken",
    r"HKCU\Software\Classes\Directory\Background\shell\GitKwaken",
];

/// Handles the command-line flags; `None` when neither is present.
pub fn run_from_args() -> Option<i32> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--unregister-shell-integration") {
        unregister();
        return Some(0);
    }
    if args.iter().any(|a| a == "--register-shell-integration") {
        let menu = args.iter().any(|a| a == "--context-menu");
        return Some(match register(menu) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("shell integration: {e}");
                1
            }
        });
    }
    None
}

/// For MSI installs: the installer records its choices under
/// `HKCU\Software\GitKwaken\Installer` (those values do arrive), so on start
/// a background thread re-creates the Explorer keys when they are missing
/// or point at another executable. Covers systems where the installer's own
/// writes are lost (see the module docs). Never runs for portable or Inno
/// installs: it requires `InstallDir` to be this executable's folder.
pub fn heal_in_background() {
    let _ = std::thread::Builder::new()
        .name("gk-shell-heal".into())
        .spawn(|| {
            let Ok(exe) = std::env::current_exe() else {
                return;
            };
            let Some(dir) = exe.parent() else { return };
            let installer = r"HKCU\Software\GitKwaken\Installer";
            let Some(install_dir) = reg_query(installer, Some("InstallDir")) else {
                return;
            };
            let same = |a: &std::path::Path, b: &str| {
                let norm = |s: &str| s.trim_end_matches('\\').to_lowercase();
                norm(&a.to_string_lossy()) == norm(b)
            };
            if !same(dir, &install_dir) {
                return;
            }
            let menu = reg_query(installer, Some("ContextMenu")).is_some_and(|v| v.contains('1'));
            let exe_s = exe.to_string_lossy().into_owned();
            let app_ok = reg_query(APP_PATHS, None).is_some_and(|v| v.eq_ignore_ascii_case(&exe_s));
            let menu_ok = !menu
                || reg_query(&format!(r"{}\command", MENU_KEYS[0]), None)
                    .is_some_and(|v| v.to_lowercase().contains(&exe_s.to_lowercase()));
            if !(app_ok && menu_ok) {
                let _ = register(menu);
            }
        });
}

/// Value data of `name` (default value when `None`), as `reg query` prints it.
fn reg_query(key: &str, name: Option<&str>) -> Option<String> {
    let mut cmd = reg();
    cmd.args(["query", key]);
    match name {
        Some(n) => cmd.args(["/v", n]),
        None => cmd.arg("/ve"),
    };
    let out = cmd.output().ok()?;
    if !out.status.success() {
        return None;
    }
    parse_reg_query(&String::from_utf8_lossy(&out.stdout))
}

/// Data of the first value line ("    <name>    REG_SZ    <data>").
fn parse_reg_query(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let (_, rest) = l.split_once("    REG_")?;
        let (_, data) = rest.split_once("    ")?;
        Some(data.trim().to_string())
    })
}

fn register(menu: bool) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().into_owned();
    reg_add(APP_PATHS, None, &exe)?;
    if menu {
        for key in MENU_KEYS {
            reg_add(key, None, "Open in GitKwaken")?;
            reg_add(key, Some("Icon"), &format!("\"{exe}\",0"))?;
            reg_add(
                &format!(r"{key}\command"),
                None,
                &format!("\"{exe}\" \"%V\""),
            )?;
        }
    } else {
        for key in MENU_KEYS {
            reg_delete(key);
        }
    }
    Ok(())
}

fn unregister() {
    reg_delete(APP_PATHS);
    for key in MENU_KEYS {
        reg_delete(key);
    }
}

fn reg_add(key: &str, name: Option<&str>, data: &str) -> Result<(), String> {
    let mut cmd = reg();
    cmd.args(["add", key]);
    match name {
        Some(n) => cmd.args(["/v", n]),
        None => cmd.arg("/ve"),
    };
    cmd.args(["/t", "REG_SZ", "/d", data, "/f"]);
    let out = cmd.output().map_err(|e| format!("reg add {key}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "reg add {key}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Missing keys are fine.
fn reg_delete(key: &str) {
    let _ = reg().args(["delete", key, "/f"]).output();
}

fn reg() -> std::process::Command {
    let mut cmd = std::process::Command::new("reg.exe");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::parse_reg_query;

    #[test]
    fn parses_reg_query_output() {
        let out = "\r\nHKEY_CURRENT_USER\\Software\\GitKwaken\\Installer\r\n    InstallDir    REG_SZ    C:\\Users\\a b\\AppData\\Local\\Programs\\GitKwaken\\\r\n\r\n";
        assert_eq!(
            parse_reg_query(out).as_deref(),
            Some("C:\\Users\\a b\\AppData\\Local\\Programs\\GitKwaken\\")
        );
        let dw = "HKEY_CURRENT_USER\\Software\\GitKwaken\\Installer\n    ContextMenu    REG_DWORD    0x1\n";
        assert_eq!(parse_reg_query(dw).as_deref(), Some("0x1"));
        let def = "HKEY_CURRENT_USER\\...\\command\n    (Default)    REG_SZ    \"C:\\x\\gitkwaken.exe\" \"%V\"\n";
        assert_eq!(
            parse_reg_query(def).as_deref(),
            Some("\"C:\\x\\gitkwaken.exe\" \"%V\"")
        );
        assert_eq!(parse_reg_query("ERROR: not found"), None);
    }
}
