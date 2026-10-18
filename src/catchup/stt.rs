//! Speech-to-text: the provider fallback chain and one transcription call each.
use super::{CatchupPhase, CatchupSegment};
use crate::settings::SttProviderConfig;

/// One resolved entry of the `[[catchup.stt]]` chain. `fields` is the per-provider
/// timestamp request, e.g. `response_format` or `timestamp_granularities`.
pub(super) struct SttProvider {
    pub(super) name: String,
    pub(super) kind: SttKind,
    pub(super) url: String,
    pub(super) model: String,
    pub(super) key: String,
    pub(super) fields: Vec<(String, String)>,
    /// Sub-windows transcribed at once when this provider heads the chain.
    pub(super) concurrency: usize,
}

#[derive(Debug, PartialEq)]
pub(super) enum SttKind {
    OpenAi,
    /// NVIDIA Riva `Recognize` over gRPC ([`crate::riva`]); `url` is the gRPC host.
    Riva { function_id: String },
}

const NO_STT: &str = "no transcription provider with a key: add one under [[catchup.stt]] in settings.toml";

/// How many sub-windows to transcribe at once: the head of the chain decides, since it
/// takes nearly all the traffic.
pub(super) fn stt_concurrency(providers: &[SttProvider]) -> usize {
    providers.first().map_or(1, |p| p.concurrency)
}

/// Resolve each provider's key through `key_for` (by provider name), dropping keyless
/// providers. `fields` defaults to `response_format=verbose_json`.
pub(super) fn parse_stt_providers(
    cfg: &[SttProviderConfig],
    key_for: impl Fn(&str) -> Option<String>,
) -> Vec<SttProvider> {
    cfg.iter()
        .filter_map(|p| {
            if p.url.is_empty() {
                return None;
            }
            let key = key_for(&p.name).filter(|s| !s.is_empty())?;
            let fields: Vec<(String, String)> = match &p.fields {
                Some(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
                None => vec![("response_format".into(), "verbose_json".into())],
            };
            let kind = match p.kind.as_str() {
                "riva" if p.function_id.is_empty() => return None,
                "riva" => SttKind::Riva { function_id: p.function_id.clone() },
                _ => SttKind::OpenAi,
            };
            Some(SttProvider {
                name: if p.name.is_empty() { "stt".to_string() } else { p.name.clone() },
                kind,
                url: p.url.clone(),
                model: p.model.clone(),
                key,
                fields,
                concurrency: p.concurrency.map_or(1, |c| c.clamp(1, 32) as usize),
            })
        })
        .collect()
}

/// The configured chain, keeping only providers with a resolvable key.
pub(super) async fn stt_providers() -> Vec<SttProvider> {
    let cfg = crate::settings::read(|s| s.catchup.stt.clone());
    let mut keys = std::collections::HashMap::new();
    for p in &cfg {
        if let Some(k) = crate::api_keys::load(&crate::api_keys::stt_slot(&p.name)).await {
            keys.insert(p.name.clone(), k);
        }
    }
    parse_stt_providers(&cfg, |name| keys.get(name).cloned())
}

/// One transcription POST. Returns `(status, retry_after_secs, body)`.
pub(super) async fn stt_post(
    client: &reqwest::Client,
    p: &SttProvider,
    m4a: &[u8],
) -> Result<(reqwest::StatusCode, Option<u64>, serde_json::Value), String> {
    let part = reqwest::multipart::Part::bytes(m4a.to_vec())
        .file_name("audio.m4a")
        .mime_str("audio/mp4")
        .map_err(|e| e.to_string())?;
    let mut form = reqwest::multipart::Form::new().part("file", part).text("model", p.model.clone());
    for (k, v) in &p.fields {
        form = form.text(k.clone(), v.clone());
    }
    let resp = client
        .post(&p.url)
        .bearer_auth(&p.key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("{}: {e}", p.name))?;
    let status = resp.status();
    let retry_after = resp
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.trim().parse::<f64>().ok())
        .map(|s| (s.ceil() as u64).clamp(1, 300));
    let v = resp.json().await.map_err(|e| format!("{} body: {e}", p.name))?;
    Ok((status, retry_after, v))
}

