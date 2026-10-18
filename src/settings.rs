//! User settings in `$XDG_CONFIG_HOME/moderntube/settings.toml`, loaded at startup and
//! re-read on each change. A missing file or key reads as the default.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

use serde::{Deserialize, Serialize};

const FILE: &str = "settings.toml";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub playback: Playback,
    pub catchup: Catchup,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playback {
    /// Hovering a results card plays a preview.
    pub hover_previews: bool,
    /// An AV1 ladder plays when a hardware AV1 decoder is installed.
    pub av1: bool,
}

impl Default for Playback {
    fn default() -> Self {
        Self { hover_previews: true, av1: true }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
/// Catch me up and Ask. Both are off until `summarizer.api_base` is set.
pub struct Catchup {
    pub summarizer: Endpoint,
    /// Tried with its own model and key when every base of `summarizer` fails.
    pub fallback: Endpoint,
    /// Transcription providers, tried in order.
    pub stt: Vec<SttProviderConfig>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
/// An OpenAI-compatible chat endpoint. Its API key is in the keyring ([`crate::api_keys`]).
pub struct Endpoint {
    pub model: String,
    /// Base URLs including `/v1`, comma-separated, tried in order.
    pub api_base: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
/// A transcription provider. Its API key is in the keyring under [`crate::api_keys::stt_slot`].
pub struct SttProviderConfig {
    pub name: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    /// `riva` (NVIDIA Riva over gRPC, with `function_id`) or an OpenAI-compatible endpoint.
    pub kind: String,
    pub url: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub function_id: String,
    /// Extra multipart form fields; absent means `response_format = "verbose_json"`.
    pub fields: Option<BTreeMap<String, String>>,
    /// Sub-windows transcribed at once when this provider heads the list.
    pub concurrency: Option<u32>,
}

fn store() -> &'static RwLock<Settings> {
    static SETTINGS: OnceLock<RwLock<Settings>> = OnceLock::new();
    SETTINGS.get_or_init(|| RwLock::new(crate::prefs::config_dir().map(|d| Settings::load_in(&d)).unwrap_or_default()))
}

/// Reads the settings file. Later calls are free.
pub fn init() {
    store();
}

pub fn read<T>(f: impl FnOnce(&Settings) -> T) -> T {
    f(&store().read().unwrap_or_else(|e| e.into_inner()))
}

pub fn get() -> Settings {
    read(Settings::clone)
}

/// Applies `f` to the file as it is on disk now and writes the result, so edits made to the
/// file while the app runs survive. Returns why the change was not saved.
pub fn update(f: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let mut mem = store().write().unwrap_or_else(|e| e.into_inner());
    let Some(dir) = crate::prefs::config_dir() else {
        f(&mut mem);
        return Ok(());
    };
    let r = update_in(&dir, &mut mem, f);
    if let Err(e) = &r {
        tracing::warn!("[settings] change not saved: {e}");
    }
    r
}

/// A file that does not parse stays untouched; `f` then applies to `mem` only.
fn update_in(dir: &Path, mem: &mut Settings, f: impl FnOnce(&mut Settings)) -> Result<(), String> {
    let mut disk = match Settings::read_in(dir) {
        Ok(disk) => disk,
        Err(e) => {
            f(mem);
            return Err(e);
        }
    };
    f(&mut disk);
    let saved = disk.save_in(dir).map_err(|e| format!("could not write {}: {e}", dir.join(FILE).display()));
    *mem = disk;
    saved
}

pub fn path() -> Option<PathBuf> {
    crate::prefs::config_dir().map(|d| d.join(FILE))
}

impl Settings {
    fn load_in(dir: &Path) -> Self {
        Self::read_in(dir).unwrap_or_else(|e| {
            tracing::warn!("[settings] using defaults: {e}");
            Self::default()
        })
    }

    fn read_in(dir: &Path) -> Result<Self, String> {
        let path = dir.join(FILE);
        match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|e| format!("{} is invalid: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::from_legacy_files(dir)),
            Err(e) => Err(format!("could not read {}: {e}", path.display())),
        }
    }

    /// Reads the legacy layout: one file per catch-up endpoint key, and the transcription
    /// list in `stt_providers.json`.
    fn from_legacy_files(dir: &Path) -> Self {
        let read = |f: &str| std::fs::read_to_string(dir.join(f)).map(|s| s.trim().to_string()).unwrap_or_default();
        let mut s = Self::default();
        s.catchup.summarizer = Endpoint { model: read("catchup_model"), api_base: read("catchup_api_base") };
        if let Ok(json) = std::fs::read_to_string(dir.join("stt_providers.json")) {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&json) {
                s.catchup.stt = serde_json::from_value(v["providers"].clone()).unwrap_or_default();
            }
        }
        s
    }

    fn save_in(&self, dir: &Path) -> std::io::Result<()> {
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{FILE}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, dir.join(FILE))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("moderntube-settings-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_partial_file_keeps_the_defaults_it_leaves_out() {
        let d = tmp("partial");
        std::fs::write(d.join(FILE), "[playback]\nav1 = false\n").unwrap();
        let s = Settings::load_in(&d);
        assert!(!s.playback.av1);
        assert_eq!(s.playback.hover_previews, Playback::default().hover_previews);
        assert_eq!(s.catchup, Catchup::default());
    }

    #[test]
    fn an_update_keeps_edits_made_to_the_file_and_leaves_an_invalid_file_alone() {
        let d = tmp("update");
        let mut mem = Settings::default();
        std::fs::write(d.join(FILE), "[catchup.summarizer]\nmodel = \"edited\"\n").unwrap();
        update_in(&d, &mut mem, |s| s.playback.av1 = false).unwrap();
        let s = Settings::load_in(&d);
        assert_eq!((s.catchup.summarizer.model.as_str(), s.playback.av1), ("edited", false));
        assert_eq!(mem, s);

        std::fs::write(d.join(FILE), "[playback\n").unwrap();
        assert!(update_in(&d, &mut mem, |s| s.playback.hover_previews = false).is_err());
        assert_eq!(std::fs::read_to_string(d.join(FILE)).unwrap(), "[playback\n");
        assert!(!mem.playback.hover_previews);
    }

    #[test]
    fn legacy_catchup_files_are_read_when_there_is_no_settings_file() {
        let d = tmp("legacy");
        std::fs::write(d.join("catchup_model"), "some-model\n").unwrap();
        std::fs::write(d.join("catchup_api_base"), "http://localhost:8080/v1").unwrap();
        std::fs::write(
            d.join("stt_providers.json"),
            r#"{"_comment": "x", "providers": [
                {"name": "a", "url": "https://api.example.com/v1/audio/transcriptions", "model": "m",
                 "api_key_env": "EXAMPLE_KEY", "fields": {"response_format": "verbose_json"}},
                {"name": "b", "kind": "riva", "url": "https://api.example.com", "function_id": "fid",
                 "concurrency": 8}
            ]}"#,
        )
        .unwrap();
        let s = Settings::load_in(&d);
        assert_eq!(s.catchup.summarizer.model, "some-model");
        assert_eq!(s.catchup.summarizer.api_base, "http://localhost:8080/v1");
        assert_eq!(s.catchup.stt.len(), 2);
        assert_eq!(s.catchup.stt[1].function_id, "fid");
        assert_eq!(s.catchup.stt[1].concurrency, Some(8));

        s.save_in(&d).unwrap();
        assert_eq!(Settings::load_in(&d), s, "the file written is the file read");
    }
}
