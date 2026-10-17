//! Batch "Catch me up" summary for a live stream's DVR window.
//!
//! Pipeline: audio window ([`innertube::fetch_audio_window`]), STT provider chain (segment
//! timestamps), live-chat replay over the same window, then a summary from the configured
//! OpenAI-compatible endpoint ([`SummarizerConfig`]) with clickable `[h:mm:ss]` anchors,
//! returned as a [`CatchupResult`].
//!
//! GUI-free: runs on the tokio runtime and reports progress over a channel. Timestamps are
//! stream-absolute ms, the SABR `player_time` coordinate (REQ-004) that
//! `SabrStreamOptions::start_ms` takes, so a clicked anchor maps straight onto a seek.

use innertube::{
    fetch_audio_window, ChatReplay, ChatRun, ClientConfig, NSigSolver, Session,
};

mod cache;
mod stt;
mod summary;

pub use cache::cached_transcript;
pub(crate) use summary::chat_complete;
pub use summary::{SummarizerConfig, SUMMARIZER_OFF};
use cache::{assemble, cache_dir, cache_file, load_cache, load_video_cache_in, plan_starts, save_cache, CachedChunk};
#[cfg(test)]
use stt::{parse_stt_providers, SttKind};
use stt::{stt_concurrency, stt_providers, transcribe};
use summary::summarize;

/// One transcript segment, stream-absolute.
#[derive(Debug, Clone)]
pub struct CatchupSegment {
    pub start_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct CatchupResult {
    /// Markdown-ish summary: a TL;DR paragraph + `[h:mm:ss]`-anchored bullets.
    pub summary: String,
    pub segments: Vec<CatchupSegment>,
    pub from_ms: i64,
    pub to_ms: i64,
    /// How many chat messages were folded in (0 = replay unavailable / none).
    pub chat_count: usize,
}

/// Progress phases for the panel's status line.
#[derive(Debug, Clone)]
pub enum CatchupPhase {
    /// Fetching audio: (buffered_ms_of_window, window_ms).
    Fetching(i64, i64),
    /// Transcribing chunk `.0` of `.1`. Transcription is the longest phase, so the counts
    /// keep the status line moving.
    Transcribing(usize, usize),
    /// Every STT provider rate-limited this chunk; waiting `secs` before retrying (transcript so
    /// far is cached, so no work is lost).
    RateLimited(u64),
    Chat,
    Summarizing,
}

/// Format a stream-absolute ms as `h:mm:ss` (or `m:ss` under an hour).
pub fn fmt_ts(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m}:{sec:02}")
    }
}

/// Parse `h:mm:ss` / `m:ss` back to ms (the panel's link hrefs).
pub fn parse_ts(t: &str) -> Option<i64> {
    let parts: Vec<&str> = t.split(':').collect();
    let nums: Vec<i64> = parts.iter().map(|p| p.parse().ok()).collect::<Option<_>>()?;
    match nums[..] {
        [m, s] => Some((m * 60 + s) * 1000),
        [h, m, s] => Some((h * 3600 + m * 60 + s) * 1000),
        _ => None,
    }
}

/// Max bytes per Groq upload (the file cap is 25 MB). At the usual ~128 kbps live audio that
/// is ~24 minutes; longer windows are fetched and transcribed in sub-windows and stitched
/// with offset timestamps.
const GROQ_MAX_BYTES: usize = 23 * 1024 * 1024;

/// Sub-window bounds (ms). A sub-window is the unit of parallel fetch, transcription and cache.
const SUBWINDOW_MIN_MS: i64 = 60 * 1000;
const SUBWINDOW_MAX_MS: i64 = 5 * 60 * 1000;

/// Concurrent SABR audio streams off one bootstrap. Beyond it the server answers 401 (SABR-055).
const FETCH_CONCURRENCY: usize = 24;

