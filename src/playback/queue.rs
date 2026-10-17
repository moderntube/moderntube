//! Watch queue: a play-order list. The front (index 0) is the currently playing item when
//! playback is active. Items leave the queue when they play to the end (`advance`) or the
//! queue is cleared (app restart), not when selected. UI-free; the GUI holds it as
//! `Rc<RefCell<Queue>>` and decides how to play (fullscreen or corner PiP) from the player
//! mode.

use std::collections::VecDeque;
use std::rc::Rc;

use crate::models::ResultItem;

/// The ⋮-menu actions on a result card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueAction {
    /// Insert right after the currently-playing item.
    PlayNext,
    /// Append to the end.
    AddToQueue,
}

/// Panel-refresh callback: invoked with the current queued items (front = playing) after
/// every mutation.
pub type OnChange = Rc<dyn Fn(&[ResultItem])>;

#[derive(Default)]
pub struct Queue {
    /// Play order; front = currently playing (when active).
    items: VecDeque<ResultItem>,
    /// Every surface rendering the queue, in registration order. Two surfaces render it
    /// (the header popover and the watch-page card), so a single slot would leave the first
    /// registered showing a stale queue.
    on_change: Vec<OnChange>,
}

impl Queue {
    pub fn new() -> Self {
        Self::default()
    }

    /// Make `item` the current (front); used by "play this now".
    pub fn push_front(&mut self, item: ResultItem) {
        self.drop_id(&item.video_id());
        self.items.push_front(item);
        self.notify();
    }

    /// Append to the end of the queue.
    pub fn push_back(&mut self, item: ResultItem) {
        self.drop_id(&item.video_id());
        self.items.push_back(item);
        self.notify();
    }

    /// Insert right after the current item (index 1), or at the front if the queue is empty.
    pub fn insert_after_current(&mut self, item: ResultItem) {
        self.drop_id(&item.video_id());
        let at = if self.items.is_empty() { 0 } else { 1 };
        self.items.insert(at, item);
        self.notify();
    }

    /// Drop any entry with this video id, so re-queueing moves a video instead of copying it.
    /// Does not notify.
    fn drop_id(&mut self, id: &str) {
        self.items.retain(|i| i.video_id() != id);
    }

    /// Replace the queue with `items[start..]`: the item at `start` becomes the front and the
    /// rest follow in order, so `advance` walks a playlist. A repeated video id keeps its first
    /// position, which the panel's index-addressed controls rely on. Returns the item to play,
    /// or `None` if `start` is out of range.
    pub fn play_from(&mut self, items: &[ResultItem], start: usize) -> Option<ResultItem> {
        let first = items.get(start)?.clone();
        self.items.clear();
        for it in items.iter().skip(start) {
            if self.items.iter().all(|q| q.video_id() != it.video_id()) {
                self.items.push_back(it.clone());
            }
        }
        self.notify();
        Some(first)
    }

    /// The currently-playing (front) item.
    pub fn front(&self) -> Option<ResultItem> {
        self.items.front().cloned()
    }

    /// The current item finished: drop it and return the new front (next to play), if any.
    pub fn advance(&mut self) -> Option<ResultItem> {
        self.items.pop_front();
        self.notify();
        self.items.front().cloned()
    }

    /// Move the item at `i` to the front (panel "play this now"); it stays in the queue.
    /// Returns the new front.
    pub fn move_to_front(&mut self, i: usize) -> Option<ResultItem> {
        if i < self.items.len() {
            if let Some(item) = self.items.remove(i) {
                self.items.push_front(item);
                self.notify();
            }
        }
        self.front()
    }

    /// Remove the item at `i` (panel ×). Out-of-range is a no-op.
    pub fn remove(&mut self, i: usize) {
        if i < self.items.len() {
            self.items.remove(i);
            self.notify();
        }
    }

    /// Move the item at `i` one step toward the front (▲). No-op at the front / out of range.
    pub fn move_up(&mut self, i: usize) {
        if i > 0 && i < self.items.len() {
            self.items.swap(i, i - 1);
            self.notify();
        }
    }

    /// Move the item at `i` one step toward the back (▼). No-op at the back / out of range.
    pub fn move_down(&mut self, i: usize) {
        if i + 1 < self.items.len() {
            self.items.swap(i, i + 1);
            self.notify();
        }
    }

