//! A video's transcript as timestamped lines, and the questions asked about it.
//!
//! A VOD's transcript comes from its caption track; a live stream's from the catch-up STT
//! pipeline, whose timestamps are stream-absolute.

use crate::catchup::{chat_complete, fmt_ts, SummarizerConfig};

/// Caption cues are 1-3 s fragments; lines this long keep timestamps useful and tokens low.
const LINE_SPAN_MS: i64 = 20_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub start_ms: i64,
    pub text: String,
}

/// Merge caption cues into lines of at most [`LINE_SPAN_MS`], whitespace collapsed.
pub fn from_captions(cues: &[innertube::Caption]) -> Vec<Line> {
    merge(cues.iter().map(|c| (c.start_ms, c.text.as_str())))
}

/// The same, for a live stream's speech-to-text segments.
pub fn from_stt(segments: &[crate::catchup::CatchupSegment]) -> Vec<Line> {
    merge(segments.iter().map(|s| (s.start_ms, s.text.as_str())))
}

fn merge<'a>(items: impl Iterator<Item = (i64, &'a str)>) -> Vec<Line> {
    let mut lines: Vec<Line> = Vec::new();
    for (start_ms, raw) in items {
        let text = raw.split_whitespace().collect::<Vec<_>>().join(" ");
        if text.is_empty() {
            continue;
        }
        match lines.last_mut() {
            Some(last) if start_ms - last.start_ms < LINE_SPAN_MS => {
                last.text.push(' ');
                last.text.push_str(&text);
            }
            _ => lines.push(Line { start_ms, text }),
        }
    }
    lines
}

/// Indices of the lines containing every word of `query`, case-insensitively.
pub fn search(lines: &[Line], query: &str) -> Vec<usize> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Vec::new();
    }
    lines
        .iter()
        .enumerate()
        .filter(|(_, l)| {
            let t = l.text.to_lowercase();
            words.iter().all(|w| t.contains(w.as_str()))
        })
        .map(|(i, _)| i)
        .collect()
}

/// `[m:ss] text` per line, the form the model reads and cites back.
pub fn render(lines: &[Line]) -> String {
    let mut out = String::new();
    for l in lines {
        out.push_str(&format!("[{}] {}\n", fmt_ts(l.start_ms), l.text));
    }
    out
}

/// The chat messages for one question: transcript as context, earlier turns, then `question`.
pub fn ask_messages(title: &str, lines: &[Line], history: &[(String, String)], question: &str) -> serde_json::Value {
    let system = "You answer questions about a video using ONLY its timestamped transcript. \
        Cite the moment(s) you rely on with timestamps copied verbatim from the transcript markers, \
        format `[h:mm:ss]` or `[m:ss]`. For 'when' questions, lead with the timestamp. \
        If the transcript does not contain the answer, say so plainly and do not guess. \
        Be brief: a few sentences or a short list. Plain text, no markdown headers.";
    let context = format!("VIDEO: {title}\n\nTRANSCRIPT:\n{}", render(lines));
    let mut messages = vec![
        serde_json::json!({"role": "system", "content": system}),
        serde_json::json!({"role": "user", "content": context}),
        serde_json::json!({"role": "assistant", "content": "I have the transcript. What would you like to know?"}),
    ];
    for (q, a) in history {
        messages.push(serde_json::json!({"role": "user", "content": q}));
        messages.push(serde_json::json!({"role": "assistant", "content": a}));
    }
    messages.push(serde_json::json!({"role": "user", "content": question}));
    serde_json::Value::Array(messages)
}

/// Answer `question` from the transcript, against the catch-up summarizer endpoint.
pub async fn ask(
    title: String,
    lines: Vec<Line>,
    history: Vec<(String, String)>,
    question: String,
    cfg: SummarizerConfig,
) -> Result<String, String> {
    chat_complete(&cfg, ask_messages(&title, &lines, &history, &question)).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cue(start_ms: i64, text: &str) -> innertube::Caption {
        innertube::Caption { start_ms, dur_ms: 2_000, text: text.to_string() }
    }

    #[test]
    fn merging_cues_keeps_every_word_in_order_and_start_times_ascend() {
        let cues = [
            cue(0, "hello"),
            cue(2_000, "there\nfriends"),
            cue(19_999, "still line one"),
            cue(20_000, "line two"),
            cue(21_000, "   "),
            cue(95_000, "much later"),
        ];
        let lines = from_captions(&cues);
        let words = |texts: Vec<&str>| texts.join(" ").split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            words(lines.iter().map(|l| l.text.as_str()).collect()),
            words(cues.iter().map(|c| c.text.as_str()).collect())
        );
        assert!(lines.windows(2).all(|w| w[0].start_ms < w[1].start_ms));
        assert!(lines.len() < cues.len());
    }

    #[test]
    fn search_needs_every_word_in_any_case() {
        let lines = vec![
            Line { start_ms: 0, text: "The Farm broke overnight".into() },
            Line { start_ms: 20_000, text: "the farm is fine".into() },
        ];
        assert_eq!(search(&lines, "farm BROKE"), vec![0]);
        assert_eq!(search(&lines, "farm"), vec![0, 1]);
        assert!(search(&lines, "   ").is_empty());
    }

    #[test]
    fn the_question_is_the_last_message_and_transcript_and_history_are_sent() {
        let lines = vec![Line { start_ms: 65_000, text: "the reveal".into() }];
        let history = vec![("q1".to_string(), "a1".to_string())];
        let m = ask_messages("Title", &lines, &history, "when was the reveal?");
        let m = m.as_array().unwrap();
        let all = serde_json::to_string(&m).unwrap();
        for part in ["the reveal", "q1", "a1"] {
            assert!(all.contains(part), "{part} missing from the request");
        }
        assert_eq!(m.last().unwrap()["content"], "when was the reveal?");
    }
}