/// One Riva call; `ogg_opus` holds the transcode across retries of the same chunk.
pub(super) async fn riva_transcribe(
    p: &SttProvider,
    function_id: &str,
    m4a: &[u8],
    ogg_opus: &mut Option<Vec<u8>>,
) -> Result<Vec<(i64, String)>, crate::riva::RivaError> {
    use crate::riva::{self, RivaError};
    let audio = match ogg_opus {
        Some(a) => a.clone(),
        None => {
            let a = riva::to_ogg_opus(m4a.to_vec()).await.map_err(RivaError::Other)?;
            *ogg_opus = Some(a.clone());
            a
        }
    };
    let client = riva::client().map_err(RivaError::Other)?;
    let resp = riva::recognize(&client, &p.url, function_id, &p.key, audio).await?;
    Ok(riva::segments(&resp))
}

/// Transcribes one m4a sub-window into stream-absolute segments by walking the provider
/// chain. A 429 moves on to the next provider at once (separate quota). When every provider
/// is rate-limited, it waits the shortest `retry-after`, reports the wait as `RateLimited`
/// and retries. Earlier chunks are already cached, so a hard failure loses nothing.
pub(super) async fn transcribe(
    m4a: &[u8],
    base_ms: i64,
    progress: &async_channel::Sender<CatchupPhase>,
) -> Result<Vec<CatchupSegment>, String> {
    const MAX_RETRIES: u32 = 5;
    let providers = stt_providers().await;
    if providers.is_empty() {
        return Err(NO_STT.to_string());
    }
    let client = reqwest::Client::new();
    let mut ogg_opus: Option<Vec<u8>> = None;
    let mut attempt = 0u32;
    loop {
        let mut all_rate_limited = true;
        let mut min_retry: Option<u64> = None;
        let mut last_err = String::new();
        for p in &providers {
            if let SttKind::Riva { function_id } = &p.kind {
                match riva_transcribe(p, function_id, m4a, &mut ogg_opus).await {
                    Ok(segs) => {
                        return Ok(segs
                            .into_iter()
                            .map(|(start, text)| CatchupSegment { start_ms: base_ms + start, text })
                            .collect())
                    }
                    Err(crate::riva::RivaError::RateLimited) => {
                        tracing::info!("[catchup] {} rate-limited", p.name);
                        last_err = format!("{} rate-limited", p.name);
                        min_retry = Some(min_retry.map_or(30, |m| m.min(30)));
                    }
                    Err(e) => {
                        all_rate_limited = false;
                        tracing::info!("[catchup] {}: {e}", p.name);
                        last_err = format!("{}: {e}", p.name);
                    }
                }
                continue;
            }
            match stt_post(&client, p, m4a).await {
                Ok((status, _, v)) if status.is_success() => {
                    let mut out = Vec::new();
                    for seg in v["segments"].as_array().into_iter().flatten() {
                        let text = seg["text"].as_str().unwrap_or("").trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        let start = (seg["start"].as_f64().unwrap_or(0.0) * 1000.0) as i64;
                        out.push(CatchupSegment { start_ms: base_ms + start, text });
                    }
                    return Ok(out);
                }
                Ok((status, retry_after, _)) if status.as_u16() == 429 => {
                    last_err = format!("{} rate-limited", p.name);
                    let ra = retry_after.unwrap_or(60);
                    min_retry = Some(min_retry.map_or(ra, |m| m.min(ra)));
                    // try the next provider's independent quota before waiting
                }
                Ok((status, _, v)) => {
                    all_rate_limited = false;
                    last_err = format!("{} {status}: {}", p.name, v["error"]["message"].as_str().unwrap_or("?"));
                }
                Err(e) => {
                    all_rate_limited = false;
                    last_err = e;
                }
            }
        }
        // Every provider failed this round. If it was purely rate limits, wait out the
        // shortest refill and retry the whole chain; otherwise give up (cache intact).
        if all_rate_limited && attempt < MAX_RETRIES {
            attempt += 1;
            let wait = min_retry.unwrap_or(60);
            let _ = progress.try_send(CatchupPhase::RateLimited(wait));
            tokio::time::sleep(std::time::Duration::from_secs(wait + 1)).await;
            continue;
        }
        return Err(format!("{last_err} (transcript cached — Retry to resume)"));
    }
}
