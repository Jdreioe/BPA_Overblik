//! Check GitHub releases and replace the running package.
//!
//! Windows and Linux are one file the app can overwrite. macOS is a `.pkg`, so
//! the updater opens the installer instead of writing into `/Applications`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use teamup_shift_sync_core::live::app_version;

const RELEASES: &str = "https://api.github.com/repos/Jdreioe/teamup_sync/releases/latest";
const FETCH_FAILED: &str = "Opdateringen kunne ikke hentes. Prøv igen senere.";
const INSTALL_FAILED: &str = "Opdateringen kunne ikke installeres. Prøv igen senere.";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Offer {
    pub version: String,
    pub asset_name: String,
    url: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApplyOutcome {
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    Restart(PathBuf),
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    OpenedInstaller,
}

pub fn is_release_version(version: &str) -> bool {
    let bytes = version.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'.'
        && bytes[7] == b'.'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

pub fn is_newer(current: &str, latest: &str) -> bool {
    is_release_version(current) && is_release_version(latest) && latest > current
}

pub fn asset_suffix(os: &str) -> Option<&'static str> {
    match os {
        "windows" => Some("x86_64.exe"),
        "linux" => Some("x86_64.AppImage"),
        "macos" => Some("universal.pkg"),
        _ => None,
    }
}

pub fn offer_from_release(body: &str, current: &str, os: &str) -> Result<Option<Offer>, String> {
    let release: Value = serde_json::from_str(body).map_err(|_| FETCH_FAILED.to_string())?;
    let tag = release["tag_name"].as_str().unwrap_or_default();
    let latest = tag.strip_prefix('v').unwrap_or(tag);
    if !is_newer(current, latest) {
        return Ok(None);
    }
    let suffix = asset_suffix(os).ok_or_else(|| FETCH_FAILED.to_string())?;
    let expected = format!("teamup-shift-sync-{latest}-{suffix}");
    let assets = release["assets"]
        .as_array()
        .ok_or_else(|| FETCH_FAILED.to_string())?;
    for asset in assets {
        if asset["name"].as_str() != Some(expected.as_str()) {
            continue;
        }
        let url = asset["browser_download_url"]
            .as_str()
            .filter(|url| url.starts_with("https://"))
            .ok_or_else(|| FETCH_FAILED.to_string())?;
        return Ok(Some(Offer {
            version: latest.to_owned(),
            asset_name: expected,
            url: url.to_owned(),
        }));
    }
    Err(FETCH_FAILED.to_string())
}

pub async fn check_latest() -> Result<Option<Offer>, String> {
    let body = fetch_latest().await?;
    offer_from_release(&body, app_version(), std::env::consts::OS)
}

pub async fn apply(offer: Offer) -> Result<ApplyOutcome, String> {
    #[cfg(target_os = "macos")]
    {
        let path = std::env::temp_dir().join(&offer.asset_name);
        download(&offer.url, &path).await?;
        open_path(&path)?;
        return Ok(ApplyOutcome::OpenedInstaller);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let current = install_path()?;
        let staged = sibling(&current, ".new");
        download(&offer.url, &staged).await?;
        make_executable(&staged)?;
        replace_current(&current, &staged)?;
        Ok(ApplyOutcome::Restart(current))
    }
}

pub fn restart(path: &Path) -> Result<(), String> {
    spawn_restart(path).map(|_| ()).map_err(|_| {
        "Opdateringen er hentet, men appen kunne ikke startes igen. Åbn den nye fil selv."
            .to_string()
    })
}

/// Start the replaced package as its own process.
///
/// An AppImage child must not inherit this process's mount (`APPDIR` and
/// friends), or it keeps running the old image. On Windows the new process
/// has to leave this one's console group so closing the old window does not
/// close the new one.
fn spawn_restart(path: &Path) -> std::io::Result<std::process::Child> {
    restart_command(path).spawn()
}

fn restart_command(path: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(path);
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            command.current_dir(dir);
        }
    }
    for key in ["APPIMAGE", "APPDIR", "ARGV0", "OWD"] {
        command.env_remove(key);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x00000008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    command
}

