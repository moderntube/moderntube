use adw::prelude::*;
use gtk4 as gtk;
use moderntube::runtime::AsyncRuntime;
use moderntube::window;

const DEFAULT_FILTER: &str = "info,innertube=debug,innertube_pot=debug,innertube_js=debug,innertube_sabr=debug";

/// `$XDG_STATE_HOME/moderntube/moderntube.log`, or under `~/.local/state`.
fn log_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/state")))?;
    Some(base.join("moderntube").join("moderntube.log"))
}

/// The log file, truncated, readable by the owner only.
fn open_log() -> Option<std::fs::File> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let path = log_path()?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(path.parent()?).ok()?;
    let f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path).ok()?;
    // `mode` applies only when the file is created.
    f.set_permissions(std::fs::Permissions::from_mode(0o600)).ok()?;
    Some(f)
}

/// Bare messages to stderr, and to [`log_path`] when stderr is not a terminal.
/// `RUST_LOG` replaces [`DEFAULT_FILTER`].
fn setup_logging() {
    use std::io::IsTerminal;
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    moderntube::uptime_ms();
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(DEFAULT_FILTER));
    let fmt = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false)
        .with_target(false)
        .with_level(false)
        .without_time();
    // The log carries search queries, video ids and the account name.
    let file = (!std::io::stderr().is_terminal()).then(open_log).flatten();
    match file {
        Some(f) => fmt.with_writer(std::io::stderr.and(std::sync::Mutex::new(f))).init(),
        None => fmt.with_writer(std::io::stderr).init(),
    }
}

fn main() -> gtk::glib::ExitCode {
    setup_logging();
    moderntube::settings::init();
    let app = adw::Application::builder().application_id("io.github.moderntube.ModernTube").build();
    moderntube::hooks::install(&app);
    app.connect_activate(|app| {
        let rt = AsyncRuntime::new().expect("tokio runtime");
        let window = window::build(app, rt.clone());
        // The player build is the same for every video, so it is fetched before the first click.
        moderntube::playback::prewarm(&rt);
        window.present();
    });
    app.run()
}
