//! Seek-preview thumbnails: storyboard sprite sheets fetched once, cached, and cropped.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4 as gtk;
use gtk::glib;
use gtk::gdk;
use gtk::prelude::*;

use gtk::{Box as GtkBox, EventControllerMotion, Label, Orientation, Picture, Scale};

use innertube::{Storyboard, StoryboardThumb};

use crate::playback::Player;
use crate::runtime::AsyncRuntime;

use super::fmt_time;

/// A decoded storyboard sprite-sheet as raw pixels (cairo ARGB32 / B8G8R8A8-premul, the
/// format `gdk::Texture::download` produces), so individual cells can be cropped out
/// cheaply without any clip-container tricks.
struct SheetPixels {
    w: usize,
    h: usize,
    bytes: Vec<u8>, // stride = w*4
}

/// Session-lifetime cache of decoded sprite-sheets, keyed by sheet URL. Sheets are small and
/// never evicted, so a repeat hover costs a cell copy.
type SheetCache = Rc<RefCell<HashMap<String, Rc<SheetPixels>>>>;

/// Loads and decodes a sprite-sheet to raw pixels (cached) and hands it to `apply`. A miss
/// fetches once, then decodes on the main loop.
fn load_sheet<F: Fn(&Rc<SheetPixels>) + 'static>(rt: &AsyncRuntime, cache: &SheetCache, url: String, apply: F) {
    if let Some(sheet) = cache.borrow().get(&url) {
        apply(sheet);
        return;
    }
    let cache = cache.clone();
    rt.spawn(
        async move { fetch_bytes(&url).await.map(|b| (url, b)) },
        move |res: Option<(String, Vec<u8>)>| {
            if let Some((url, bytes)) = res {
                let gb = glib::Bytes::from_owned(bytes);
                if let Ok(tex) = gdk::Texture::from_bytes(&gb) {
                    let (w, h) = (tex.width() as usize, tex.height() as usize);
                    let mut buf = vec![0u8; w * h * 4];
                    tex.download(&mut buf, w * 4);
                    let sheet = Rc::new(SheetPixels { w, h, bytes: buf });
                    cache.borrow_mut().insert(url, sheet.clone());
                    apply(&sheet);
                }
            }
        },
    );
}

/// Crop one cell out of a decoded sheet into its own texture. `None` if the cell falls
/// outside the sheet's real pixels (e.g. a partial final sheet).
fn crop_cell(sheet: &SheetPixels, thumb: &StoryboardThumb) -> Option<gdk::MemoryTexture> {
    let (x, y, w, h) = (thumb.x as usize, thumb.y as usize, thumb.w as usize, thumb.h as usize);
    if w == 0 || h == 0 || x + w > sheet.w || y + h > sheet.h {
        return None;
    }
    let stride = sheet.w * 4;
    let mut cell = vec![0u8; w * h * 4];
    for row in 0..h {
        let src = (y + row) * stride + x * 4;
        cell[row * w * 4..(row + 1) * w * 4].copy_from_slice(&sheet.bytes[src..src + w * 4]);
    }
    Some(gdk::MemoryTexture::new(
        w as i32,
        h as i32,
        gdk::MemoryFormat::B8g8r8a8Premultiplied,
        &glib::Bytes::from_owned(cell),
        w * 4,
    ))
}

async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    let resp = reqwest::get(url).await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    resp.bytes().await.ok().map(|b| b.to_vec())
}