/// Drop the previous binary left behind after a Windows/Linux replace.
pub fn cleanup_replaced_backup() {
    let Ok(current) = install_path() else {
        return;
    };
    let _ = std::fs::remove_file(sibling(&current, ".old"));
}

fn install_path() -> Result<PathBuf, String> {
    if let Ok(appimage) = std::env::var("APPIMAGE") {
        if !appimage.is_empty() {
            return Ok(PathBuf::from(appimage));
        }
    }
    std::env::current_exe().map_err(|_| INSTALL_FAILED.to_string())
}

fn sibling(path: &Path, extra: &str) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|name| name.to_os_string())
        .unwrap_or_default();
    name.push(extra);
    path.with_file_name(name)
}

#[cfg(not(target_os = "macos"))]
fn replace_current(current: &Path, staged: &Path) -> Result<(), String> {
    let backup = sibling(current, ".old");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(current, &backup).map_err(|_| INSTALL_FAILED.to_string())?;
    if std::fs::rename(staged, current).is_err() {
        let _ = std::fs::rename(&backup, current);
        return Err(INSTALL_FAILED.to_string());
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn make_executable(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)
            .map_err(|_| INSTALL_FAILED.to_string())?
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).map_err(|_| INSTALL_FAILED.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(target_os = "macos")]
fn open_path(path: &Path) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|_| INSTALL_FAILED.to_string())
}

async fn fetch_latest() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("teamup-shift-sync/0.1")
        .build()
        .map_err(|_| FETCH_FAILED.to_string())?;
    let response = client
        .get(RELEASES)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|_| FETCH_FAILED.to_string())?;
    if !response.status().is_success() {
        return Err(FETCH_FAILED.to_string());
    }
    response.text().await.map_err(|_| FETCH_FAILED.to_string())
}

async fn download(url: &str, dest: &Path) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .user_agent("teamup-shift-sync/0.1")
        .build()
        .map_err(|_| FETCH_FAILED.to_string())?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|_| FETCH_FAILED.to_string())?;
    if !response.status().is_success() {
        return Err(FETCH_FAILED.to_string());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| FETCH_FAILED.to_string())?;
    write_package(dest, &bytes)
}

