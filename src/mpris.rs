//! MPRIS: exposes `org.mpris.MediaPlayer2` and `.Player` on D-Bus so media keys, headset
//! buttons, the lock screen and `playerctl` can control playback and read now-playing
//! metadata.
//!
//! The D-Bus connection lives on the tokio runtime. Control requests reach the glib main
//! thread over a channel (`MprisCommand`), because handlers must not touch the `Rc`-bound
//! engine. State pushed the other way (`MprisUpdate`) updates the interface's properties and
//! emits `PropertiesChanged`.

use std::collections::HashMap;
use std::rc::Rc;

use gtk4 as gtk;
use zbus::connection;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use crate::runtime::AsyncRuntime;

const PATH: &str = "/org/mpris/MediaPlayer2";
const BUS_NAME: &str = "org.mpris.MediaPlayer2.moderntube";
const TRACK_ID: &str = "/org/mpris/MediaPlayer2/moderntube/track";

/// A control request coming from D-Bus (media keys / playerctl / lockscreen).
#[derive(Clone, Copy, Debug)]
pub enum MprisCommand {
    Play,
    Pause,
    PlayPause,
    Stop,
    Next,
    Previous,
    Raise,
    Quit,
}

/// A state update pushed from the app to the D-Bus interface.
pub enum MprisUpdate {
    /// Now-playing track metadata (`length_us` = duration in microseconds, 0 if unknown).
    Track { title: String, channel: String, art_url: Option<String>, length_us: i64 },
    /// "Playing" | "Paused" | "Stopped".
    Status(&'static str),
    /// Whether Next/Previous are currently meaningful.
    Nav { can_next: bool, can_prev: bool },
}

fn metadata(title: &str, channel: &str, art_url: Option<&str>, length_us: i64) -> HashMap<String, OwnedValue> {
    let mut m = HashMap::new();
    if let Ok(p) = ObjectPath::try_from(TRACK_ID) {
        if let Ok(v) = OwnedValue::try_from(Value::from(p)) {
            m.insert("mpris:trackid".to_string(), v);
        }
    }
    if let Ok(v) = OwnedValue::try_from(Value::from(title)) {
        m.insert("xesam:title".to_string(), v);
    }
    if let Ok(v) = OwnedValue::try_from(Value::from(vec![channel.to_string()])) {
        m.insert("xesam:artist".to_string(), v);
    }
    if let Some(url) = art_url {
        if let Ok(v) = OwnedValue::try_from(Value::from(url)) {
            m.insert("mpris:artUrl".to_string(), v);
        }
    }
    if length_us > 0 {
        if let Ok(v) = OwnedValue::try_from(Value::from(length_us)) {
            m.insert("mpris:length".to_string(), v);
        }
    }
    m
}

/// The `org.mpris.MediaPlayer2.Player` interface.
struct Player {
    tx: async_channel::Sender<MprisCommand>,
    status: String,
    metadata: HashMap<String, OwnedValue>,
    can_next: bool,
    can_prev: bool,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    async fn next(&self) {
        let _ = self.tx.send(MprisCommand::Next).await;
    }
    async fn previous(&self) {
        let _ = self.tx.send(MprisCommand::Previous).await;
    }
    async fn pause(&self) {
        let _ = self.tx.send(MprisCommand::Pause).await;
    }
    async fn play_pause(&self) {
        let _ = self.tx.send(MprisCommand::PlayPause).await;
    }
    async fn stop(&self) {
        let _ = self.tx.send(MprisCommand::Stop).await;
    }
    async fn play(&self) {
        let _ = self.tx.send(MprisCommand::Play).await;
    }
    // Seek, SetPosition and OpenUri are accepted and ignored (`CanSeek` is false).
    async fn seek(&self, _offset: i64) {}
    async fn set_position(&self, _track: ObjectPath<'_>, _pos: i64) {}
    async fn open_uri(&self, _uri: String) {}

    #[zbus(property)]
    async fn playback_status(&self) -> String {
        self.status.clone()
    }
    #[zbus(property)]
    async fn loop_status(&self) -> String {
        "None".to_string()
    }
    #[zbus(property)]
    async fn rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    async fn shuffle(&self) -> bool {
        false
    }
    #[zbus(property)]
    async fn metadata(&self) -> HashMap<String, OwnedValue> {
        self.metadata
            .iter()
            .filter_map(|(k, v)| v.try_clone().ok().map(|v| (k.clone(), v)))
            .collect()
    }
    #[zbus(property)]
    async fn volume(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    async fn position(&self) -> i64 {
        0
    }
    #[zbus(property)]
    async fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    async fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    async fn can_go_next(&self) -> bool {
        self.can_next
    }
    #[zbus(property)]
    async fn can_go_previous(&self) -> bool {
        self.can_prev
    }
    #[zbus(property)]
    async fn can_play(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn can_pause(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn can_seek(&self) -> bool {
        false
    }
    #[zbus(property)]
    async fn can_control(&self) -> bool {
        true
    }
}

/// The root `org.mpris.MediaPlayer2` interface.
struct Root {
    tx: async_channel::Sender<MprisCommand>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    async fn raise(&self) {
        let _ = self.tx.send(MprisCommand::Raise).await;
    }
    async fn quit(&self) {
        let _ = self.tx.send(MprisCommand::Quit).await;
    }
    #[zbus(property)]
    async fn can_quit(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn can_raise(&self) -> bool {
        true
    }
    #[zbus(property)]
    async fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    async fn identity(&self) -> String {
        "ModernTube".to_string()
    }
    #[zbus(property)]
    async fn desktop_entry(&self) -> String {
        "moderntube".to_string()
    }
    #[zbus(property)]
    async fn supported_uri_schemes(&self) -> Vec<String> {
        vec!["https".to_string()]
    }
    #[zbus(property)]
    async fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

/// Handle held by the app: drives commands into the UI and pushes state out to D-Bus.
pub struct Mpris {
    cmd_rx: async_channel::Receiver<MprisCommand>,
    state_tx: async_channel::Sender<MprisUpdate>,
}

impl Mpris {
    /// Starts the D-Bus service on the tokio runtime. If the connection or name request fails
    /// (for example another instance owns the name), media keys stay disabled and startup
    /// continues.
    pub fn new(rt: &AsyncRuntime) -> Self {
        let (cmd_tx, cmd_rx) = async_channel::unbounded::<MprisCommand>();
        let (state_tx, state_rx) = async_channel::unbounded::<MprisUpdate>();
        rt.spawn_bg(async move {
            let player = Player {
                tx: cmd_tx.clone(),
                status: "Stopped".to_string(),
                metadata: HashMap::new(),
                can_next: false,
                can_prev: false,
            };
            let root = Root { tx: cmd_tx };
            let conn = match connection::Builder::session()
                .and_then(|b| b.name(BUS_NAME))
                .and_then(|b| b.serve_at(PATH, player))
                .and_then(|b| b.serve_at(PATH, root))
            {
                Ok(b) => match b.build().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("[mpris] connection failed: {e} (media keys disabled)");
                        return;
                    }
                },
                Err(e) => {
                    tracing::warn!("[mpris] setup failed: {e} (media keys disabled)");
                    return;
                }
            };
            // Apply state updates from the app and emit PropertiesChanged.
            while let Ok(update) = state_rx.recv().await {
                let Ok(iref) = conn.object_server().interface::<_, Player>(PATH).await else {
                    continue;
                };
                let emitter = iref.signal_emitter().clone();
                let mut p = iref.get_mut().await;
                match update {
                    MprisUpdate::Track { title, channel, art_url, length_us } => {
                        p.metadata = metadata(&title, &channel, art_url.as_deref(), length_us);
                        let _ = p.metadata_changed(&emitter).await;
                    }
                    MprisUpdate::Status(s) => {
                        if p.status != s {
                            p.status = s.to_string();
                            let _ = p.playback_status_changed(&emitter).await;
                        }
                    }
                    MprisUpdate::Nav { can_next, can_prev } => {
                        if p.can_next != can_next {
                            p.can_next = can_next;
                            let _ = p.can_go_next_changed(&emitter).await;
                        }
                        if p.can_prev != can_prev {
                            p.can_prev = can_prev;
                            let _ = p.can_go_previous_changed(&emitter).await;
                        }
                    }
                }
            }
        });
        Self { cmd_rx, state_tx }
    }

    /// Run `handler` on the glib main thread for every D-Bus control request.
    pub fn on_command(&self, handler: Rc<dyn Fn(MprisCommand)>) {
        let rx = self.cmd_rx.clone();
        gtk::glib::spawn_future_local(async move {
            while let Ok(cmd) = rx.recv().await {
                handler(cmd);
            }
        });
    }

    /// Push the now-playing metadata.
    pub fn set_track(&self, title: &str, channel: &str, art_url: Option<&str>, length_secs: Option<f64>) {
        let length_us = length_secs.map(|s| (s * 1_000_000.0) as i64).unwrap_or(0);
        let _ = self.state_tx.send_blocking(MprisUpdate::Track {
            title: title.to_string(),
            channel: channel.to_string(),
            art_url: art_url.map(str::to_string),
            length_us,
        });
    }

    /// Push the playback status ("Playing" / "Paused" / "Stopped").
    pub fn set_status(&self, status: &'static str) {
        let _ = self.state_tx.send_blocking(MprisUpdate::Status(status));
    }

    /// Push whether Next/Previous are meaningful right now.
    pub fn set_nav(&self, can_next: bool, can_prev: bool) {
        let _ = self.state_tx.send_blocking(MprisUpdate::Nav { can_next, can_prev });
    }
}
