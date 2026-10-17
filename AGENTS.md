# AGENTS.md

Rules for anyone changing this repository, human or coding agent. `DESIGN.md` describes
the architecture.

## What lives where

moderntube is the user interface. Everything that knows about YouTube (InnerTube calls,
authentication, SABR streaming, n-sig, PO tokens) lives in
[innertube-rs](https://github.com/moderntube/innertube-rs). If a change needs
protocol knowledge, it belongs there. This repository holds presentation, navigation,
local persistence and the GStreamer playback substrate.

The library's rule applies here too: behave like YouTube's web client. The one sanctioned
difference is the display substrate. The web player appends segments to MSE; this app
feeds them to GStreamer, and code in `src/playback/gst/` that exists for that reason says
so. Do not mask a problem by lowering quality, adding delay or retrying until it works.

## Verifying a change

1. **A log line is not evidence for anything visual.** Blank, flashing, offset or frozen:
   each needs a screenshot that someone has looked at. A file size or a grep hit is a lead.
2. **Drive the real app.** The test hooks (`moderntube --help`) open videos, seek, switch
   quality and change speed from the command line or as `app.<name>` actions over D-Bus,
   so a harness can run a scenario. Judge the result on the picture.
3. **Test the binary you built.** An unchanged frame after a fix often means an old binary
   ran.
4. **Control local state.** The profile keeps resume positions, a stored quality cap and a
   signed-in session across runs. A stored resume position puts an open on the seek path.
   Clear or record that state before you measure.
5. **Swap the order.** Two videos opened in a fixed order cannot separate "second open"
   from "that video".
6. **Find the mechanism first.** Read the library's captures and the GStreamer source
   before you change one variable at a time against YouTube. Drive a run to confirm a
   located mechanism.
7. **Keep live traffic small.** Use a throwaway, signed-out profile where you can, and
   space opens out. Bursts from one address get throttled.
8. **Never print secrets.** The keyring holds session cookies. Avoid commands that print
   a credential by default, and keep cookies out of logs, issues and commits.

## Build and test

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings   # must be clean
```

`README.md` lists the system libraries. To build against a local innertube-rs checkout,
add a `[patch]` section for the innertube-rs git URL to `Cargo.toml` that points each
`innertube*` crate at the checkout. Without it, `cargo` builds the pinned revision and
your library changes do not run.

`cargo test` covers logic that runs without a display: preferences and settings, the
queue, the substrate's timing helpers, the WSOLA time stretch, parsers. Playback and
rendering are checked by driving the app (see above).

## Traps

- **The parse appsink asks for AV1 `alignment=tu`.** With frame alignment, `decodebin3`
  adds a second `av1parse` that corrupts timestamps and freezes video every few seconds.
- **The pump reads `seek_gen` after `pull_sample()` returns.** Read before the blocking
  pull, a seek that lands during the wait makes the first post-seek keyframe look stale,
  and a live DVR seek never resyncs.
- **A seek after a track's feed ended restarts that track** (`start_track`). The VOD
  feed exits at the end of the video, and without the restart the picture freezes at the
  seek target.
- **Every async UI request needs its own `Generation`.** Bump it per request and drop late
  results whose token is stale, or a slow older reply overwrites a newer one.

## Conventions

- Comments follow innertube-rs's [comment style](https://github.com/moderntube/innertube-rs/blob/main/docs/comment-style.md); tests
  follow its [test style](https://github.com/moderntube/innertube-rs/blob/main/docs/test-style.md).
- `snake_case` modules, one widget per file in `src/widgets/`.
- Logging goes through `tracing`. Diagnostic streams are targets under
  `moderntube::diag::*` at `debug`, for example
  `RUST_LOG=info,moderntube::diag::fps=debug`.
- Conventional commits, one kind of change per commit.
