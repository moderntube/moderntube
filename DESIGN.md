# Design

moderntube is a GTK4 and libadwaita YouTube client. It is the user interface over
[innertube-rs](https://github.com/moderntube/innertube-rs), which holds everything
that talks to YouTube: the InnerTube API, sign-in headers, SABR streaming, n-sig and PO
tokens. This app holds presentation, navigation, local persistence and the playback
substrate. Protocol facts belong in innertube-rs's
[protocol register](https://github.com/moderntube/innertube-rs/tree/main/docs/protocol).

## Layout

| Path | Role |
|---|---|
| `src/main.rs` | Logging, settings, the `adw::Application`, the test hooks, then `window::build` |
| `src/window/` | The main window: the shared `App` state and the controllers that wire it |
| `src/widgets/` | One widget per file: results grid, cards, player view and controls, watch info, comments, live chat, Shorts, channel and playlist pages, nav rail, side panels |
| `src/playback/` | The `Player` interface, the GStreamer substrate, hover previews, the watch queue |
| `src/runtime/` | The bridge between the GLib main loop and tokio |
| `src/auth/` | Sign-in through an embedded WebKitGTK window, cookie storage, building the `Session` |
| `src/settings.rs`, `src/prefs/` | User settings (TOML) and UI state (small per-key files) |
| `src/catchup/`, `src/transcript.rs`, `src/riva.rs` | The opt-in "Catch me up" and Ask features |
| `src/mpris.rs`, `src/sponsorblock.rs`, `src/theme.rs`, `src/emoji.rs` | Self-contained integrations |
| `src/hooks.rs` | Test hooks: app actions a harness can drive |
| `src/bin/` | Development probes and offline checks |

## Window: state and controllers

`window::build` creates one `App` (`src/window/app.rs`), the window's shared state: the
`Session`, the player, the widgets, the queue, the current video and the per-request
generations. It is an `Rc<App>` that every callback captures.

The other modules in `src/window/` are controllers. Each one wires one concern onto
`App`:

| Module | Concern |
|---|---|
| `pages.rs` | Home and subscription feeds, the results grid, channel and playlist pages, card actions |
| `search.rs` | Search boxes with suggestions, infinite scroll of results |
| `open.rs`, `watch.rs`, `player_page.rs` | Opening a video, the watch page, comments paging, the sidebar |
| `stage.rs`, `nav.rs` | The video surface in its slot, the corner miniplayer or fullscreen; leaving the player |
| `live.rs`, `chat.rs`, `quality.rs` | The live-edge button, live chat, the quality menu |
| `catchup.rs`, `transcript.rs` | Catch me up, Ask and Transcribe |
| `auth.rs` | Sign in and out, restoring the stored session |
| `media_keys.rs` | MPRIS controls |
| `settings.rs`, `preferences.rs`, `appearance.rs`, `sponsor.rs`, `shortcuts.rs` | Settings popover, Preferences window, theming, SponsorBlock, keyboard shortcuts |
| `hooks.rs`, `probes.rs` | Startup test hooks and diagnostic probes |

The window is a `GtkStack` of pages (`results`, `channel`, `playlist`, `player`,
`shorts`). One app-lifetime `Player` serves every video. Opening a video prepares it off
the main thread, loads it on that player and switches the stack.

## GLib and tokio

innertube-rs is async on tokio. `runtime::AsyncRuntime` runs a tokio runtime beside the
GLib main loop. A handler spawns the future there and receives the result on the main
thread through `async-channel` and `glib::spawn_future_local`.

Replies can arrive out of order. Each kind of request (search, open, chat, comments) has a
`Generation`: a counter bumped per request. A result whose token is no longer current is
dropped, so a slow older reply never overwrites a newer one.

## Playback

`src/playback/player.rs` is the only interface the rest of the app uses: `prepare`,
`prepare_vod` and `prepare_live` open a video off the main thread and return a `Prepared`;
the `Player` trait loads, seeks, pauses, changes speed and quality, and reports
position. Nothing outside `src/playback/gst/` names GStreamer.

`prepare` calls innertube-rs's `Playback::open`, which returns a `SegmentVod` for VOD or a
`SabrStream` for live. Both give the app complete segments per track, in time order.

### The substrate

`GstSubstrate` (`src/playback/gst/substrate/`) plays those segments into a
`gtk::Picture`:

```
SegmentTrack ─► feed thread ─► parse pipeline ─► pump thread ─► play pipeline
 (innertube)                  appsrc ! parsebin                 appsrc ! decodebin3 ! … ! gtk4paintablesink
                                 ! appsink                      appsrc ! decodebin3 ! mtwsola ! … ! audio sink
```

- **Feed** (`feed.rs`): one thread per track pushes segment bytes into its parse pipeline.
  MP4 goes in with qtdemux's MSE caps, so a flush resets its box parser.
- **Parse** (`pipeline.rs`): `parsebin` demuxes and parses each track into timestamped
  samples. AV1 is requested with `alignment=tu`.
- **Pump** (`pump.rs`): moves parsed samples into the play pipeline. After a parser reset
  it drops frames until a random access point, as MSE does. It throttles against the play
  `appsrc`, which is bounded in media time.
- **Play**: `decodebin3` picks a decoder. With VA-API decoders for every ladder codec,
  frames stay on the GPU and reach `gtk4paintablesink` as DMABuf.
- **Seeks** flush both pipelines. `seek_sync.rs` starts audio and video together after a
  seek.
- **Speed** is a pitch-preserving time stretch: `mtwsola` (`wsola_element.rs`) wraps a
  port of Chromium's WSOLA (`wsola.rs`). `stretch.rs` maps content time to output time so
  video follows the audio across live tempo changes.

This substrate is the app's side of the one sanctioned divergence from the web client:
the web player appends the same segments to MSE.

### Previews, Shorts and the queue

- **Hover previews** (`preview_player.rs`, `gst/preview.rs`): a small, muted, low-rung
  pipeline per hovered card, drawn into one shared surface. Cards whose response carries an
  animated WebP play that instead (`widgets/an_webp.rs`).
- **Shorts** (`widgets/shorts_view.rs`) have their own `Player` and preload the next
  Short as the web client does.
- **Queue** (`queue.rs`): a play-order list with no UI dependency; the queue panel and the
  miniplayer render it.

## Sign-in

`auth/login.rs` opens Google's sign-in page for YouTube in a WebKitGTK window and captures
the YouTube cookie header. `auth/store.rs` keeps it in the Secret Service keyring (through `oo7`), or in a
mode-0600 file when no keyring answers. `auth::build_session` builds an authenticated
`Session` when the stored cookie parses, and an anonymous one otherwise.

## Settings and state

- **Settings** (`src/settings.rs`): `$XDG_CONFIG_HOME/moderntube/settings.toml`, one serde
  struct, edited in the Preferences window (`app.preferences`, Ctrl+,). It holds the
  playback options and the Catch me up endpoints. A missing file or key reads as the
  default.
- **UI state** (`src/prefs/`): small plain-text files in the same directory, one per key
  (theme and accent, quality cap, miniplayer size, SponsorBlock categories, resume
  positions and similar). `PrefsStore` is the typed, observable snapshot; a change writes
  through to disk.
- **Logs**: stderr, plus `$XDG_STATE_HOME/moderntube/moderntube.log` (mode 0600) when
  stderr is not a terminal. `RUST_LOG` filters both.

## Catch me up and Ask

Off until an endpoint is configured. "Catch me up" fetches a live stream's audio for a DVR
range (`innertube::fetch_audio_window`), transcribes it through the configured speech-to-
text providers in order, adds the live-chat replay for the same range, and asks a
configured OpenAI-compatible endpoint for a summary with clickable timestamps. Ask answers
questions over a video's transcript. Neither feature has a default endpoint.

## Integrations

- **MPRIS** (`mpris.rs`): `org.mpris.MediaPlayer2` on the session bus, for media keys,
  the lock screen and `playerctl`.
- **SponsorBlock** (`sponsorblock.rs`): fetches skip segments; the window's playback poll
  skips the enabled categories.
- **Theme** (`theme.rs`): one CSS provider driven by an accent colour, an OLED option and
  the libadwaita colour scheme.

## Testing

`cargo test` covers what runs without a display: settings and preferences, the queue,
the substrate's timing and gating helpers, WSOLA, parsers. Playback and rendering are
verified by driving the app through the test hooks and looking at the picture; see
`AGENTS.md`.
