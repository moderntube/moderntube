//! An ambient glow behind the Shorts card: the playing video, blurred and saturated, filling
//! the page. Each new video frame is rendered once into a tiny offscreen texture (blur and
//! colour applied at that size), which the page draws stretched with linear filtering. The
//! cost per frame is one small render pass and one texture draw, at the video's frame rate.
use std::cell::RefCell;

use gtk4 as gtk;
use gtk::{gdk, glib, graphene, gsk, prelude::*, subclass::prelude::*};

/// Long side of the offscreen glow texture, in pixels.
const TINY: f64 = 64.0;
/// Blur radius at that size; stretched to the page it reads as a very wide blur.
const BLUR: f64 = 3.0;
const SATURATE: f32 = 1.35;
/// Baked into the offscreen texture: an opacity on the widget would composite the whole page
/// through an extra full-size pass every frame.
const OPACITY: f32 = 0.32;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct AmbientGlow {
        pub source: RefCell<Option<(gdk::Paintable, glib::SignalHandlerId)>>,
        pub texture: RefCell<Option<gdk::Texture>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AmbientGlow {
        const NAME: &'static str = "MtAmbientGlow";
        type Type = super::AmbientGlow;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for AmbientGlow {
        fn dispose(&self) {
            if let Some((p, h)) = self.source.take() {
                p.disconnect(h);
            }
        }
    }

    impl WidgetImpl for AmbientGlow {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let Some(tex) = self.texture.borrow().clone() else { return };
            let w = self.obj();
            let (vw, vh) = (w.width() as f32, w.height() as f32);
            let (tw, th) = (tex.width() as f32, tex.height() as f32);
            let scale = (vw / tw).max(vh / th);
            let (dw, dh) = (tw * scale, th * scale);
            let rect = graphene::Rect::new((vw - dw) / 2.0, (vh - dh) / 2.0, dw, dh);
            snapshot.append_scaled_texture(&tex, gsk::ScalingFilter::Linear, &rect);
        }
    }
}

glib::wrapper! {
    pub struct AmbientGlow(ObjectSubclass<imp::AmbientGlow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for AmbientGlow {
    fn default() -> Self {
        Self::new()
    }
}

impl AmbientGlow {
    pub fn new() -> Self {
        let w: Self = glib::Object::new();
        w.set_can_target(false);
        w
    }

    /// Follow `paintable`: every frame it presents is redrawn into the glow.
    pub fn set_source(&self, paintable: Option<&gdk::Paintable>) {
        let imp = self.imp();
        if let Some((old, h)) = imp.source.take() {
            old.disconnect(h);
        }
        if let Some(p) = paintable {
            let weak = self.downgrade();
            let h = p.connect_invalidate_contents(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.refresh();
                }
            });
            imp.source.replace(Some((p.clone(), h)));
            self.refresh();
        }
    }

    fn refresh(&self) {
        if !self.is_mapped() {
            return;
        }
        let Some(src) = self.imp().source.borrow().as_ref().map(|(p, _)| p.clone()) else { return };
        let Some(renderer) = self.native().and_then(|n| n.renderer()) else { return };
        let (iw, ih) = (src.intrinsic_width().max(1) as f64, src.intrinsic_height().max(1) as f64);
        let k = TINY / iw.max(ih);
        let (sw, sh) = ((iw * k).round().max(1.0), (ih * k).round().max(1.0));
        let snap = gtk::Snapshot::new();
        let s = SATURATE;
        let (r, g, b) = (0.2126 * (1.0 - s), 0.7152 * (1.0 - s), 0.0722 * (1.0 - s));
        let matrix = graphene::Matrix::from_float([
            r + s, r, r, 0.0, //
            g, g + s, g, 0.0, //
            b, b, b + s, 0.0, //
            0.0, 0.0, 0.0, OPACITY,
        ]);
        snap.push_color_matrix(&matrix, &graphene::Vec4::zero());
        snap.push_blur(BLUR);
        src.snapshot(&snap, sw, sh);
        snap.pop();
        snap.pop();
        let Some(node) = snap.to_node() else { return };
        let viewport = graphene::Rect::new(0.0, 0.0, sw as f32, sh as f32);
        self.imp().texture.replace(Some(renderer.render_texture(node, Some(&viewport))));
        self.queue_draw();
    }
}
