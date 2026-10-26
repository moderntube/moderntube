//! The "Leave a note" dialog: a bug, idea or other note with the app state, a window
//! screenshot, the log tail and any added images, saved by [`crate::notes`].
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk4 as gtk;
use gtk::{gdk, gio, glib};
use gtk::{Align, Box as GtkBox, Button, Label, Orientation, ToggleButton};

use crate::notes::{self, Attachments, Note, NoteContext, NoteKind};

/// Called with the saved note's folder, or the error text.
pub type OnSaved = Rc<dyn Fn(Result<PathBuf, String>)>;

/// Opens the dialog over `parent`. `screenshot` is the window captured before it opened.
pub fn present(parent: &impl IsA<gtk::Widget>, context: NoteContext, screenshot: Option<gdk::Texture>, on_saved: OnSaved) {
    let dialog = adw::Dialog::builder().title("Leave a note").content_width(540).build();
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::builder()
        .show_start_title_buttons(false)
        .show_end_title_buttons(false)
        .title_widget(&adw::WindowTitle::new("Leave a note", "Kept on this computer"))
        .build();
    let cancel = Button::with_label("Cancel");
    let save = Button::builder().label("Save").sensitive(false).build();
    save.add_css_class("suggested-action");
    header.pack_start(&cancel);
    header.pack_end(&save);
    toolbar.add_top_bar(&header);

    let body = GtkBox::new(Orientation::Vertical, 18);
    body.set_margin_top(12);
    body.set_margin_bottom(18);
    body.set_margin_start(18);
    body.set_margin_end(18);

    let kinds = GtkBox::new(Orientation::Horizontal, 0);
    kinds.add_css_class("linked");
    kinds.set_homogeneous(true);
    let kind_button = |icon: &str, label: &str| {
        let b = ToggleButton::builder().child(&adw::ButtonContent::builder().icon_name(icon).label(label).build()).build();
        kinds.append(&b);
        b
    };
    let bug = kind_button("dialog-warning-symbolic", "Bug");
    let idea = kind_button("starred-symbolic", "Idea");
    let other = kind_button("document-edit-symbolic", "Other");
    idea.set_group(Some(&bug));
    other.set_group(Some(&bug));
    bug.set_active(true);
    body.append(&kinds);

    let text = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .top_margin(12)
        .bottom_margin(12)
        .left_margin(14)
        .right_margin(14)
        .build();
    text.add_css_class("note-text");
    let scroller = gtk::ScrolledWindow::builder()
        .child(&text)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(150)
        .max_content_height(320)
        .propagate_natural_height(true)
        .build();
    let placeholder = Label::builder()
        .label("What happened, or what would you like to see?")
        .halign(Align::Start)
        .valign(Align::Start)
        .margin_top(12)
        .margin_start(16)
        .can_target(false)
        .build();
    placeholder.add_css_class("dim-label");
    let editor = gtk::Overlay::new();
    editor.set_child(Some(&scroller));
    editor.add_overlay(&placeholder);
    editor.add_css_class("card");
    editor.add_css_class("note-editor");
    editor.set_overflow(gtk::Overflow::Hidden);
    body.append(&editor);

    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::None);
    list.add_css_class("boxed-list");

    let where_row = adw::ActionRow::builder().title("Where you were").subtitle(context.summary()).build();
    where_row.add_prefix(&gtk::Image::from_icon_name("find-location-symbolic"));
    if let Some(t) = &context.video_title {
        where_row.set_tooltip_text(Some(t));
    }
    list.append(&where_row);

    let shot_row = adw::SwitchRow::builder().title("Screenshot").build();
    match &screenshot {
        Some(tex) => {
            shot_row.set_subtitle("The window when you opened this");
            shot_row.set_active(true);
            let thumb = thumbnail(parent.as_ref(), tex, 112.0, 63.0);
            thumb.set_valign(Align::Center);
            thumb.set_margin_top(6);
            thumb.set_margin_bottom(6);
            shot_row.add_prefix(&thumb);
        }
        None => {
            shot_row.set_subtitle("Not available");
            shot_row.set_sensitive(false);
        }
    }
    list.append(&shot_row);

    let log_path = crate::log_path().filter(|p| p.is_file());
    let log_row = adw::SwitchRow::builder().title("Recent log").build();
    log_row.add_prefix(&gtk::Image::from_icon_name("utilities-terminal-symbolic"));
    if log_path.is_some() {
        log_row.set_subtitle(&format!("The last {} lines of moderntube.log", notes::LOG_TAIL_LINES));
        log_row.set_active(true);
    } else {
        log_row.set_subtitle("The log file is written when the app runs without a terminal");
        log_row.set_sensitive(false);
    }
    list.append(&log_row);

    let files: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let images_row = adw::ActionRow::builder().title("Images").subtitle("Add files or drop them here").build();
    images_row.add_prefix(&gtk::Image::from_icon_name("image-x-generic-symbolic"));
    let add = Button::builder().icon_name("list-add-symbolic").valign(Align::Center).tooltip_text("Add images").build();
    add.add_css_class("flat");
    images_row.add_suffix(&add);
    list.append(&images_row);
    body.append(&list);

    let strip = GtkBox::new(Orientation::Horizontal, 8);
    strip.set_visible(false);
    body.append(&strip);

    let footer = GtkBox::new(Orientation::Horizontal, 6);
    let privacy = Label::builder()
        .label("Notes are saved on this computer only.")
        .halign(Align::Start)
        .hexpand(true)
        .xalign(0.0)
        .build();
    privacy.add_css_class("dim-label");
    privacy.add_css_class("caption");
    let open_folder = Button::with_label("Open notes folder");
    open_folder.add_css_class("flat");
    open_folder.add_css_class("caption");
    footer.append(&privacy);
    footer.append(&open_folder);
    body.append(&footer);

    let content = gtk::ScrolledWindow::builder()
        .child(&body)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .build();
    toolbar.set_content(Some(&content));
    dialog.set_child(Some(&toolbar));

    let add_files: Rc<dyn Fn(Vec<PathBuf>)> = {
        let (files, strip, images_row) = (files.clone(), strip.clone(), images_row.clone());
        let parent = parent.as_ref().clone();
        Rc::new(move |new: Vec<PathBuf>| {
            for p in new.into_iter().filter(|p| p.is_file()) {
                let thumb = match gdk::Texture::from_filename(&p) {
                    Ok(tex) => thumbnail(&parent, &tex, 96.0, 54.0),
                    Err(_) => gtk::Image::from_icon_name("text-x-generic-symbolic").upcast(),
                };
                thumb.set_tooltip_text(p.file_name().and_then(|n| n.to_str()));
                strip.append(&thumb);
                files.borrow_mut().push(p);
            }
            let n = files.borrow().len();
            strip.set_visible(n > 0);
            images_row.set_subtitle(&match n {
                0 => "Add files or drop them here".to_string(),
                1 => "1 image".to_string(),
                n => format!("{n} images"),
            });
        })
    };

    {
        let add_files = add_files.clone();
        let dialog_w = dialog.downgrade();
        add.connect_clicked(move |_| {
            let filter = gtk::FileFilter::new();
            filter.set_name(Some("Images"));
            filter.add_mime_type("image/*");
            let chooser = gtk::FileDialog::builder().title("Add images").modal(true).default_filter(&filter).build();
            let parent = dialog_w.upgrade().and_then(|d| d.root()).and_downcast::<gtk::Window>();
            let add_files = add_files.clone();
            chooser.open_multiple(parent.as_ref(), None::<&gio::Cancellable>, move |res| {
                if let Ok(model) = res {
                    let paths = (0..model.n_items())
                        .filter_map(|i| model.item(i).and_downcast::<gio::File>())
                        .filter_map(|f| f.path())
                        .collect();
                    add_files(paths);
                }
            });
        });
    }
    {
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let add_files = add_files.clone();
        drop.connect_drop(move |_, value, _, _| {
            let Ok(list) = value.get::<gdk::FileList>() else { return false };
            add_files(list.files().iter().filter_map(|f| f.path()).collect());
            true
        });
        toolbar.add_controller(drop);
    }

    let buffer = text.buffer();
    {
        let (save, placeholder) = (save.clone(), placeholder.clone());
        buffer.connect_changed(move |b| {
            let empty = b.text(&b.start_iter(), &b.end_iter(), false).trim().is_empty();
            save.set_sensitive(!empty);
            placeholder.set_visible(b.char_count() == 0);
        });
    }

    let do_save: Rc<dyn Fn()> = {
        let dialog = dialog.downgrade();
        let (buffer, bug, idea) = (buffer.clone(), bug.clone(), idea.clone());
        let (shot_row, log_row) = (shot_row.clone(), log_row.clone());
        Rc::new(move || {
            let body = buffer.text(&buffer.start_iter(), &buffer.end_iter(), false).to_string();
            if body.trim().is_empty() {
                return;
            }
            let kind = if bug.is_active() {
                NoteKind::Bug
            } else if idea.is_active() {
                NoteKind::Idea
            } else {
                NoteKind::Other
            };
            let note = Note { kind, text: body, context: context.clone() };
            let att = Attachments {
                screenshot_png: screenshot
                    .as_ref()
                    .filter(|_| shot_row.is_active())
                    .map(|t| t.save_to_png_bytes().to_vec()),
                log_tail: log_path
                    .as_deref()
                    .filter(|_| log_row.is_active())
                    .and_then(|p| notes::log_tail(p, notes::LOG_TAIL_LINES)),
                files: files.borrow().clone(),
            };
            on_saved(save_note(&note, &att));
            if let Some(d) = dialog.upgrade() {
                d.close();
            }
        })
    };
    {
        let do_save = do_save.clone();
        save.connect_clicked(move |_| do_save());
    }
    {
        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, mods| {
            if matches!(key, gdk::Key::Return | gdk::Key::KP_Enter) && mods.contains(gdk::ModifierType::CONTROL_MASK) {
                do_save();
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        text.add_controller(keys);
    }
    {
        let dialog = dialog.downgrade();
        cancel.connect_clicked(move |_| {
            if let Some(d) = dialog.upgrade() {
                d.close();
            }
        });
    }
    {
        let dialog_w = dialog.downgrade();
        open_folder.connect_clicked(move |_| {
            let parent = dialog_w.upgrade().and_then(|d| d.root()).and_downcast::<gtk::Window>();
            open_notes_folder(parent.as_ref());
        });
    }

    dialog.set_focus(Some(&text));
    dialog.present(Some(parent));
}

/// Shows the notes folder in the file manager, creating it first.
pub fn open_notes_folder(parent: Option<&gtk::Window>) {
    let Some(dir) = notes::notes_dir() else { return };
    use std::os::unix::fs::DirBuilderExt;
    if std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).is_err() {
        return;
    }
    gtk::FileLauncher::new(Some(&gio::File::for_path(&dir))).launch(parent, None::<&gio::Cancellable>, |res| {
        if let Err(e) = res {
            tracing::warn!("[notes] could not open the notes folder: {e}");
        }
    });
}

