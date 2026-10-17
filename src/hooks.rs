//! Test hooks: app actions a harness drives headlessly, as a command-line option
//! (`moderntube --seek-test 45:10`) or as `app.<name>` over D-Bus.
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::rc::Rc;
use std::sync::{Mutex, OnceLock};

use gtk4 as gtk;
use gtk::prelude::*;
use gtk::{gio, glib};

#[derive(Clone, Copy)]
enum Param {
    Str,
    Int,
    Float,
    Flag,
}

/// Every hook: name, parameter type, and the command-line help text.
const HOOKS: &[(&str, Param, &str)] = &[
    ("autoplay", Param::Str, "Open VIDEO_ID 1 s after startup"),
    ("autoplay-pip", Param::Str, "Open VIDEO_ID in the corner player 1 s after startup"),
    ("autoplay2", Param::Str, "Open a second video at [SECS:]VIDEO_ID (default 60 s)"),
    ("autoplay3", Param::Str, "Open a third video at [SECS:]VIDEO_ID (default 90 s)"),
    ("shorts", Param::Int, "Open the Shorts tab after SECS"),
    ("search", Param::Str, "Run a search 1 s after startup"),
    ("playlist", Param::Str, "Open PLAYLIST_ID 1 s after startup"),
    ("playlist-play", Param::Int, "Press Play all on the opened playlist after SECS"),
    ("quality-test", Param::Str, "SECS:HEIGHT|auto[,...]: pick a quality at each time"),
    ("speed-test", Param::Str, "SECS:RATE[,...]: set the playback rate at each time"),
    ("seek-test", Param::Str, "SECS:TARGET_S[,...]: seek to TARGET_S at each time"),
    ("skip-test", Param::Str, "SECS:COUNT:EVERY_MS:DELTA_S[,...]: arrow-key skips"),
    ("pause-test", Param::Str, "SECS:HOLD_S[,...]: pause at SECS, resume HOLD_S later"),
    ("live-dvr-test", Param::Str, "SECS:STEP[,...]: a relative seek, live, or a NNx speed"),
    ("live-edge-test", Param::Int, "Press the live button after SECS"),
    ("live-switch-test", Param::Str, "SECS:HEIGHT[,...]: live quality switches after a live open"),
    ("ask-test", Param::Str, "SECS:QUESTION: open Ask and ask QUESTION"),
    ("dvr-test", Param::Str, "SECS:BEHIND_S: reopen the live stream BEHIND_S behind its head"),
    ("catchup-test", Param::Str, "SECS:joined|paused: run Catch me up"),
    ("catchup-cap-hours", Param::Float, "Catch-up window in hours, over the saved setting"),
    ("chat-mode", Param::Str, "Place live chat: docked, overlay or floating"),
    ("desc-toggle", Param::Str, "SECS[,...]: press the channel description toggle"),
    ("channel-tab", Param::Str, "SECS:Videos|Shorts|Live: open a channel tab"),
    ("hover-test", Param::Str, "SECS:INDEX[,...]: preview a results card at each time"),
    ("hover-activate", Param::Int, "Open a hover-test preview SECS after it shows"),
    ("open-settings", Param::Flag, "Show the Settings content in a window"),
    ("autoscroll", Param::Flag, "Scroll the results grid while the fps probe runs"),
    ("max-height", Param::Int, "Quality cap in pixels, over the saved setting"),
    ("mute", Param::Flag, "Start the player muted"),
    ("speed", Param::Float, "Start the player at this rate (0.25 to 4)"),
    ("audio-device", Param::Str, "pipewire/SINK: play audio on that sink"),
    ("dump-feed", Param::Str, "Write each fed segment to DIR"),
    ("catchup-save-audio", Param::Str, "Write each catch-up audio chunk to DIR"),
    ("no-resume", Param::Flag, "Start every video at 0 and save no positions"),
    ("no-keyring", Param::Flag, "Read the stored session from the file only"),
];

const NEW_INSTANCE: &str = "new-instance";

fn values() -> &'static Mutex<HashMap<&'static str, String>> {
    static VALUES: OnceLock<Mutex<HashMap<&'static str, String>>> = OnceLock::new();
    VALUES.get_or_init(Default::default)
}

type Listener = Rc<dyn Fn(&str)>;

thread_local! {
    static LISTENERS: RefCell<HashMap<&'static str, Vec<Listener>>> = RefCell::default();
}

/// The hook's last value, from the command line or an activation. A flag reads "1".
pub fn get(name: &str) -> Option<String> {
    values().lock().unwrap_or_else(|e| e.into_inner()).get(name).cloned()
}

pub fn flag(name: &str) -> bool {
    get(name).is_some()
}

/// Run `f` with the hook's value now if it has one, and again on each later activation.
/// Command-line values are set before the window is built, so they keep the app-start clock.
pub fn on(name: &'static str, f: impl Fn(&str) + 'static) {
    let f: Listener = Rc::new(f);
    if let Some(v) = get(name) {
        f(&v);
    }
    LISTENERS.with(|l| l.borrow_mut().entry(name).or_default().push(f));
}

fn set(name: &'static str, value: String) {
    values().lock().unwrap_or_else(|e| e.into_inner()).insert(name, value);
}

fn variant_text(v: &glib::Variant) -> String {
    if let Some(s) = v.str() {
        s.to_string()
    } else if let Some(i) = v.get::<i32>() {
        i.to_string()
    } else if let Some(d) = v.get::<f64>() {
        d.to_string()
    } else {
        "1".to_string()
    }
}

/// Registers every hook as an app action and a command-line option, and `--new-instance`,
/// which keeps a test run out of single-instance hand-off.
pub fn install(app: &impl IsA<gio::Application>) {
    let app = app.as_ref();
    for &(name, param, help) in HOOKS {
        let (arg, ty, arg_help) = match param {
            Param::Str => (glib::OptionArg::String, Some(glib::VariantTy::STRING), Some("VALUE")),
            Param::Int => (glib::OptionArg::Int, Some(glib::VariantTy::INT32), Some("N")),
            Param::Float => (glib::OptionArg::Double, Some(glib::VariantTy::DOUBLE), Some("X")),
            Param::Flag => (glib::OptionArg::None, None, None),
        };
        app.add_main_option(name, glib::Char::from(0), glib::OptionFlags::NONE, arg, help, arg_help);
        let action = gio::SimpleAction::new(name, ty);
        action.connect_activate(move |_, p| {
            let value = p.map_or_else(|| "1".to_string(), variant_text);
            tracing::info!("[hook] {name} {value}");
            set(name, value.clone());
            let listeners = LISTENERS.with(|l| l.borrow().get(name).cloned().unwrap_or_default());
            for f in listeners {
                f(&value);
            }
        });
        app.add_action(&action);
    }
    app.add_main_option(
        NEW_INSTANCE,
        glib::Char::from(0),
        glib::OptionFlags::NONE,
        glib::OptionArg::None,
        "Run beside an instance that is already open",
        None,
    );
    app.connect_handle_local_options(|app, options| {
        for &(name, _, _) in HOOKS {
            if let Some(v) = options.lookup_value(name, None) {
                set(name, variant_text(&v));
            }
        }
        if options.contains(NEW_INSTANCE) {
            app.set_flags(app.flags() | gio::ApplicationFlags::NON_UNIQUE);
        }
        ControlFlow::Continue(())
    });
}