/// The scrub preview: a thumbnail of the hovered position and its timestamp, shown above
/// the seek bar while the pointer is over it.
pub(super) fn build_scrub_preview(
    engine: &Rc<dyn Player>,
    rt: AsyncRuntime,
    scale: &Scale,
    storyboard: &Rc<RefCell<Option<Storyboard>>>,
    dur: &Rc<Cell<f64>>,
) -> GtkBox {
    let preview = GtkBox::new(Orientation::Vertical, 2);
    {
        preview.add_css_class("scrub-preview");
        preview.set_can_target(false);
        preview.set_visible(false);
        preview.set_halign(gtk::Align::Start);
        preview.set_valign(gtk::Align::Start);
        preview.set_overflow(gtk::Overflow::Hidden); // clip the Picture to the radius
        // The thumbnail is one cropped cell as its own texture, shown 1:1.
        let pic = Picture::new();
        pic.set_can_shrink(false);
        let ts = Label::new(None);
        ts.add_css_class("caption-heading");
        preview.append(&pic);
        preview.append(&ts);

        let sheets: SheetCache = Rc::new(RefCell::new(HashMap::new()));
        // Cell currently shown (sheet url, x, y); skips redundant crops and uploads.
        let cur_cell: Rc<RefCell<Option<(String, u32, u32)>>> = Rc::new(RefCell::new(None));
        let motion = EventControllerMotion::new();
        let (sb, dur_c, scale_c) = (storyboard.clone(), dur.clone(), scale.clone());
        {
            let (preview, pic, ts, cur_cell) =
                (preview.clone(), pic.clone(), ts.clone(), cur_cell.clone());
            let engine = engine.clone();
            motion.connect_motion(move |_, mx, _| {
                let w = scale_c.width();
                if w <= 0 {
                    return;
                }
                let frac = (mx / w as f64).clamp(0.0, 1.0);
                // Live DVR: the hovered stream time's segment frame, labelled by how far
                // behind the head it is.
                let (thumb, label) = if let Some((lo, head)) = engine.live_window() {
                    let at = lo + frac * (head - lo);
                    let Some(thumb) = engine.live_thumbnail(at) else {
                        preview.set_visible(false);
                        return;
                    };
                    (thumb, format!("−{}", fmt_time(head - at)))
                } else {
                    let sb = sb.borrow();
                    let Some(sb) = sb.as_ref() else { return };
                    let d = dur_c.get();
                    if d <= 0.0 {
                        return;
                    }
                    let Some(thumb) = sb.thumbnail_at((frac * d * 1000.0) as i64) else { return };
                    (thumb, fmt_time(frac * d))
                };
                ts.set_text(&label);
                // Position the overlay child so its center sits over the pointer just above the
                // scrubber, in the coordinates of the overlay parent.
                if let Some(parent) = preview.parent() {
                    if let Some(pt) = scale_c.compute_point(
                        &parent,
                        &gtk::graphene::Point::new(mx as f32, 0.0),
                    ) {
                        let pw = preview.width().max(thumb.w as i32 + 8);
                        let ph = preview.height().max(thumb.h as i32 + 30);
                        let x = (pt.x() as i32 - pw / 2)
                            .clamp(0, (parent.width() - pw).max(0));
                        let y = (pt.y() as i32 - ph - 10).max(0);
                        preview.set_margin_start(x);
                        preview.set_margin_top(y);
                    }
                }
                if !preview.is_visible() {
                    preview.set_visible(true);
                }
                // Only re-crop/upload when the cell actually changes (cheap guard).
                let cell = (thumb.url.clone(), thumb.x, thumb.y);
                if cur_cell.borrow().as_ref() == Some(&cell) {
                    return;
                }
                *cur_cell.borrow_mut() = Some(cell);
                let pic2 = pic.clone();
                // Sheet fetched+decoded once (cached for the session); crop is a memcpy.
                load_sheet(&rt, &sheets, thumb.url.clone(), move |sheet| {
                    if let Some(tex) = crop_cell(sheet, &thumb) {
                        pic2.set_paintable(Some(&tex));
                        pic2.set_size_request(thumb.w as i32, thumb.h as i32);
                    }
                });
            });
        }
        {
            let preview = preview.clone();
            motion.connect_leave(move |_| preview.set_visible(false));
        }
        scale.add_controller(motion);
    }
    preview
}