/// `tex` scaled down to fit `max_w` × `max_h`. A `Picture` measures at its paintable's full
/// size, so the texture is rendered small first.
fn thumbnail(parent: &gtk::Widget, tex: &gdk::Texture, max_w: f64, max_h: f64) -> gtk::Widget {
    let (w, h) = (f64::from(tex.width()), f64::from(tex.height()));
    let scale = (max_w / w).min(max_h / h).min(1.0);
    let (tw, th) = ((w * scale).round(), (h * scale).round());
    let snapshot = gtk::Snapshot::new();
    tex.snapshot(&snapshot, tw, th);
    let small = snapshot
        .to_node()
        .zip(parent.native().and_then(|n| n.renderer()))
        .map(|(node, r)| r.render_texture(&node, None));
    let picture = gtk::Picture::for_paintable(small.as_ref().unwrap_or(tex));
    picture.set_can_shrink(false);
    picture.add_css_class("note-thumb");
    picture.set_overflow(gtk::Overflow::Hidden);
    picture.upcast()
}

fn save_note(note: &Note, att: &Attachments) -> Result<PathBuf, String> {
    let dir = notes::notes_dir().ok_or("no home directory")?;
    let now = glib::DateTime::now_local().map_err(|e| e.to_string())?;
    let stamp = now.format("%Y%m%d-%H%M%S").map_err(|e| e.to_string())?;
    let created = now.format("%Y-%m-%d %H:%M:%S %z").map_err(|e| e.to_string())?;
    notes::save_in(&dir, note, att, &stamp, &created).map_err(|e| e.to_string())
}
