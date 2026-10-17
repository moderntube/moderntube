//! Cookie-header persistence: secret-service keyring (oo7) with a 0600-file fallback.
use std::path::{Path, PathBuf};

/// Attributes identifying our item in the secret-service keyring.
const ATTRS: &[(&str, &str)] = &[("app", "moderntube"), ("kind", "yt-cookie-header")];
const LABEL: &str = "moderntube YouTube session";

/// `~/.config/moderntube/session` (honours `XDG_CONFIG_HOME`). `None` if neither
/// `XDG_CONFIG_HOME` nor `HOME` is set.
fn fallback_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("moderntube").join("session"))
}

fn save_file(path: &Path, header: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, header)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn load_file(path: &Path) -> Option<String> {
    let s = std::fs::read_to_string(path).ok()?;
    let trimmed = s.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn clear_file(path: &Path) {
    let _ = std::fs::remove_file(path);
}

async fn save_keyring(header: &str) -> Result<(), oo7::Error> {
    let kr = oo7::Keyring::new().await?;
    let _ = kr.unlock().await; // login keyring is usually already unlocked
    kr.create_item(LABEL, &ATTRS, oo7::Secret::text(header), true).await?;
    Ok(())
}

async fn load_keyring() -> Option<String> {
    let kr = oo7::Keyring::new().await.ok()?;
    let _ = kr.unlock().await;
    let items = kr.search_items(&ATTRS).await.ok()?;
    let secret = items.first()?.secret().await.ok()?;
    String::from_utf8(secret.as_bytes().to_vec()).ok()
}

async fn clear_keyring() {
    if let Ok(kr) = oo7::Keyring::new().await {
        let _ = kr.unlock().await;
        let _ = kr.delete(&ATTRS).await;
    }
}

/// Persist the cookie header. Prefers the secret-service keyring; on any keyring
/// error (e.g. no daemon) falls back to a 0600 file.
pub async fn save(header: &str) {
    if save_keyring(header).await.is_err() {
        if let Some(path) = fallback_path() {
            let _ = save_file(&path, header);
        }
    }
}

/// Load the persisted cookie header, if any (keyring first, then the file fallback).
pub async fn load() -> Option<String> {
    // On a private D-Bus session the secret-service activation never answers.
    if crate::hooks::flag("no-keyring") {
        return fallback_path().and_then(|p| load_file(&p));
    }
    if let Some(header) = load_keyring().await {
        return Some(header);
    }
    fallback_path().and_then(|p| load_file(&p))
}

/// Remove the persisted cookie from both backends.
pub async fn clear() {
    clear_keyring().await;
    if let Some(path) = fallback_path() {
        clear_file(&path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_session_file_reloads_and_is_private() {
        let dir = std::env::temp_dir().join(format!("mt_store_test_{}", std::process::id()));
        let path = dir.join("session");
        let _ = std::fs::remove_dir_all(&dir);

        assert_eq!(load_file(&path), None, "missing file -> None");
        save_file(&path, "SAPISID=abc; X=y").unwrap();
        assert_eq!(load_file(&path).as_deref(), Some("SAPISID=abc; X=y"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "file must be 0600");
        }

        clear_file(&path);
        assert_eq!(load_file(&path), None, "after clear -> None");
        let _ = std::fs::remove_dir_all(&dir);
    }

}
