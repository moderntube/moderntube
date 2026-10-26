//! Local feedback notes. Each note is a folder under [`notes_dir`] holding `note.md` (YAML
//! front matter with the app state, then the text), and optionally `screenshot.png`,
//! `log.txt` and copied attachments. Notes stay on this machine.
use std::io::Write;
use std::path::{Path, PathBuf};

/// Lines of the log copied into a note.
pub const LOG_TAIL_LINES: usize = 300;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteKind {
    Bug,
    Idea,
    Other,
}

impl NoteKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bug => "bug",
            Self::Idea => "idea",
            Self::Other => "other",
        }
    }
}

/// The app state when the note was opened.
#[derive(Clone, Debug, Default)]
pub struct NoteContext {
    /// The visible stack page: results, player, shorts, channel or playlist.
    pub page: String,
    /// hidden, full or mini.
    pub player: String,
    pub video_id: Option<String>,
    pub video_title: Option<String>,
    pub position_secs: Option<f64>,
    pub live: bool,
    pub quality: Option<String>,
}

impl NoteContext {
    /// One line for the dialog: "Player · dQw4w9WgXcQ · 1:02".
    pub fn summary(&self) -> String {
        let mut parts = vec![capitalize(&self.page)];
        if self.player == "mini" {
            parts.push("miniplayer".into());
        }
        if let Some(id) = &self.video_id {
            parts.push(id.clone());
        }
        if self.live {
            parts.push("live".into());
        } else if let Some(p) = self.position_secs {
            parts.push(format_position(p));
        }
        parts.join(" · ")
    }
}

pub struct Note {
    pub kind: NoteKind,
    pub text: String,
    pub context: NoteContext,
}

#[derive(Default)]
pub struct Attachments {
    pub screenshot_png: Option<Vec<u8>>,
    pub log_tail: Option<String>,
    pub files: Vec<PathBuf>,
}

/// `$XDG_DATA_HOME/moderntube/notes`, or under `~/.local/share`.
pub fn notes_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))?;
    Some(base.join("moderntube").join("notes"))
}

/// The last `lines` lines of the file at `path`, or `None` when it is missing or empty.
pub fn log_tail(path: &Path, lines: usize) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let all: Vec<&str> = text.lines().collect();
    let tail = all[all.len().saturating_sub(lines)..].join("\n");
    (!tail.trim().is_empty()).then_some(tail)
}

/// `h:mm:ss`, or `m:ss` under an hour.
pub fn format_position(secs: f64) -> String {
    let t = secs.max(0.0) as u64;
    let (h, m, s) = (t / 3600, t / 60 % 60, t % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Saves `note` as a new folder in `dir` and returns the folder. `stamp` is the local time
/// as `YYYYMMDD-HHMMSS`; `created` is the human-readable form written to the front matter.
pub fn save_in(dir: &Path, note: &Note, att: &Attachments, stamp: &str, created: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    let stem = format!("{stamp}-{}{}", note.kind.as_str(), slug(&note.text).map(|s| format!("-{s}")).unwrap_or_default());
    let folder = unique_folder(dir, &stem)?;

    let mut links = Vec::new();
    if let Some(png) = &att.screenshot_png {
        write_private(&folder.join("screenshot.png"), png)?;
        links.push("![screenshot](screenshot.png)".to_string());
    }
    if let Some(log) = &att.log_tail {
        write_private(&folder.join("log.txt"), log.as_bytes())?;
        links.push("[log](log.txt)".to_string());
    }
    for (i, src) in att.files.iter().enumerate() {
        let name = attachment_name(i, src);
        std::fs::copy(src, folder.join(&name))?;
        links.push(if is_image(&name) { format!("![]({name})") } else { format!("[{name}]({name})") });
    }
    write_private(&folder.join("note.md"), render_markdown(note, created, &links).as_bytes())?;
    Ok(folder)
}

/// Front matter, the note text, then the attachment links.
pub fn render_markdown(note: &Note, created: &str, links: &[String]) -> String {
    let c = &note.context;
    let mut out = String::from("---\n");
    let mut field = |k: &str, v: String| out.push_str(&format!("{k}: {v}\n"));
    field("kind", note.kind.as_str().into());
    field("status", "open".into());
    field("created", quote(created));
    field("app_version", quote(env!("CARGO_PKG_VERSION")));
    field("page", quote(&c.page));
    field("player", quote(&c.player));
    if let Some(id) = &c.video_id {
        field("video_id", quote(id));
        field("url", quote(&format!("https://www.youtube.com/watch?v={id}")));
    }
    if let Some(t) = &c.video_title {
        field("video_title", quote(t));
    }
    if let Some(p) = c.position_secs {
        field("position", quote(&format_position(p)));
        field("position_secs", format!("{p:.1}"));
    }
    if c.live {
        field("live", "true".into());
    }
    if let Some(q) = &c.quality {
        field("quality", quote(q));
    }
    out.push_str("---\n\n");
    out.push_str(note.text.trim());
    out.push('\n');
    if !links.is_empty() {
        out.push_str("\n## Attachments\n\n");
        for l in links {
            out.push_str(&format!("- {l}\n"));
        }
    }
    out
}

/// A JSON string is a valid YAML double-quoted scalar.
fn quote(s: &str) -> String {
    serde_json::Value::String(s.to_string()).to_string()
}

/// The first few words of `text` as lowercase ASCII joined by `-`.
fn slug(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
        .map(|w| w.to_ascii_lowercase())
        .collect();
    let mut s = words.join("-");
    s.truncate(48);
    let s = s.trim_end_matches('-').to_string();
    (!s.is_empty()).then_some(s)
}

fn unique_folder(dir: &Path, stem: &str) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    for n in 1..1000 {
        let name = if n == 1 { stem.to_string() } else { format!("{stem}-{n}") };
        let path = dir.join(name);
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "no free note folder name"))
}

fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?.write_all(bytes)
}

/// `attachment-N.ext`, keeping the source's extension.
fn attachment_name(i: usize, src: &Path) -> String {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .filter(|e| e.chars().all(|c| c.is_ascii_alphanumeric()))
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .unwrap_or_default();
    format!("attachment-{}{ext}", i + 1)
}

fn is_image(name: &str) -> bool {
    [".png", ".jpg", ".jpeg", ".webp", ".gif"].iter().any(|e| name.ends_with(e))
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(text: &str) -> Note {
        Note {
            kind: NoteKind::Bug,
            text: text.into(),
            context: NoteContext {
                page: "player".into(),
                player: "full".into(),
                video_id: Some("dQw4w9WgXcQ".into()),
                video_title: Some("A \"quoted\" title".into()),
                position_secs: Some(62.4),
                ..Default::default()
            },
        }
    }

    #[test]
    fn markdown_front_matter_quotes_strings() {
        let md = render_markdown(&note("Seek bar jumps"), "2026-10-26 15:30:12", &["[log](log.txt)".into()]);
        assert!(md.starts_with("---\nkind: bug\nstatus: open\n"));
        assert!(md.contains("video_title: \"A \\\"quoted\\\" title\"\n"));
        assert!(md.contains("position: \"1:02\"\n"));
        assert!(md.contains("---\n\nSeek bar jumps\n\n## Attachments\n\n- [log](log.txt)\n"));
    }

    #[test]
    fn slug_takes_leading_ascii_words() {
        assert_eq!(slug("Shorts: skips to the next one on scroll-end!").as_deref(), Some("shorts-skips-to-the-next-one"));
        assert_eq!(slug("🙂 ✨"), None);
    }

    #[test]
    fn save_in_writes_a_folder_per_note() {
        let dir = std::env::temp_dir().join(format!("moderntube-notes-test-{}", std::process::id()));
        let att = Attachments { screenshot_png: Some(vec![1, 2, 3]), log_tail: Some("a\nb".into()), files: vec![] };
        let a = save_in(&dir, &note("Seek bar jumps"), &att, "20261026-153012", "now").unwrap();
        let b = save_in(&dir, &note("Seek bar jumps"), &att, "20261026-153012", "now").unwrap();
        assert_eq!(a.file_name().unwrap(), "20261026-153012-bug-seek-bar-jumps");
        assert_eq!(b.file_name().unwrap(), "20261026-153012-bug-seek-bar-jumps-2");
        let md = std::fs::read_to_string(a.join("note.md")).unwrap();
        assert!(md.contains("![screenshot](screenshot.png)"));
        assert_eq!(std::fs::read(a.join("screenshot.png")).unwrap(), vec![1, 2, 3]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn log_tail_keeps_the_last_lines() {
        let path = std::env::temp_dir().join(format!("moderntube-log-tail-{}", std::process::id()));
        std::fs::write(&path, "1\n2\n3\n4\n").unwrap();
        assert_eq!(log_tail(&path, 2).as_deref(), Some("3\n4"));
        std::fs::remove_file(&path).unwrap();
    }
}
