//! What both playback substrates share: the streaming deps, the warm-up, errors.
pub mod events;
pub mod live;
pub mod quality;

/// Rung for every hover preview. It never changes the user's own quality choice.
pub const PREVIEW_QUALITY: innertube::Quality = innertube::Quality::Height(240);

/// A PO-token minter, shared by every open that uses it.
pub(crate) type Pot = std::sync::Arc<dyn innertube::PoTokenProvider>;

static POT: std::sync::RwLock<Option<(innertube::PotBackendKind, Pot)>> = std::sync::RwLock::new(None);

fn new_pot(kind: innertube::PotBackendKind) -> Pot {
    innertube::pot_provider(kind, innertube::WEB_USER_AGENT)
}

/// The minter kind the `pot_backend` setting chose.
pub(crate) fn pot_kind() -> innertube::PotBackendKind {
    if let Some((kind, _)) = POT.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return *kind;
    }
    innertube::PotBackendKind::parse(&crate::prefs::Prefs::load().pot_backend).unwrap_or_default()
}

/// The PO-token minter every open shares, chosen by the `pot_backend` setting. A native
/// minter holds one attestation for its TTL, so it must be shared to be cheap.
pub(crate) fn pot_backend() -> Pot {
    if let Some((_, p)) = POT.read().unwrap_or_else(|e| e.into_inner()).as_ref() {
        return p.clone();
    }
    let kind = pot_kind();
    POT.write().unwrap_or_else(|e| e.into_inner()).get_or_insert_with(|| (kind, new_pot(kind))).1.clone()
}

/// Swap the minter; opens already running keep the one they started with.
pub(crate) fn set_pot_backend(kind: innertube::PotBackendKind) {
    let mut pot = POT.write().unwrap_or_else(|e| e.into_inner());
    if pot.as_ref().map(|(k, _)| *k) != Some(kind) {
        tracing::info!("[pot] minter -> {}", kind.as_str());
        *pot = Some((kind, new_pot(kind)));
    }
}

static ACCOUNT: std::sync::RwLock<Option<(innertube::Session, Pot)>> =
    std::sync::RwLock::new(None);

/// The signed-in session videos stream on. It has its own minter because an attestation is
/// bound to the session whose page it came from (POT-003). `None` streams anonymously.
pub(crate) fn set_playback_account(session: Option<&innertube::Session>) {
    let mut account = ACCOUNT.write().unwrap_or_else(|e| e.into_inner());
    let session = session.filter(|s| s.is_authenticated());
    if account.as_ref().map(|(s, _)| s.id()) == session.map(|s| s.id()) {
        return;
    }
    tracing::info!("[deps] playback session -> {}", if session.is_some() { "signed in" } else { "anonymous" });
    *account = session.map(|s| (s.clone(), new_pot(pot_kind())));
}

fn account_deps() -> Option<(innertube::Session, Pot)> {
    ACCOUNT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

static DIRECT_OPENS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// The "direct opens" setting, applied to every session handed out from here on.
pub(crate) fn set_direct_opens(on: bool) {
    DIRECT_OPENS.store(on, std::sync::atomic::Ordering::Relaxed);
}

fn direct_opens() -> bool {
    DIRECT_OPENS.load(std::sync::atomic::Ordering::Relaxed)
}

/// The streaming deps every engine and hover preview shares: one `Session` (visitorData,
/// connection pool), the disk-cached `NSigSolver`, and the [`pot_backend`] minter.
pub(crate) fn shared_deps(
) -> Result<(innertube::Session, innertube::NSigSolver, Pot), PlaybackError> {
    use innertube::{ClientConfig, NSigSolver, Session};
    static DEPS: std::sync::OnceLock<(Session, NSigSolver)> = std::sync::OnceLock::new();
    let base = match DEPS.get() {
        Some(d) => d.clone(),
        None => {
            let fresh = (Session::new(ClientConfig::web())?, NSigSolver::new());
            DEPS.get_or_init(|| fresh).clone()
        }
    };
    base.0.set_direct_opens(direct_opens());
    if let Some((session, pot)) = account_deps() {
        session.set_direct_opens(direct_opens());
        return Ok((session, base.1, pot));
    }
    Ok((base.0, base.1, pot_backend()))
}

/// The deps for a click's main open. With direct opens on, the open is a new visitor with
/// its own `Session` and minter: the content-ads hold follows a visitor that has loaded the
/// homepage (GATE-009), and an attestation is bound to the page it came from (POT-003).
pub(crate) fn open_deps(
) -> Result<(innertube::Session, innertube::NSigSolver, Pot), PlaybackError> {
    let shared = shared_deps()?;
    if !direct_opens() || account_deps().is_some() {
        return Ok(shared);
    }
    tracing::info!("[deps] direct open: new visitor and minter for this open");
    let pot = new_pot(pot_kind());
    let session = innertube::Session::new(innertube::ClientConfig::web())?;
    session.set_direct_opens(true);
    Ok((session, shared.1, pot))
}

/// Warms the video-independent half of a cold start while the user browses the feed. See
/// `innertube::prewarm_session`: `base.js`, the first n-sig solve and `visitorData` depend on
/// the player build and the session, not the video. Failure is silent; the bootstrap fetches
/// whatever is missing.
pub fn prewarm(rt: &crate::runtime::AsyncRuntime) {
    let Ok((session, solver, _pot)) = shared_deps() else { return };
    rt.spawn_bg(async move {
        // The home feed gets the link first: on a slow link the ~2.5 MB base.js would delay it.
        tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        let t = std::time::Instant::now();
        match innertube::prewarm_session(&session, &solver).await {
            Some(url) => {
                tracing::info!("[prewarm] player resident in {}ms ({url})", t.elapsed().as_millis())
            }
            None => tracing::info!("[prewarm] warm-up did not complete; a click pays as before"),
        }
    });
}

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error(transparent)]
    Session(#[from] innertube::Error),
    #[error(transparent)]
    Stream(#[from] innertube::StreamError),
}

/// A stream failure's class as toast text; the detail belongs in the log.
pub fn failure_class(e: &innertube::StreamError) -> &'static str {
    use innertube::StreamErrorKind::*;
    match e.kind() {
        Network => "Network error",
        Refused => "YouTube refused playback",
        Unavailable => "Video unavailable",
        Internal => "Internal error",
    }
}

/// The toast for a stream that gave up mid-playback. Logs the detail at warn.
pub fn stopped_message(e: &innertube::StreamError) -> String {
    tracing::warn!("[playback] stream failed: {e}");
    format!("Playback stopped: {}", failure_class(e))
}