/// A SABR stream fetches at ~3.5x real time whatever the count, so the window is split to
/// give every fetch stream one sub-window.
fn subwindow_ms(window_ms: i64) -> i64 {
    ((window_ms + FETCH_CONCURRENCY as i64 - 1) / FETCH_CONCURRENCY as i64).clamp(SUBWINDOW_MIN_MS, SUBWINDOW_MAX_MS)
}

/// `(window_from, to_ms, transcribe time, segments)` for one finished sub-window.
type TranscribedChunk = (i64, i64, std::time::Duration, Result<Vec<CatchupSegment>, String>);

pub async fn run_catchup(
    video_id: String,
    from_ms: i64,
    to_ms: Option<i64>,
    progress: async_channel::Sender<CatchupPhase>,
    summarizer: SummarizerConfig,
) -> Result<CatchupResult, String> {
    if summarizer.is_off() {
        return Err(SUMMARIZER_OFF.to_string());
    }
    let t_run = std::time::Instant::now();
    let (segments, resolved_to) = transcribe_window(&video_id, from_ms, to_ms, &progress).await?;
    let t_transcribed = t_run.elapsed();

    let _ = progress.try_send(CatchupPhase::Chat);
    let session = Session::new(ClientConfig::web()).map_err(|e| e.to_string())?;
    let t2 = std::time::Instant::now();
    let chat = fetch_chat_window(&session, &video_id, from_ms, resolved_to).await;
    let chat_count = chat.len();
    let t_chat = t2.elapsed();

    let _ = progress.try_send(CatchupPhase::Summarizing);
    let t3 = std::time::Instant::now();
    let summary = summarize(&segments, &chat, from_ms, resolved_to, &summarizer).await?;
    tracing::info!(
        "[catchup] timing: transcript={:.1}s chat={:.1}s summarize={:.1}s total={:.1}s (window {:.0}s)",
        t_transcribed.as_secs_f64(),
        t_chat.as_secs_f64(),
        t3.elapsed().as_secs_f64(),
        t_run.elapsed().as_secs_f64(),
        (resolved_to - from_ms) as f64 / 1000.0,
    );
    Ok(CatchupResult { summary, segments, from_ms, to_ms: resolved_to, chat_count })
}

