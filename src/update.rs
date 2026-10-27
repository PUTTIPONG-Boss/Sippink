//! Updating Sippink itself from its GitHub releases.
//!
//! The latest version comes from where `releases/latest` redirects
//! (`.../releases/tag/vX.Y.Z`), which needs no API token and has no rate
//! limit. Windows won't overwrite a running exe but will rename it, so the
//! update moves the running file aside, puts the new one in its place and
//! starts it; the leftover is deleted on the next start.

use std::path::{Path, PathBuf};

use crate::job::Evt;

const REPO: &str = "https://github.com/PUTTIPONG-Boss/Sippink";

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// The newest release's tag, e.g. "v0.1.4".
pub fn latest_tag() -> Result<String, String> {
    // Don't follow the redirect: its target names the tag.
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .user_agent(concat!("Sippink/", env!("CARGO_PKG_VERSION")))
        .build();
    let resp = agent
        .head(&format!("{REPO}/releases/latest"))
        .call()
        .map_err(|e| format!("can't reach GitHub: {e}"))?;
    let location = resp.header("location").ok_or("GitHub has no release yet")?;
    let tag = location.rsplit('/').next().unwrap_or_default();
    if parse(tag).is_some() {
        Ok(tag.to_string())
    } else {
        Err(format!("unexpected release link: {location}"))
    }
}

/// "v1.2.3" or "1.2.3" as comparable numbers.
fn parse(v: &str) -> Option<(u32, u32, u32)> {
    let mut n = v.trim_start_matches('v').split('.').map(|p| p.parse::<u32>().ok());
    Some((n.next()??, n.next()??, n.next()??))
}

/// True when `tag` is a later version than this build.
pub fn is_newer(tag: &str) -> bool {
    matches!((parse(tag), parse(CURRENT)), (Some(t), Some(c)) if t > c)
}

/// Downloads release `tag` and swaps it in for the running exe. Returns the
/// path to start. Progress goes to queue row `id`.
pub fn install(agent: &ureq::Agent, tag: &str, id: u64, report: &dyn Fn(Evt)) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("no folder for the exe")?.to_path_buf();
    let dir = dir.as_path();
    let name = exe.file_name().ok_or("no exe name")?.to_string_lossy().into_owned();
    let fresh = dir.join(format!("{name}.new"));
    let url = format!("{REPO}/releases/download/{tag}/sippink-{tag}-windows-x64.exe");
    crate::job::fetch(agent, &url, &fresh, id, report).map_err(|e| {
        let _ = std::fs::remove_file(&fresh);
        if e.contains("status code 404") {
            format!("{tag} has no Windows exe on GitHub")
        } else {
            format!("download failed: {e}")
        }
    })?;
    report(Evt::Phase { id, text: "installing".into() });
    swap(&exe, &fresh).inspect_err(|_| {
        let _ = std::fs::remove_file(&fresh);
    })?;
    set_installed_version(dir, tag);
    Ok(exe)
}

/// Puts `fresh` where `exe` is. The running file is renamed to
/// `<name>.old` first; if the second step fails it is put back.
fn swap(exe: &Path, fresh: &Path) -> Result<(), String> {
    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("can't move the running exe aside: {e}"))?;
    if let Err(e) = std::fs::rename(fresh, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!("can't put the new exe in place: {e}"));
    }
    Ok(())
}

fn old_path(exe: &Path) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(".old");
    PathBuf::from(s)
}

/// Deletes what the previous update left behind (it was running then).
pub fn clean_up() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_path(&exe));
    }
}

/// Keeps Settings > Apps showing the right version when this exe is the
/// copy scripts/install.ps1 installed (no-op for any other copy).
fn set_installed_version(dir: &Path, tag: &str) {
    #[cfg(windows)]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Uninstall\Sippink";
        let query = crate::tools::hidden(Path::new("reg"))
            .args(["query", key, "/v", "InstallLocation"])
            .output();
        let location = query.map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase()).unwrap_or_default();
        let ours = location.contains(&dir.to_string_lossy().to_lowercase());
        if ours {
            let version = tag.trim_start_matches('v');
            let _ = crate::tools::hidden(Path::new("reg"))
                .args(["add", key, "/v", "DisplayVersion", "/d", version, "/f"])
                .output();
        }
    }
    #[cfg(not(windows))]
    let _ = (dir, tag);
}

/// Starts `exe` and ends this process (removing the tray icon).
pub fn restart(exe: &Path) -> ! {
    let _ = std::process::Command::new(exe).spawn();
    crate::tray::quit_now()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert_eq!(parse("v0.1.10"), Some((0, 1, 10)));
        assert!(parse("v0.1").is_none());
        assert!(parse("latest").is_none());
        assert!(matches!((parse("v0.1.10"), parse("0.1.9")), (Some(a), Some(b)) if a > b));
        assert!(!is_newer(&format!("v{CURRENT}")));
        assert!(is_newer("v99.0.0"));
    }

    #[test]
    fn swaps_and_cleans_up() {
        let dir = std::env::temp_dir().join("sippink-swap-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("app.exe");
        let fresh = dir.join("app.exe.new");
        std::fs::write(&exe, "old").unwrap();
        std::fs::write(&fresh, "new").unwrap();
        swap(&exe, &fresh).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(old_path(&exe)).unwrap(), "old");
        assert!(!fresh.exists());
    }
}