/// Refuse an HTML error page or a truncated download before it replaces the
/// running program.
fn package_bytes_ok(bytes: &[u8]) -> bool {
    if bytes.len() < 64 {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        bytes.starts_with(b"MZ")
    }
    #[cfg(target_os = "linux")]
    {
        bytes.starts_with(b"\x7fELF")
    }
    #[cfg(target_os = "macos")]
    {
        bytes.starts_with(b"xar!")
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

fn write_package(dest: &Path, bytes: &[u8]) -> Result<(), String> {
    if !package_bytes_ok(bytes) {
        return Err(INSTALL_FAILED.to_string());
    }
    let mut file = std::fs::File::create(dest).map_err(|_| INSTALL_FAILED.to_string())?;
    std::io::Write::write_all(&mut file, bytes).map_err(|_| INSTALL_FAILED.to_string())?;
    file.sync_all().map_err(|_| INSTALL_FAILED.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const BODY: &str = r#"{
        "tag_name": "v2026.09.22",
        "assets": [
            {"name": "teamup-shift-sync-2026.09.22-x86_64.exe", "browser_download_url": "https://example.test/win.exe"},
            {"name": "teamup-shift-sync-2026.09.22-universal.pkg", "browser_download_url": "https://example.test/mac.pkg"},
            {"name": "teamup-shift-sync-2026.09.22-x86_64.AppImage", "browser_download_url": "https://example.test/linux.AppImage"}
        ]
    }"#;

    #[test]
    fn a_newer_dated_release_selects_this_system_asset() {
        let offer = offer_from_release(BODY, "2026.09.21", "windows")
            .expect("parse")
            .expect("newer");
        assert_eq!(offer.version, "2026.09.22");
        assert_eq!(offer.asset_name, "teamup-shift-sync-2026.09.22-x86_64.exe");
        assert_eq!(offer.url, "https://example.test/win.exe");
        assert_eq!(
            offer_from_release(BODY, "2026.09.21", "linux")
                .unwrap()
                .unwrap()
                .asset_name,
            "teamup-shift-sync-2026.09.22-x86_64.AppImage"
        );
        assert_eq!(
            offer_from_release(BODY, "2026.09.21", "macos")
                .unwrap()
                .unwrap()
                .asset_name,
            "teamup-shift-sync-2026.09.22-universal.pkg"
        );
    }

    #[test]
    fn development_builds_and_current_releases_are_not_offered() {
        assert_eq!(offer_from_release(BODY, "0.1.0", "linux").unwrap(), None);
        assert_eq!(
            offer_from_release(BODY, "2026.09.22", "linux").unwrap(),
            None
        );
        assert_eq!(
            offer_from_release(BODY, "2026.10.01", "linux").unwrap(),
            None
        );
    }

    #[test]
    fn a_missing_asset_is_an_error_not_a_current_install() {
        assert!(offer_from_release(BODY, "2026.09.21", "freebsd").is_err());
        let empty = r#"{"tag_name":"v2026.09.22","assets":[]}"#;
        assert!(offer_from_release(empty, "2026.09.21", "linux").is_err());
    }

    #[test]
    fn http_assets_are_rejected() {
        let body = r#"{
            "tag_name": "v2026.09.22",
            "assets": [{"name": "teamup-shift-sync-2026.09.22-x86_64.AppImage", "browser_download_url": "http://example.test/linux.AppImage"}]
        }"#;
        assert!(offer_from_release(body, "2026.09.21", "linux").is_err());
    }

    #[test]
    fn a_restart_does_not_keep_the_running_appimage_mount() {
        let command = restart_command(Path::new("/tmp/teamup-shift-sync.AppImage"));
        for key in ["APPIMAGE", "APPDIR", "ARGV0", "OWD"] {
            assert!(
                command
                    .get_envs()
                    .any(|(name, value)| name == key && value.is_none()),
                "{key} must not be inherited"
            );
        }
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn replace_swaps_the_running_file_and_keeps_the_previous_one() {
        let dir = tempfile::tempdir().expect("temp dir");
        let current = dir
            .path()
            .join("teamup-shift-sync-2026.09.21-x86_64.AppImage");
        let staged = sibling(&current, ".new");
        std::fs::write(&current, b"old").expect("old package");
        std::fs::write(&staged, b"new").expect("staged package");
        replace_current(&current, &staged).expect("replace");
        assert_eq!(std::fs::read(&current).expect("new package"), b"new");
        assert_eq!(
            std::fs::read(sibling(&current, ".old")).expect("backup"),
            b"old"
        );
        assert!(!staged.exists());
    }

    #[test]
    fn an_error_page_is_not_a_package() {
        assert!(!package_bytes_ok(
            b"<!DOCTYPE html><html>not a package</html>"
        ));
        assert!(!package_bytes_ok(&[0; 8]));
        let mut bytes = vec![0u8; 64];
        #[cfg(target_os = "linux")]
        bytes[..4].copy_from_slice(b"\x7fELF");
        #[cfg(target_os = "windows")]
        bytes[..2].copy_from_slice(b"MZ");
        #[cfg(target_os = "macos")]
        bytes[..4].copy_from_slice(b"xar!");
        #[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
        assert!(package_bytes_ok(&bytes));
    }

    #[test]
    fn sibling_keeps_the_real_extension() {
        let path = PathBuf::from("/tmp/teamup-shift-sync-2026.09.21-x86_64.exe");
        assert_eq!(
            sibling(&path, ".old"),
            PathBuf::from("/tmp/teamup-shift-sync-2026.09.21-x86_64.exe.old")
        );
    }
}
