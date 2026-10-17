//! The catch-up summary from an OpenAI-compatible chat endpoint.
use super::{fmt_ts, CatchupSegment};

/// Where the summary comes from, built from [`crate::settings::Catchup`].
#[derive(Clone, Debug, PartialEq)]
pub struct SummarizerConfig {
    pub model: String,
    /// Base URLs tried in order until one answers.
    pub bases: Vec<String>,
    /// Name of the env var holding the API key.
    pub key_env: String,
    /// A second provider, tried with its own key and model when every base above fails.
    pub fallback: Option<Box<SummarizerConfig>>,
}

impl SummarizerConfig {
    /// Split the comma-separated `api_base` setting into the ordered base list.
    pub fn from_parts(model: &str, base: &str, key_env: &str) -> Self {
        SummarizerConfig {
            model: model.to_string(),
            bases: base
                .split(',')
                .map(|b| b.trim().trim_end_matches('/').to_string())
                .filter(|b| !b.is_empty())
                .collect(),
            key_env: key_env.to_string(),
            fallback: None,
        }
    }

    pub fn with_fallback(mut self, fallback: SummarizerConfig) -> Self {
        self.fallback = Some(Box::new(fallback));
        self
    }

    /// The configured summarizer and its fallback. An endpoint with no base URL is off.
    pub fn from_settings(c: &crate::settings::Catchup) -> Self {
        let part = |e: &crate::settings::Endpoint| Self::from_parts(&e.model, &e.api_base, &e.api_key_env);
        let (primary, fallback) = (part(&c.summarizer), part(&c.fallback));
        if fallback.bases.is_empty() {
            primary
        } else {
            primary.with_fallback(fallback)
        }
    }

    pub fn is_off(&self) -> bool {
        self.bases.is_empty()
    }
}

pub const SUMMARIZER_OFF: &str = "Catch me up is off: set a summarizer endpoint in Preferences";

/// Summarize transcript + chat into a TL;DR + `[h:mm:ss]`-anchored bullets, against
/// whichever OpenAI-compatible endpoint `cfg` names.
pub(super) async fn summarize(
    segments: &[CatchupSegment],
    chat: &[(i64, String, String)],
    from_ms: i64,
    to_ms: i64,
    cfg: &SummarizerConfig,
) -> Result<String, String> {
    let mut transcript = String::new();
    for s in segments {
        transcript.push_str(&format!("[{}] {}\n", fmt_ts(s.start_ms), s.text));
    }
    let mut chat_block = String::new();
    for (off, author, text) in chat.iter().take(400) {
        chat_block.push_str(&format!("[{}] {author}: {text}\n", fmt_ts(*off)));
    }
    let window = format!("[{}] .. [{}]", fmt_ts(from_ms), fmt_ts(to_ms));
    // The instruction to pick moments a viewer would be annoyed to have missed keeps the
    // bullets on concrete events instead of greetings and small talk.
    let system = "You summarize a live stream segment the viewer missed. Write: (1) a 2-3 sentence TL;DR paragraph; (2) 4-10 bullet points, each STARTING with a timestamp copied verbatim from the transcript markers, format `[h:mm:ss]` or `[m:ss]`, in chronological order. Use ONLY timestamps that appear in the transcript.\n\nPick the moments a viewer would actually be annoyed to have missed: surprising numbers, money/strategy insights, drama, reveals, things that went wrong, strong reactions. Include the SPECIFIC figures and names when the streamer says them. Do NOT spend a bullet on greetings, small talk, or 'the streamer begins reacting to a video' — describe what was actually IN it. If chat reactions add context, weave them in briefly. Plain text, no markdown headers; bullets start with '- '.";
    let user = format!(
        "Stream window {window}.\n\nTRANSCRIPT (timestamped):\n{transcript}\n\nCHAT ({} messages):\n{chat_block}",
        chat.len()
    );
    let messages = serde_json::json!([
        {"role": "system", "content": system},
        {"role": "user", "content": user},
    ]);
    chat_complete(cfg, messages).await
}

/// One chat completion against whichever OpenAI-compatible endpoint `cfg` names, trying
/// each base in order, then the fallback provider.
pub(crate) async fn chat_complete(cfg: &SummarizerConfig, messages: serde_json::Value) -> Result<String, String> {
    if cfg.is_off() {
        return Err(SUMMARIZER_OFF.to_string());
    }
    let mut failures = Vec::new();
    let mut next = Some(cfg);
    while let Some(c) = next {
        match complete_once(c, &messages).await {
            Ok(text) => return Ok(text),
            Err(e) => failures.push(e),
        }
        next = c.fallback.as_deref();
    }
    tracing::warn!("[llm] chat completion failed: {}", failures.join("; "));
    Err(format!("summarizer failed: {}", failures.join("; ")))
}

pub(super) async fn complete_once(cfg: &SummarizerConfig, messages: &serde_json::Value) -> Result<String, String> {
    let key = match cfg.key_env.as_str() {
        "" => None,
        var => Some(std::env::var(var).map_err(|_| format!("{var} not set (Preferences → Catch me up)"))?),
    };
    let body = serde_json::json!({ "model": cfg.model, "messages": messages });
    let client = reqwest::Client::new();
    let mut failures = Vec::new();
    for base in &cfg.bases {
        let mut req = client.post(format!("{base}/chat/completions"));
        if let Some(k) = &key {
            req = req.bearer_auth(k);
        }
        let resp = req
            .json(&body)
            .timeout(std::time::Duration::from_secs(180))
            .send()
            .await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{base}: {e}"));
                continue;
            }
        };
        let status = resp.status();
        let v = resp.json::<serde_json::Value>().await.unwrap_or_default();
        if let (true, Some(content)) = (status.is_success(), v["choices"][0]["message"]["content"].as_str()) {
            return Ok(content.trim().to_string());
        }
        let msg = v["message"].as_str().or_else(|| v["error"]["message"].as_str()).unwrap_or("no reply text");
        failures.push(format!("{base}: HTTP {} {msg}", status.as_u16()));
    }
    Err(failures.join("; "))
}
