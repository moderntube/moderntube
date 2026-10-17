//! Print the signed-in Home feed as the app loads it: one tab-separated row per item.
//! Usage: feed-probe [pages]   (uses the app's saved login from the keyring)

use innertube::SearchItem;

#[tokio::main]
async fn main() -> Result<(), String> {
    let pages: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let cookie = moderntube::auth::store::load().await.filter(|h| moderntube::auth::cookie_is_usable(h));
    eprintln!("[feed-probe] signed in: {}", cookie.is_some());
    let session = moderntube::auth::build_session(cookie.as_deref()).map_err(|e| e.to_string())?;
    let mut page = session.home().await.map_err(|e| e.to_string())?;
    let mut n = 0;
    for p in 0..pages {
        for item in &page.items {
            match item {
                SearchItem::Video { id, title, channel, views, published, duration, live, .. } => {
                    n += 1;
                    println!(
                        "{n}\t{id}\t{}\t{}\t{}\t{}\t{title}",
                        channel.as_deref().unwrap_or("-"),
                        if *live { "LIVE" } else { duration.as_deref().unwrap_or("-") },
                        views.as_deref().unwrap_or("-"),
                        published.as_deref().unwrap_or("-"),
                    );
                }
                other => println!("-\t{other:?}"),
            }
        }
        match (&page.continuation, p + 1 < pages) {
            (Some(c), true) => page = session.feed_continuation(c).await.map_err(|e| e.to_string())?,
            _ => break,
        }
    }
    Ok(())
}
