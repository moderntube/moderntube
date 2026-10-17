//! SponsorBlock: crowd-sourced skip segments for a video. Pure data plus one HTTP fetch; the
//! skipping itself lives in the window's playback poll.

use serde_json::Value;

/// A skippable span `[start, end]` (seconds) plus its SponsorBlock category.
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub start: f64,
    pub end: f64,
    pub category: String,
}

/// All SponsorBlock categories we support, with their display label. Order = settings-menu
/// order. The default enabled set is sponsors only (`default_categories`).
pub const ALL_CATEGORIES: &[(&str, &str)] = &[
    ("sponsor", "Sponsor"),
    ("selfpromo", "Unpaid/self-promo"),
    ("interaction", "Interaction reminder"),
    ("intro", "Intro / intermission"),
    ("outro", "Endcards / credits"),
    ("preview", "Preview / recap"),
    ("music_offtopic", "Non-music section"),
];

/// Default enabled categories (most viewers only want sponsors).
pub fn default_categories() -> Vec<String> {
    vec!["sponsor".to_string()]
}

/// The SponsorBlock-extension color for a category, as RGB in 0..1. Falls back to grey.
pub fn category_color(category: &str) -> (f64, f64, f64) {
    match category {
        "sponsor" => (0.0, 0.83, 0.0),         // green   #00d400
        "selfpromo" => (1.0, 1.0, 0.0),        // yellow  #ffff00
        "interaction" => (0.8, 0.0, 1.0),      // purple  #cc00ff
        "intro" => (0.0, 1.0, 1.0),            // cyan    #00ffff
        "outro" => (0.01, 0.01, 0.93),         // blue    #0202ed
        "preview" => (0.0, 0.56, 0.84),        // l-blue  #008fd6
        "music_offtopic" => (1.0, 0.6, 0.0),   // orange  #ff9900
        _ => (0.5, 0.5, 0.5),
    }
}

/// Fetch skip segments for `video_id` in the given `categories` from the SponsorBlock API.
/// Best-effort: returns an empty list on any error (incl. 404 = no segments submitted) or
/// when `categories` is empty.
pub async fn fetch_segments(video_id: &str, categories: &[String]) -> Vec<Segment> {
    if categories.is_empty() {
        return Vec::new();
    }
    let cats_json = serde_json::to_string(categories).unwrap_or_else(|_| "[\"sponsor\"]".to_string());
    let resp = reqwest::Client::new()
        .get("https://sponsor.ajay.app/api/skipSegments")
        .query(&[("videoID", video_id), ("categories", &cats_json)])
        .send()
        .await;
    let Ok(resp) = resp else { return Vec::new() };
    if !resp.status().is_success() {
        return Vec::new();
    }
    let Ok(text) = resp.text().await else { return Vec::new() };
    parse_segments(&text)
}

/// Parse the SponsorBlock `skipSegments` JSON: `[{"category":..,"segment":[s,e]},..]`.
fn parse_segments(json: &str) -> Vec<Segment> {
    let Ok(Value::Array(arr)) = serde_json::from_str::<Value>(json) else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|s| {
            let seg = s.get("segment")?.as_array()?;
            let start = seg.first()?.as_f64()?;
            let end = seg.get(1)?.as_f64()?;
            let category = s.get("category")?.as_str()?.to_string();
            (end > start).then_some(Segment { start, end, category })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_segments;

    #[test]
    fn segments_parse_from_the_api_response() {
        let json = r#"[
            {"category":"sponsor","segment":[12.5,30.0],"UUID":"x"},
            {"category":"intro","segment":[0.0,5.0]}
        ]"#;
        let segs = parse_segments(json);
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].category, "sponsor");
        assert_eq!(segs[0].start, 12.5);
        assert_eq!(segs[0].end, 30.0);
        assert_eq!(segs[1].category, "intro");
    }

    #[test]
    fn malformed_responses_and_empty_segments_yield_nothing() {
        assert!(parse_segments("not json").is_empty());
        assert!(parse_segments("{}").is_empty());
        // zero-length / reversed segments dropped
        assert!(parse_segments(r#"[{"category":"sponsor","segment":[10.0,10.0]}]"#).is_empty());
    }
}
