//! `win.leave-note` (Ctrl+Shift+F) and `win.open-notes`: local feedback notes.
use std::rc::Rc;

use adw::prelude::*;
use adw::ApplicationWindow;
use gtk4 as gtk;
use gtk::{gdk, gio};

use crate::notes::NoteContext;
use crate::widgets::feedback_dialog;

use super::app::App;
use super::PlayerMode;

pub(super) fn wire_feedback(app: &Rc<App>, window: &ApplicationWindow) {
    let leave = gio::SimpleAction::new("leave-note", None);
    {
        let app = app.clone();
        leave.connect_activate(move |_, _| open_dialog(&app));
    }
    window.add_action(&leave);
    if let Some(application) = window.application() {
        application.set_accels_for_action("win.leave-note", &["<Control><Shift>f"]);
    }

    let open = gio::SimpleAction::new("open-notes", None);
    {
        let window = window.downgrade();
        open.connect_activate(move |_, _| {
            feedback_dialog::open_notes_folder(window.upgrade().map(|w| w.upcast::<gtk::Window>()).as_ref());
        });
    }
    window.add_action(&open);
}

fn open_dialog(app: &Rc<App>) {
    let Some(window) = app.window.get() else { return };
    let screenshot = capture(window);
    let toasts = app.toasts.clone();
    let window_w = window.downgrade();
    feedback_dialog::present(
        window,
        context(app),
        screenshot,
        Rc::new(move |res| match res {
            Ok(folder) => {
                tracing::info!("[notes] saved {}", folder.display());
                let toast = adw::Toast::builder().title("Note saved").button_label("Show").timeout(4).build();
                let window_w = window_w.clone();
                toast.connect_button_clicked(move |_| {
                    let parent = window_w.upgrade().map(|w| w.upcast::<gtk::Window>());
                    gtk::FileLauncher::new(Some(&gio::File::for_path(&folder))).launch(
                        parent.as_ref(),
                        None::<&gio::Cancellable>,
                        |_| {},
                    );
                });
                toasts.add_toast(toast);
            }
            Err(e) => {
                tracing::warn!("[notes] could not save a note: {e}");
                toasts.add_toast(adw::Toast::new(&format!("Note not saved: {e}")));
            }
        }),
    );
}

/// The window's content rendered to a texture, at the logical size.
fn capture(window: &ApplicationWindow) -> Option<gdk::Texture> {
    let content = window.content()?;
    let (w, h) = (content.width(), content.height());
    if w <= 0 || h <= 0 {
        return None;
    }
    let paintable = gtk::WidgetPaintable::new(Some(&content));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(&snapshot, f64::from(w), f64::from(h));
    let node = snapshot.to_node()?;
    let renderer = window.renderer()?;
    Some(renderer.render_texture(&node, Some(&gtk::graphene::Rect::new(0.0, 0.0, w as f32, h as f32))))
}

fn context(app: &App) -> NoteContext {
    let page = app.stack.visible_child_name().map(|s| s.to_string()).unwrap_or_default();
    let mode = app.player_mode.get();
    let player = match mode {
        PlayerMode::Hidden => "hidden",
        PlayerMode::Full => "full",
        PlayerMode::Mini => "mini",
    };
    let live = mode != PlayerMode::Hidden && app.engine.is_live();
    let (video_id, video_title, position_secs) = if page == "shorts" {
        let id = app.shorts_view.get().and_then(|s| s.current_video_id());
        (id, None, None)
    } else if mode != PlayerMode::Hidden {
        let id = app.current_video.borrow().clone().or_else(|| app.current_live.borrow().clone());
        let title = Some(app.watch_info.title()).filter(|t| !t.is_empty());
        (id, title, app.engine.time_pos_checked())
    } else {
        (None, None, None)
    };
    NoteContext {
        page,
        player: player.into(),
        video_id,
        video_title,
        position_secs,
        live,
        quality: (mode != PlayerMode::Hidden).then(|| format!("{:?}", app.current_quality.get())),
    }
}