/// Transcribe `[from_ms, to_ms]` of a live DVR window (`None` = the live edge), reusing every
/// chunk cached for the video. Returns the segments and the resolved window end.
pub async fn transcribe_window(
    video_id: &str,
    from_ms: i64,
    to_ms: Option<i64>,
    progress: &async_channel::Sender<CatchupPhase>,
) -> Result<(Vec<CatchupSegment>, i64), String> {
    let video_id = video_id.to_string();
    let progress = progress.clone();
    let session = Session::new(ClientConfig::web()).map_err(|e| e.to_string())?;
    let solver = NSigSolver::new();
    let pot = crate::playback::pot_backend();

    // ---- 1. Audio in sub-windows. Every cached sub-window is reused as is, since
    // transcription time is the scarce resource. ----
    let cf = cache_file(&video_id, from_ms);
    let cached: Vec<CachedChunk> = cf.as_deref().map(load_cache).unwrap_or_default();
    let mut cache: Vec<CachedChunk> = match cache_dir() {
        Some(dir) => load_video_cache_in(&dir, &video_id),
        None => cached.clone(),
    };
    cache.sort_by_key(|c| (c.window_from, c.to_ms));
    cache.dedup_by_key(|c| (c.window_from, c.to_ms));

    let mut window_to = to_ms; // None = live head, resolved by the first fetch
    // An edge-bounded retry ends at the cache's reach: it reuses the span already done
    // instead of chasing the moved live edge. A fresh run with no cache resolves it live.
    if window_to.is_none() {
        if let Some(max_to) = cached.iter().map(|c| c.to_ms).max() {
            window_to = Some(max_to);
        }
    }

    // Per-stage wall time, logged at the end.
    let t_run = std::time::Instant::now();
    let mut t_transcribe = std::time::Duration::ZERO;

    // ---- 1a. An unbounded window ends at the live head, which one SABR round reports.
    let mut source: Option<innertube::AudioSource> = None;
    let mut live_head: Option<i64> = None;
    if window_to.is_none() {
        let s = innertube::open_audio_source(&session, &solver, &pot, &video_id)
            .await
            .map_err(|e| format!("audio fetch: {e}"))?;
        let head = innertube::live_head_ms(&s, from_ms)
            .await
            .map_err(|e| format!("audio fetch: {e}"))?
            .ok_or("the live head is unknown")?;
        tracing::info!("[catchup] live head at {}", fmt_ts(head));
        live_head = Some(head);
        window_to = Some(head);
        source = Some(s);
    }
    let wend = window_to.unwrap_or(from_ms);
    let total_window = (wend - from_ms).max(0);

    // ---- 1b. Fetch the remaining sub-windows concurrently off one bootstrap, transcribed
    // in completion order. ----
    let sub = subwindow_ms(total_window);
    let starts = plan_starts(&cache, from_ms, wend, sub);
    let planned_total: i64 = starts.iter().map(|&s| (wend - s).min(sub)).sum();
    let done_base = (total_window - planned_total).max(0);
    let _ = progress.try_send(CatchupPhase::Fetching(done_base.min(total_window), total_window));

    if !starts.is_empty() {
        let source = match source.take() {
            Some(s) => s,
            None => innertube::open_audio_source(&session, &solver, &pot, &video_id)
                .await
                .map_err(|e| format!("audio fetch: {e}"))?,
        };
        if live_head.is_none() {
            live_head = innertube::live_head_ms(&source, from_ms).await.ok().flatten();
        }
        let n_chunks = starts.len();
        tracing::info!(
            "[catchup] plan: video={video_id} window={}..{} ({:.0}s) stream_length={} cached_chunks={} \
             subwindow={:.0}s chunks={n_chunks} concurrency={FETCH_CONCURRENCY} source={}",
            fmt_ts(from_ms),
            fmt_ts(wend),
            total_window as f64 / 1000.0,
            live_head.map_or("unknown".to_string(), fmt_ts),
            cache.len(),
            sub as f64 / 1000.0,
            source.describe(),
        );
        let t_fetch = std::time::Instant::now();
        let in_flight = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fails_401 = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let done_map: std::sync::Arc<std::sync::Mutex<std::collections::HashMap<i64, i64>>> =
            Default::default();
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(FETCH_CONCURRENCY));
        let (tx, mut rx) =
            tokio::sync::mpsc::unbounded_channel::<(i64, Result<innertube::AudioWindow, String>)>();
        for &start in &starts {
            let sub_end = (start + sub).min(wend);
            let sub_to = Some(sub_end);
            let sem = sem.clone();
            let tx = tx.clone();
            let source = source.clone();
            let done_map = done_map.clone();
            let ptx = progress.clone();
            let in_flight = in_flight.clone();
            let fails_401 = fails_401.clone();
            tokio::spawn(async move {
                let Ok(_permit) = sem.acquire_owned().await else { return };
                let t0 = std::time::Instant::now();
                let started_with = in_flight.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                let started_at = t_fetch.elapsed().as_secs_f64();
                let r = async {
                    let mut cb = move |end: Option<i64>, _t: Option<i64>, _b: usize| {
                        if let Some(e) = end {
                            let mut m = done_map.lock().unwrap();
                            m.insert(start, (e - start).max(0));
                            let done: i64 = done_base + m.values().sum::<i64>();
                            let _ = ptx
                                .try_send(CatchupPhase::Fetching(done.min(total_window), total_window));
                        }
                    };
                    innertube::fetch_audio_range(&source, start, sub_to, Some(&mut cb)).await
                }
                .await;
                let left_with = in_flight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                let span = format!(
                    "{}..{}, started +{started_at:.1}s with {started_with} in flight, ended with {left_with}",
                    fmt_ts(start),
                    fmt_ts(sub_end)
                );
                match &r {
                    Ok(w) => tracing::info!(
                        "[catchup] chunk@{}s: fetched {:.0}s of audio ({} B) in {:.1}s [{span}]",
                        (start - from_ms) / 1000,
                        (w.to_ms - w.from_ms) as f64 / 1000.0,
                        w.bytes.len(),
                        t0.elapsed().as_secs_f64()
                    ),
                    Err(e) => {
                        if e.http_status() == Some(401) {
                            fails_401.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        }
                        let e = e.to_string();
                        let short = e.split(" for url").next().unwrap_or(&e);
                        tracing::info!(
                            "[catchup] chunk@{}s: FAILED after {:.1}s: {short} [{span}]",
                            (start - from_ms) / 1000,
                            t0.elapsed().as_secs_f64()
                        )
                    }
                }
                let _ = tx.send((start, r.map_err(|e| format!("audio fetch: {e}"))));
            });
        }
        drop(tx);
        // Transcribe in completion order, overlapped with the ongoing fetches, at the
        // head provider's concurrency (Groq throttles parallel uploads; NVIDIA does not).
        let stt_sem = std::sync::Arc::new(tokio::sync::Semaphore::new(stt_concurrency(&stt_providers())));
        let mut jobs: tokio::task::JoinSet<TranscribedChunk> = tokio::task::JoinSet::new();
        let mut t_first_dispatch: Option<std::time::Instant> = None;
        let mut done_chunks = 0usize;
        let mut failed = 0usize;
        let mut last_err = String::new();
        let finish = |(start, to, took, r): TranscribedChunk, cache: &mut Vec<CachedChunk>| -> Result<(), String> {
            let segs = r?;
            tracing::info!("[catchup] chunk@{}s: transcribed in {:.1}s", (start - from_ms) / 1000, took.as_secs_f64());
            cache.push(CachedChunk { window_from: start, to_ms: to, segments: segs });
            if let Some(p) = cf.as_deref() {
                save_cache(p, cache);
            }
            Ok(())
        };
        while let Some((start, r)) = rx.recv().await {
            let win = match r {
                Ok(w) => w,
                Err(e) => {
                    failed += 1;
                    last_err = e;
                    continue;
                }
            };
            if win.bytes.len() > GROQ_MAX_BYTES {
                return Err(format!(
                    "audio chunk unexpectedly large ({}MB) — bitrate assumption broken",
                    win.bytes.len() / (1024 * 1024)
                ));
            }
            if let Some(dir) = crate::hooks::get("catchup-save-audio") {
                let path = std::path::Path::new(&dir).join(format!("{}_{}.m4a", win.from_ms, win.to_ms));
                if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, &win.bytes)) {
                    tracing::warn!("[catchup] could not save {}: {e}", path.display());
                }
            }
            // Announced before the call, since transcription is the longest phase.
            done_chunks += 1;
            let _ = progress.try_send(CatchupPhase::Transcribing(done_chunks, n_chunks));
            t_first_dispatch.get_or_insert_with(std::time::Instant::now);
            let permit = stt_sem.clone().acquire_owned().await.map_err(|e| e.to_string())?;
            let progress = progress.clone();
            jobs.spawn(async move {
                let _permit = permit;
                let t1 = std::time::Instant::now();
                let r = transcribe(&win.bytes, win.from_ms, &progress).await;
                (start, win.to_ms, t1.elapsed(), r)
            });
            while let Some(done) = jobs.try_join_next() {
                finish(done.map_err(|e| e.to_string())?, &mut cache)?;
            }
        }
        while let Some(done) = jobs.join_next().await {
            finish(done.map_err(|e| e.to_string())?, &mut cache)?;
        }
        if let Some(t) = t_first_dispatch {
            t_transcribe += t.elapsed();
        }
        let n401 = fails_401.load(std::sync::atomic::Ordering::SeqCst);
        tracing::warn!(
            "[catchup] fetch summary: {} of {n_chunks} chunks ok, {failed} failed ({n401} with 401, {} other) \
             at concurrency {FETCH_CONCURRENCY}, {:.1}s; last error: {}",
            n_chunks - failed,
            failed - n401.min(failed),
            t_fetch.elapsed().as_secs_f64(),
            if last_err.is_empty() { "none" } else { last_err.split(" for url").next().unwrap_or(&last_err) },
        );
        if failed == n_chunks {
            return Err(last_err);
        }
    }

    // ---- 1c. Retry whole sub-windows a failed fetch left out. A shorter gap is a held DVR
    // stretch the range already probed past (ad breaks), and refetching it only waits. ----
    {
        let mut guard = 0u32;
        let mut s = from_ms;
        while s < wend && guard < 6 {
            if let Some(c) = cache
                .iter()
                .filter(|c| c.window_from <= s && c.to_ms > s)
                .max_by_key(|c| c.to_ms)
            {
                s = c.to_ms;
                continue;
            }
            guard += 1;
            let gap_end = cache
                .iter()
                .map(|c| c.window_from)
                .filter(|&w| w > s)
                .min()
                .unwrap_or(wend)
                .min(wend);
            if gap_end - s < sub {
                s = gap_end;
                continue;
            }
            tracing::info!("[catchup] gap top-up: {}s..{}s", (s - from_ms) / 1000, (gap_end - from_ms) / 1000);
            let covered: i64 = cache.iter().map(|c| (c.to_ms.min(wend) - c.window_from.max(from_ms)).max(0)).sum();
            let _ = progress.try_send(CatchupPhase::Fetching(covered.min(total_window), total_window));
            let t_gap = std::time::Instant::now();
            let win = fetch_audio_window(&session, &solver, &pot, &video_id, s, Some(gap_end), None)
                .await
                .map_err(|e| format!("audio fetch: {e}"))?;
            tracing::info!(
                "[catchup] gap top-up {}..{}: fetched {:.0}s of audio in {:.1}s",
                fmt_ts(s),
                fmt_ts(gap_end),
                (win.to_ms - win.from_ms).max(0) as f64 / 1000.0,
                t_gap.elapsed().as_secs_f64()
            );
            if win.to_ms <= s {
                s = gap_end; // DVR hole (SABR-043): nothing there, move past it
                continue;
            }
            if win.bytes.len() > GROQ_MAX_BYTES {
                return Err("gap chunk unexpectedly large".to_string());
            }
            let t1 = std::time::Instant::now();
            let segs = transcribe(&win.bytes, win.from_ms, &progress).await?;
            t_transcribe += t1.elapsed();
            cache.push(CachedChunk { window_from: s, to_ms: win.to_ms, segments: segs });
            if let Some(p) = cf.as_deref() {
                save_cache(p, &cache);
            }
        }
    }

    tracing::info!(
        "[catchup] transcript: fetch+transcribe={:.1}s (transcribe {:.1}s of it, window {:.0}s)",
        t_run.elapsed().as_secs_f64(),
        t_transcribe.as_secs_f64(),
        (wend - from_ms) as f64 / 1000.0,
    );
    Ok((assemble(&cache, from_ms, wend), wend))
}

