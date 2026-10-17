//! The on-disk transcript cache: one JSON file per video and window start.
use super::CatchupSegment;

/// A transcribed sub-window, persisted so a retry or a re-opened panel reuses it.
/// Transcription time is the scarce resource (Groq caps audio-seconds per hour), so a late
/// failure must not discard transcription already done.
#[derive(Clone)]
pub(super) struct CachedChunk {
    /// The window_from this chunk was fetched at (the resume key).
    pub(super) window_from: i64,
    pub(super) to_ms: i64,
    pub(super) segments: Vec<CatchupSegment>,
}

/// Cache dir for catch-up transcripts (`$XDG_CACHE_HOME|~/.cache/moderntube/catchup`).
pub(super) fn cache_dir() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .map(|base| base.join("moderntube").join("catchup"))
}

/// Per-(video, window-start) cache file. A retry reuses the same `from_ms`, so its chunk
/// sequence matches and every transcribed chunk is a cache hit.
pub(super) fn cache_file(video_id: &str, from_ms: i64) -> Option<std::path::PathBuf> {
    Some(cache_dir()?.join(format!("{}_{from_ms}.json", cache_file_stem(video_id))))
}

/// The id reduced to path-safe characters.
pub(super) fn cache_file_stem(video_id: &str) -> String {
    video_id.chars().filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-').collect()
}

/// Load cached chunks (best-effort; any parse/IO error → empty).
pub(super) fn load_cache(path: &std::path::Path) -> Vec<CachedChunk> {
    let Ok(txt) = std::fs::read_to_string(path) else { return Vec::new() };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else { return Vec::new() };
    v["chunks"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let segments = c["segments"]
                .as_array()?
                .iter()
                .filter_map(|s| {
                    Some(CatchupSegment {
                        start_ms: s["start_ms"].as_i64()?,
                        text: s["text"].as_str()?.to_string(),
                    })
                })
                .collect();
            Some(CachedChunk {
                window_from: c["window_from"].as_i64()?,
                to_ms: c["to_ms"].as_i64()?,
                segments,
            })
        })
        .collect()
}

/// Persist the chunk cache (best-effort; failures never abort a run).
pub(super) fn save_cache(path: &std::path::Path, chunks: &[CachedChunk]) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let json = serde_json::json!({
        "chunks": chunks.iter().map(|c| serde_json::json!({
            "window_from": c.window_from,
            "to_ms": c.to_ms,
            "segments": c.segments.iter().map(|s| serde_json::json!({
                "start_ms": s.start_ms, "text": s.text,
            })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    let _ = std::fs::write(path, json.to_string());
}

/// Every chunk cached for `video_id`, across all the windows it was transcribed for.
pub(super) fn load_video_cache_in(dir: &std::path::Path, video_id: &str) -> Vec<CachedChunk> {
    let prefix = format!("{}_", cache_file_stem(video_id));
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .flat_map(|e| load_cache(&e.path()))
        .collect()
}

/// Segments of `chunks` inside `[from_ms, to_ms)` in timeline order. Chunks overlap by a few
/// seconds (the server rounds spans up); a segment past the next chunk's start is that chunk's.
pub(super) fn assemble(chunks: &[CachedChunk], from_ms: i64, to_ms: i64) -> Vec<CatchupSegment> {
    let mut ordered: Vec<&CachedChunk> =
        chunks.iter().filter(|c| c.to_ms > from_ms && c.window_from < to_ms).collect();
    ordered.sort_by_key(|c| (c.window_from, c.to_ms));
    ordered.dedup_by_key(|c| c.window_from);
    let mut segments: Vec<CatchupSegment> = Vec::new();
    for (i, c) in ordered.iter().enumerate() {
        let next_start = ordered.get(i + 1).map(|n| n.window_from).unwrap_or(i64::MAX);
        let floor = segments.last().map_or(i64::MIN, |s| s.start_ms);
        segments.extend(c.segments.iter().filter(|sg| sg.start_ms < next_start && sg.start_ms > floor).cloned());
    }
    segments
}

/// A gap shorter than this between the window and a cached chunk is not worth a fetch.
pub(super) const SLIVER_MS: i64 = 5_000;

/// Sub-window starts that still need fetching for `[from_ms, wend)`: deterministic
/// `step` ms from each uncovered offset, so a retry's cache lines up.
pub(super) fn plan_starts(cache: &[CachedChunk], from_ms: i64, wend: i64, step: i64) -> Vec<i64> {
    let mut starts = Vec::new();
    let mut s = from_ms;
    while wend - s >= SLIVER_MS {
        if let Some(c) = cache
            .iter()
            .filter(|c| c.window_from <= s + SLIVER_MS && c.to_ms > s)
            .max_by_key(|c| c.to_ms)
        {
            s = c.to_ms;
            continue;
        }
        starts.push(s);
        s += step;
    }
    starts
}

/// Everything transcribed so far for `video_id`, by catch-up or Transcribe.
pub fn cached_transcript(video_id: &str) -> Vec<CatchupSegment> {
    let Some(dir) = cache_dir() else { return Vec::new() };
    assemble(&load_video_cache_in(&dir, video_id), i64::MIN, i64::MAX)
}
