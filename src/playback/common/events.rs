//! Callbacks the window registers on a player; the substrate fires them.
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use innertube::StreamError;

pub type FailedFn = Rc<dyn Fn(Arc<StreamError>)>;
type Cb = RefCell<Option<Rc<dyn Fn()>>>;
type ReasonCb = RefCell<Option<FailedFn>>;

#[derive(Default)]
pub struct PlayerEvents {
    /// The first frame of a newly loaded video is on screen.
    pub(crate) on_loaded: Cb,
    /// Playback reached the natural end (never a user stop or a new load).
    pub(crate) on_ended: Cb,
    /// The stream gave up before the end; the argument says why.
    pub(crate) on_failed: ReasonCb,
}

impl PlayerEvents {
    fn fire(cb: &Cb) {
        let f = cb.borrow().clone();
        if let Some(f) = f {
            f();
        }
    }

    pub(crate) fn loaded(&self) {
        Self::fire(&self.on_loaded);
    }

    pub(crate) fn ended(&self) {
        Self::fire(&self.on_ended);
    }

    pub(crate) fn failed(&self, reason: Arc<StreamError>) {
        let f = self.on_failed.borrow().clone();
        if let Some(f) = f {
            f(reason);
        }
    }
}