/// Best-effort chat over the window: walks the replay pages from `from_ms` until past
/// `to_ms`. Returns `(offset_ms, author, text)`, empty when replay is unavailable; the
/// summary then rests on the transcript alone.
async fn fetch_chat_window(
    session: &Session,
    video_id: &str,
    from_ms: i64,
    to_ms: i64,
) -> Vec<(i64, String, String)> {
    let mut out = Vec::new();
    let Ok(ChatReplay::Available(first)) = session.live_chat_replay(video_id).await else {
        return out;
    };
    let mut cont = first.continuation;
    let mut offset = from_ms;
    // At most 40 pages of ~30 to 100 messages each.
    for _ in 0..40 {
        let Some(c) = cont.take() else { break };
        let Ok(page) = session.live_chat_replay_page(&c, offset).await else { break };
        let mut max_seen = offset;
        for m in &page.messages {
            let Some(off) = m.video_offset_ms else { continue };
            if off >= from_ms && off <= to_ms {
                let text: String = m
                    .runs
                    .iter()
                    .map(|r| match r {
                        ChatRun::Text(t) => t.clone(),
                        ChatRun::Link { text, .. } => text.clone(),
                        ChatRun::Emoji { shortcut, .. } => shortcut.clone(),
                    })
                    .collect();
                if !text.is_empty() {
                    out.push((off, m.author.clone(), text));
                }
            }
            max_seen = max_seen.max(off);
        }
        if max_seen >= to_ms {
            break;
        }
        offset = max_seen.max(offset) + 1;
        cont = page.continuation;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizer_bases_never_end_in_a_slash_or_come_out_empty() {
        // A trailing slash would produce ".../v1//chat/completions".
        let c = SummarizerConfig::from_parts(
            "m",
            "http://localhost:8080/v1/ , https://api.example.com/v1",
            "K",
        );
        assert_eq!(c.bases.len(), 2);
        assert!(c.bases.iter().all(|b| !b.is_empty() && !b.ends_with('/')), "{:?}", c.bases);
        let one = SummarizerConfig::from_parts("m", "https://api.example.com/v1", "K");
        assert_eq!(one.bases.len(), 1);
    }

    fn seg(start_ms: i64, text: &str) -> CatchupSegment {
        CatchupSegment { start_ms, text: text.to_string() }
    }

    #[test]
    fn transcripts_from_every_window_of_a_video_merge_in_order_without_repeats() {
        let dir = std::env::temp_dir().join(format!("mt-catchup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let early = CachedChunk { window_from: 0, to_ms: 60_000, segments: vec![seg(0, "a"), seg(58_000, "b")] };
        let late = CachedChunk { window_from: 57_000, to_ms: 120_000, segments: vec![seg(58_000, "b"), seg(90_000, "c")] };
        save_cache(&dir.join("vid_0.json"), std::slice::from_ref(&early));
        save_cache(&dir.join("vid_57000.json"), &[early.clone(), late]);
        save_cache(&dir.join("other_0.json"), &[CachedChunk { window_from: 0, to_ms: 1, segments: vec![seg(0, "x")] }]);

        let chunks = load_video_cache_in(&dir, "vid");
        let texts: Vec<String> = assemble(&chunks, i64::MIN, i64::MAX).into_iter().map(|s| s.text).collect();
        assert_eq!(texts, ["a", "b", "c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_summary_anchor_parses_back_to_the_second_it_was_written_from() {
        for secs in (0..40_000).step_by(37).chain([59, 60, 3_599, 3_600, 86_399]) {
            let ms = secs * 1000;
            assert_eq!(parse_ts(&fmt_ts(ms)), Some(ms), "{secs} s");
        }
    }

    #[test]
    fn cached_transcript_chunks_reload_unchanged() {
        let dir = std::env::temp_dir().join(format!("mt-catchup-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.json");
        let chunks = vec![
            CachedChunk {
                window_from: 0,
                to_ms: 1_200_000,
                segments: vec![
                    CatchupSegment { start_ms: 500, text: "hello".into() },
                    CatchupSegment { start_ms: 2_000, text: "world & <friends>".into() },
                ],
            },
            CachedChunk { window_from: 1_200_000, to_ms: 2_400_000, segments: vec![] },
        ];
        save_cache(&path, &chunks);
        let back = load_cache(&path);
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].window_from, 0);
        assert_eq!(back[0].to_ms, 1_200_000);
        assert_eq!(back[0].segments.len(), 2);
        assert_eq!(back[0].segments[1].start_ms, 2_000);
        assert_eq!(back[0].segments[1].text, "world & <friends>");
        assert_eq!(back[1].window_from, 1_200_000);
        assert!(back[1].segments.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    fn provider(name: &str, url: &str) -> crate::settings::SttProviderConfig {
        crate::settings::SttProviderConfig { name: name.into(), url: url.into(), model: "m".into(), ..Default::default() }
    }

    #[test]
    fn stt_providers_use_their_own_keys_and_a_keyless_one_is_skipped() {
        let env = |k: &str| (k == "STT_KEY").then(|| "env-secret".to_string());
        let cfg = [
            crate::settings::SttProviderConfig {
                api_key: "lit".into(),
                fields: Some([("response_format".to_string(), "verbose_json".to_string())].into()),
                ..provider("inline", "https://api.example.com/a")
            },
            crate::settings::SttProviderConfig { api_key_env: "STT_KEY".into(), ..provider("fromenv", "https://api.example.com/b") },
            crate::settings::SttProviderConfig { api_key_env: "UNSET_KEY".into(), ..provider("nokey", "https://api.example.com/c") },
        ];
        let ps = parse_stt_providers(&cfg, env);
        assert_eq!(ps.len(), 2, "keyless provider dropped");
        assert_eq!(ps[0].name, "inline");
        assert_eq!(ps[0].key, "lit");
        assert_eq!(ps[1].name, "fromenv");
        assert_eq!(ps[1].key, "env-secret");
        assert_eq!(ps[1].fields, vec![("response_format".to_string(), "verbose_json".to_string())]);
    }

    #[test]
    fn a_riva_provider_keeps_its_function_id_and_sets_the_fan_out() {
        let cfg = [
            crate::settings::SttProviderConfig {
                kind: "riva".into(),
                api_key: "k".into(),
                function_id: "fid".into(),
                concurrency: Some(8),
                ..provider("riva", "https://api.example.com")
            },
            crate::settings::SttProviderConfig { api_key: "k".into(), ..provider("multipart", "https://api.example.com/x") },
        ];
        let ps = parse_stt_providers(&cfg, |_| None);
        assert_eq!(ps[0].kind, SttKind::Riva { function_id: "fid".into() });
        assert_eq!(ps[1].kind, SttKind::OpenAi);
        assert_eq!(stt_concurrency(&ps), 8, "the primary provider sets the transcription fan-out");
    }

    /// Live: transcribes every `.m4a` in `$MT_STT_TEST_DIR` through the configured chain.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore]
    async fn live_stt_chain_transcribes_saved_chunks() {
        let dir = std::env::var("MT_STT_TEST_DIR").expect("MT_STT_TEST_DIR");
        let mut files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "m4a"))
            .collect();
        files.sort();
        let (tx, _rx) = async_channel::unbounded();
        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(stt_concurrency(&stt_providers())));
        let t0 = std::time::Instant::now();
        let mut jobs = tokio::task::JoinSet::new();
        for f in files.clone() {
            let (sem, tx) = (sem.clone(), tx.clone());
            jobs.spawn(async move {
                let _p = sem.acquire_owned().await.unwrap();
                (f.clone(), transcribe(&std::fs::read(&f).unwrap(), 0, &tx).await)
            });
        }
        let mut words = 0;
        while let Some(r) = jobs.join_next().await {
            let (f, segs) = r.unwrap();
            let segs = segs.expect("chunk transcribed");
            let n: usize = segs.iter().map(|s| s.text.split_whitespace().count()).sum();
            tracing::info!("{} {n}", f.file_name().unwrap().to_string_lossy());
            words += n;
        }
        tracing::info!("{} chunks, {words} words in {:.1}s", files.len(), t0.elapsed().as_secs_f64());
    }

}
