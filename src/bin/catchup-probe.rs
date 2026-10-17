//! Headless probe for the catch-up pipeline: audio window → STT chain → chat → summary.
//! Usage: catchup-probe <video_id> <from_ms> [to_ms]   (needs bgutil :4416 + keys in env)

use moderntube::catchup::{run_catchup, CatchupPhase};
use moderntube::prefs::Prefs;

#[tokio::main]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let video_id = args.next().expect("usage: catchup-probe <video_id> <from_ms> [to_ms]");
    let mut from_ms: i64 = args.next().expect("from_ms").parse().expect("from_ms: integer ms");
    let to_ms: Option<i64> = args.next().map(|s| s.parse().expect("to_ms: integer ms"));
    // A negative from_ms is an offset back from the live head.
    if from_ms < 0 {
        let session = innertube::Session::new(innertube::ClientConfig::web()).map_err(|e| e.to_string())?;
        let pot = innertube::pot_provider(
            innertube::PotBackendKind::parse(&Prefs::load().pot_backend).unwrap_or_default(),
            innertube::WEB_USER_AGENT,
        );
        let src = innertube::open_audio_source(&session, &innertube::NSigSolver::new(), &pot, &video_id)
            .await
            .map_err(|e| e.to_string())?;
        let head = match innertube::live_head_ms(&src, 0).await.map_err(|e| e.to_string())? {
            Some(h) => h,
            None => {
                eprintln!("[probe] no head at player_time 0; asking at the live-edge sentinel");
                innertube::live_head_ms(&src, (1 << 53) - 1)
                    .await
                    .map_err(|e| e.to_string())?
                    .ok_or("no live head")?
            }
        };
        from_ms += head;
        eprintln!("[probe] live head {head} ms, window starts at {from_ms} ms");
    }

    let (tx, rx) = async_channel::unbounded::<CatchupPhase>();
    let printer = tokio::spawn(async move {
        while let Ok(p) = rx.recv().await {
            eprintln!("[phase] {p:?}");
        }
    });
    // Same summarizer settings the app uses, so a probe measures the real path.
    let summarizer =
        moderntube::settings::read(|s| moderntube::catchup::SummarizerConfig::from_settings(&s.catchup));
    eprintln!("[probe] summarizer: {} via {}", summarizer.model, summarizer.bases.join(", "));
    let res = run_catchup(video_id, from_ms, to_ms, tx, summarizer).await?;
    printer.abort();

    eprintln!(
        "\n== window [{}..{}] — {} transcript segments, {} chat messages ==",
        res.from_ms,
        res.to_ms,
        res.segments.len(),
        res.chat_count
    );
    for s in res.segments.iter().take(5) {
        eprintln!("  [{}] {}", moderntube::catchup::fmt_ts(s.start_ms), s.text);
    }
    println!("\n===== SUMMARY =====\n{}", res.summary);
    Ok(())
}
