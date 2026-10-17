//! Does a watch register in YouTube history? Reads FEhistory, drives `innertube::WatchReport`
//! with a simulated playhead (play 0..`secs`, seek to `seek_to`, play 40 s more, leave), then
//! reads FEhistory again.
//! Usage: watched-probe <video_id> [secs] [seek_to] [--dry]   (uses the app's saved login)

use std::time::Duration;

fn video_ids(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(m) => {
            for key in ["videoId", "contentId"] {
                if let Some(id) = m.get(key).and_then(|x| x.as_str()) {
                    if id.len() == 11 && !out.iter().any(|o| o == id) {
                        out.push(id.to_string());
                    }
                }
            }
            m.values().for_each(|x| video_ids(x, out));
        }
        serde_json::Value::Array(a) => a.iter().for_each(|x| video_ids(x, out)),
        _ => {}
    }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().filter(|a| a != "--dry").collect();
    let dry = std::env::args().any(|a| a == "--dry");
    let id = args.get(1).expect("video id").clone();
    let secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(60);
    let seek_to: Option<u64> = args.get(3).and_then(|s| s.parse().ok());
    let cookie = moderntube::auth::store::load().await.filter(|h| moderntube::auth::cookie_is_usable(h));
    let session = moderntube::auth::build_session(cookie.as_deref()).map_err(|e| e.to_string())?;
    eprintln!("[watched] signed in: {}", session.is_authenticated());

    let history = |label: &'static str| {
        let session = session.clone();
        let id = id.clone();
        async move {
            let v = session.browse("FEhistory").await.map_err(|e| e.to_string())?;
            let mut ids = Vec::new();
            video_ids(&v, &mut ids);
            let pos = ids.iter().position(|x| *x == id);
            let text = v.to_string();
            let watched = text.find(id.as_str()).and_then(|i| {
                let rest = &text[i..];
                let rest = &rest[..rest.len().min(20_000)];
                let key = ["\"percentDurationWatched\":", "\"startPercent\":"].into_iter().find(|k| rest.contains(k))?;
                let p = rest.find(key)? + key.len();
                Some(rest[p..].chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect::<String>())
            });
            eprintln!(
                "[watched] {label}: {} ids in history, target at {:?} watched {:?}%, first 5 {:?}",
                ids.len(),
                pos,
                watched,
                &ids[..ids.len().min(5)]
            );
            Ok::<_, String>(())
        }
    };
    history("before").await?;
    if dry {
        return Ok(());
    }

    let (report, task) = innertube::WatchReport::new(session.clone(), &id);
    let task = tokio::spawn(task);
    let play = |from: u64, n: u64| {
        let report = &report;
        async move {
            for t in from..=from + n {
                report.update(t as f64, true);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    };
    play(0, secs).await;
    if let Some(to) = seek_to {
        play(to, 40).await;
    }
    drop(report);
    let _ = task.await;
    tokio::time::sleep(Duration::from_secs(10)).await;
    history("after").await
}