    pub fn clear(&mut self) {
        if !self.items.is_empty() {
            self.items.clear();
            self.notify();
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Snapshot of the queued items (front = currently playing), for rendering the panel.
    pub fn items(&self) -> Vec<ResultItem> {
        self.items.iter().cloned().collect()
    }

    /// Register a panel-refresh hook. Every hook fires after every mutation with the current
    /// items. Hooks are never removed.
    pub fn add_on_change(&mut self, f: OnChange) {
        self.on_change.push(f);
    }

    fn notify(&self) {
        if self.on_change.is_empty() {
            return;
        }
        let snapshot: Vec<ResultItem> = self.items.iter().cloned().collect();
        for f in &self.on_change {
            f(&snapshot);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> ResultItem {
        ResultItem::for_video_id(id)
    }
    fn ids(q: &Queue) -> Vec<String> {
        q.items().iter().map(|i| i.video_id()).collect()
    }

    #[test]
    fn queue_order_follows_play_next_and_add_to_queue() {
        let mut q = Queue::new();
        q.push_back(item("a")); // [a]
        q.push_back(item("b")); // [a, b]
        q.insert_after_current(item("c")); // [a, c, b]
        assert_eq!(ids(&q), ["a", "c", "b"]);
        q.push_front(item("x")); // [x, a, c, b]
        assert_eq!(ids(&q), ["x", "a", "c", "b"]);
    }

    #[test]
    fn finishing_an_item_plays_the_next_in_order() {
        let mut q = Queue::new();
        for id in ["a", "b", "c"] {
            q.push_back(item(id));
        }
        assert_eq!(q.advance().map(|i| i.video_id()).as_deref(), Some("b")); // drop a → front b
        assert_eq!(ids(&q), ["b", "c"]);
        assert_eq!(q.advance().map(|i| i.video_id()).as_deref(), Some("c"));
        assert_eq!(q.advance(), None); // drop c → empty
        assert!(q.is_empty());
    }

    #[test]
    fn play_now_from_the_panel_moves_the_item_to_the_front() {
        let mut q = Queue::new();
        for id in ["a", "b", "c"] {
            q.push_back(item(id));
        }
        assert_eq!(q.move_to_front(2).map(|i| i.video_id()).as_deref(), Some("c"));
        assert_eq!(ids(&q), ["c", "a", "b"], "selected item moves to front, stays in queue");
    }

    #[test]
    fn panel_reorder_and_remove_ignore_out_of_range_rows() {
        let mut q = Queue::new();
        for id in ["a", "b", "c"] {
            q.push_back(item(id));
        }
        q.move_up(2); // a c b
        assert_eq!(ids(&q), ["a", "c", "b"]);
        q.move_up(0); // no-op
        q.move_down(2); // no-op
        assert_eq!(ids(&q), ["a", "c", "b"]);
        q.remove(5); // no-op
        q.remove(1); // remove c
        assert_eq!(ids(&q), ["a", "b"]);
    }

    #[test]
    fn play_from_queues_the_tail_in_order() {
        let list: Vec<ResultItem> = ["a", "b", "c", "d"].iter().map(|id| item(id)).collect();
        let mut q = Queue::new();
        q.push_back(item("old"));
        assert_eq!(q.play_from(&list, 1).map(|i| i.video_id()).as_deref(), Some("b"));
        assert_eq!(ids(&q), ["b", "c", "d"], "chosen item is current; the tail follows it");
    }

    #[test]
    fn play_from_drops_repeats_and_rejects_out_of_range() {
        let list: Vec<ResultItem> = ["a", "b", "a"].iter().map(|id| item(id)).collect();
        let mut q = Queue::new();
        q.play_from(&list, 0);
        assert_eq!(ids(&q), ["a", "b"], "a repeated id keeps its first position only");
        assert!(q.play_from(&list, 9).is_none());
        assert_eq!(ids(&q), ["a", "b"], "an out-of-range start leaves the queue alone");
    }

    #[test]
    fn every_surface_showing_the_queue_is_refreshed() {
        use std::cell::RefCell;
        let a: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
        let b: Rc<RefCell<Vec<usize>>> = Rc::new(RefCell::new(Vec::new()));
        let mut q = Queue::new();
        let (sa, sb) = (a.clone(), b.clone());
        q.add_on_change(Rc::new(move |items| sa.borrow_mut().push(items.len())));
        q.add_on_change(Rc::new(move |items| sb.borrow_mut().push(items.len())));
        q.push_back(item("a"));
        q.push_back(item("b"));
        // The header popover and the watch-page card both render the queue, so the first
        // registration must still be fed.
        assert_eq!(*a.borrow(), vec![1, 2]);
        assert_eq!(*b.borrow(), vec![1, 2]);
    }
}
