//! Diagnostic log streams. Each is on when `RUST_LOG` enables its target at `debug`,
//! e.g. `RUST_LOG=info,moderntube::diag::soak=debug`.

macro_rules! diag_stream {
    ($(#[$doc:meta])* $name:ident, $target:literal) => {
        $(#[$doc])*
        pub fn $name() -> bool {
            tracing::enabled!(target: $target, tracing::Level::DEBUG)
        }
    };
}

diag_stream!(
    /// `[soak +Ns]` once a second in the format the soak verdict tools read.
    soak, "moderntube::diag::soak"
);
diag_stream!(
    /// `[avsync]` once a second while a video plays.
    avsync, "moderntube::diag::avsync"
);
diag_stream!(
    /// `[loop]` for each main-loop gap over 80 ms.
    heartbeat, "moderntube::diag::heartbeat"
);
diag_stream!(
    /// `[fps]` once a second from the window's frame clock.
    fps, "moderntube::diag::fps"
);
diag_stream!(
    /// Paint intervals and frame times of the Shorts view every 2 s.
    frame_stats, "moderntube::diag::frame_stats"
);
diag_stream!(
    /// `[gst-gap]` for any video pts step outside 30-37 ms (one frame at 30 fps).
    gap_strict, "moderntube::diag::gap_strict"
);
