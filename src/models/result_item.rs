use innertube::SearchItem;

/// URL of the 320x180 (16:9) `mqdefault` thumbnail, which exists for any public video id.
/// `hqdefault` is 4:3 with letterbox bars.
pub fn thumbnail_url(video_id: &str) -> String {
    format!("https://i.ytimg.com/vi/{video_id}/mqdefault.jpg")
}

/// The video fields of a card. Channels and playlists carry none of them and are built
/// separately in [`ResultItem::from_search_item`].
struct VideoFields {
    id: String,
    title: String,
    channel: String,
    channel_id: Option<String>,
    channel_avatar: Option<String>,
    views: Option<String>,
    published: Option<String>,
    duration: Option<String>,
    live: bool,
    moving_thumbnail: Option<String>,
    preview_player_params: Option<String>,
}

fn video_fields(item: &SearchItem) -> Option<VideoFields> {
    match item {
        // Drop members-only videos: they fail to play for non-members. Filtering here hides
        // them from every list: feed, search, channel pages and related.
        SearchItem::Video { members_only: true, .. } => None,
        SearchItem::Video {
            id, title, channel, channel_id, channel_avatar, views, published, duration, live,
            moving_thumbnail, preview_player_params, ..
        } => {
            Some(VideoFields {
                id: id.clone(),
                title: title.clone(),
                channel: channel.clone().unwrap_or_default(),
                channel_id: channel_id.clone(),
                channel_avatar: channel_avatar.clone(),
                views: views.clone(),
                published: published.clone(),
                duration: duration.clone(),
                live: *live,
                moving_thumbnail: moving_thumbnail.clone(),
                preview_player_params: preview_player_params.clone(),
            })
        }
        _ => None,
    }
}

/// The kind of card an item renders as: a video, a channel entity (direct-match channel
/// search; click opens the channel), or a playlist (click opens the ordered playlist view
/// and queues play-through).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum ItemKind {
    #[default]
    Video,
    Channel,
    Playlist,
    /// Opens in the Shorts player.
    Short,
    /// A feed's Shorts shelf: a full-width row of `Short` items.
    ShortsShelf,
}

use gtk4 as gtk;
use gtk::glib;
use gtk::subclass::prelude::*;
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ResultItem {
        pub kind: Cell<super::ItemKind>,
        pub video_id: RefCell<String>,
        pub title: RefCell<String>,
        pub channel: RefCell<String>,
        pub channel_id: RefCell<Option<String>>,
        /// The uploader's avatar on a video card.
        pub channel_avatar: RefCell<Option<String>>,
        pub views: RefCell<Option<String>>,
        pub published: RefCell<Option<String>>,
        pub duration: RefCell<Option<String>>,
        /// The signed `an_webp` animated-preview URL, when the response carried one. YouTube
        /// sends it where a surface previews with an image instead of a stream.
        pub moving_thumbnail: RefCell<Option<String>>,
        /// The item's inline-preview `playerParams`, for the hover-preview `player()` POST.
        pub preview_player_params: RefCell<Option<String>>,
        pub live: Cell<bool>,
        /// Channel-card fields: the entity's avatar URL and subscriber count. For a `Channel`
        /// item, `channel_id` and `title` hold the id and name.
        pub avatar_url: RefCell<Option<String>>,
        pub subscribers: RefCell<Option<String>>,
        /// A Short card's endpoint, for the Shorts player.
        pub reel: RefCell<Option<innertube::ReelEntry>>,
        /// A `ShortsShelf`'s Shorts, and whether its Show more is open.
        pub shelf: RefCell<Vec<super::ResultItem>>,
        pub shelf_expanded: Cell<bool>,
        /// A shelf that is a whole grid (a channel's Shorts tab): no heading, all shown.
        pub shelf_grid: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ResultItem {
        const NAME: &'static str = "MtResultItem";
        type Type = super::ResultItem;
    }

    impl ObjectImpl for ResultItem {}
}

glib::wrapper! {
    pub struct ResultItem(ObjectSubclass<imp::ResultItem>);
}

impl ResultItem {
    /// Builds from a `SearchItem`. Videos render as cards; a direct-match channel renders as
    /// a channel entity card and a playlist as a card that opens the ordered playlist page.
    pub fn from_search_item(item: &SearchItem) -> Option<Self> {
        if let SearchItem::Channel { id, title, avatar, subscribers } = item {
            let obj: Self = glib::Object::new();
            let imp = obj.imp();
            imp.kind.set(ItemKind::Channel);
            imp.title.replace(title.clone());
            imp.channel_id.replace(Some(id.clone()));
            imp.avatar_url.replace(avatar.clone());
            imp.subscribers.replace(subscribers.clone());
            return Some(obj);
        }
        if let SearchItem::Playlist { id, title, thumbnail } = item {
            return Some(Self::for_playlist(id, title, thumbnail.clone()));
        }
        if let SearchItem::Short { title, views, thumbnail, reel } = item {
            let obj: Self = glib::Object::new();
            let imp = obj.imp();
            imp.kind.set(ItemKind::Short);
            imp.video_id.replace(reel.video_id.clone());
            imp.title.replace(title.clone());
            imp.views.replace(views.clone());
            imp.avatar_url.replace(thumbnail.clone());
            imp.reel.replace(Some(reel.clone()));
            return Some(obj);
        }
        if let SearchItem::ShortsShelf { title, items } = item {
            let shorts: Vec<Self> = items.iter().filter_map(Self::from_search_item).collect();
            let obj: Self = glib::Object::new();
            let imp = obj.imp();
            imp.kind.set(ItemKind::ShortsShelf);
            imp.title.replace(title.clone());
            imp.video_id.replace(shorts.first().map(Self::video_id).unwrap_or_default());
            imp.shelf.replace(shorts);
            return Some(obj);
        }
        let f = video_fields(item)?;
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.video_id.replace(f.id);
        imp.title.replace(f.title);
        imp.channel.replace(f.channel);
        imp.channel_id.replace(f.channel_id);
        imp.channel_avatar.replace(f.channel_avatar);
        imp.views.replace(f.views);
        imp.published.replace(f.published);
        imp.duration.replace(f.duration);
        imp.moving_thumbnail.replace(f.moving_thumbnail);
        imp.preview_player_params.replace(f.preview_player_params);
        imp.live.set(f.live);
        Some(obj)
    }

    /// Construct directly from a video id (used by the `autoplay` test hook).
    pub fn for_video_id(id: &str) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().video_id.replace(id.to_string());
        obj
    }

    pub fn kind(&self) -> ItemKind {
        self.imp().kind.get()
    }
    pub fn is_channel(&self) -> bool {
        self.imp().kind.get() == ItemKind::Channel
    }
    pub fn is_playlist(&self) -> bool {
        self.imp().kind.get() == ItemKind::Playlist
    }
    pub fn is_shorts_shelf(&self) -> bool {
        self.imp().kind.get() == ItemKind::ShortsShelf
    }
    pub fn shelf_items(&self) -> Vec<ResultItem> {
        self.imp().shelf.borrow().clone()
    }
    pub fn shelf_expanded(&self) -> bool {
        self.imp().shelf_expanded.get()
    }
    pub fn set_shelf_expanded(&self, on: bool) {
        self.imp().shelf_expanded.set(on);
    }
    pub fn is_shelf_grid(&self) -> bool {
        self.imp().shelf_grid.get()
    }
    /// A shelf item holding `shorts` as one portrait grid.
    pub fn shorts_grid(shorts: Vec<ResultItem>) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.kind.set(ItemKind::ShortsShelf);
        imp.video_id.replace(shorts.first().map(Self::video_id).unwrap_or_default());
        imp.shelf.replace(shorts);
        imp.shelf_grid.set(true);
        imp.shelf_expanded.set(true);
        obj
    }
    pub fn extend_shelf(&self, more: Vec<ResultItem>) {
        self.imp().shelf.borrow_mut().extend(more);
    }
    /// The Shorts endpoint of a `Short` card.
    pub fn reel(&self) -> Option<innertube::ReelEntry> {
        self.imp().reel.borrow().clone()
    }
    /// The playlist id for a `Playlist` card (stored in the `video_id` slot).
    pub fn playlist_id(&self) -> String {
        self.imp().video_id.borrow().clone()
    }
    /// Build a `Playlist` item directly (search card, library, the `playlist`
    /// test hook, or a playlist link). `thumbnail` is the lead video's image, if known.
    pub fn for_playlist(id: &str, title: &str, thumbnail: Option<String>) -> Self {
        let obj: Self = glib::Object::new();
        let imp = obj.imp();
        imp.kind.set(ItemKind::Playlist);
        imp.video_id.replace(id.to_string());
        imp.title.replace(title.to_string());
        // Non-video cards store their image URL in `avatar_url` (channel avatar / playlist
        // lead thumbnail); `thumbnail_url()` returns it for those kinds.
        imp.avatar_url.replace(thumbnail);
        obj
    }
    pub fn avatar_url(&self) -> Option<String> {
        self.imp().avatar_url.borrow().clone()
    }
    pub fn subscribers(&self) -> Option<String> {
        self.imp().subscribers.borrow().clone()
    }
    pub fn video_id(&self) -> String {
        self.imp().video_id.borrow().clone()
    }
    pub fn title(&self) -> String {
        self.imp().title.borrow().clone()
    }
    pub fn channel(&self) -> String {
        self.imp().channel.borrow().clone()
    }
    /// The uploader's `UC…` channel id, if known (search results have it; the autoplay
    /// debug hook and some lockups don't).
    pub fn channel_id(&self) -> Option<String> {
        self.imp().channel_id.borrow().clone()
    }
    pub fn channel_avatar(&self) -> Option<String> {
        self.imp().channel_avatar.borrow().clone()
    }
    pub fn views(&self) -> Option<String> {
        self.imp().views.borrow().clone()
    }
    pub fn published(&self) -> Option<String> {
        self.imp().published.borrow().clone()
    }
    pub fn duration(&self) -> Option<String> {
        self.imp().duration.borrow().clone()
    }
    pub fn live(&self) -> bool {
        self.imp().live.get()
    }
    /// The signed `an_webp` animated-preview URL, if the response carried one.
    ///
    /// YouTube sends it only on surfaces where a hover shows an animated image instead of
    /// opening a player session (the watch sidebar), so its presence selects the preview kind.
    pub fn moving_thumbnail(&self) -> Option<String> {
        self.imp().moving_thumbnail.borrow().clone()
    }

    /// The item's inline-preview `playerParams`, for a hover preview's `player()` POST.
    pub fn preview_player_params(&self) -> Option<String> {
        self.imp().preview_player_params.borrow().clone()
    }

    /// The card's thumbnail URL. Videos derive it from the id (mqdefault); a playlist
    /// card uses its stored lead-video thumbnail (`avatar_url`), if any.
    pub fn thumbnail_url(&self) -> Option<String> {
        if self.is_playlist() {
            self.avatar_url()
        } else {
            Some(thumbnail_url(&self.video_id()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use innertube::SearchItem;

    #[test]
    fn a_channel_result_becomes_a_channel_card() {
        let item = SearchItem::Channel {
            id: "UC1".into(),
            title: "ch".into(),
            avatar: Some("https://a/av.jpg".into()),
            subscribers: Some("1.2M subscribers".into()),
        };
        // Not a video, but it renders as a channel entity card.
        assert!(video_fields(&item).is_none());
        let ri = ResultItem::from_search_item(&item).expect("channel maps to a card");
        assert!(ri.is_channel());
        assert_eq!(ri.channel_id().as_deref(), Some("UC1"));
        assert_eq!(ri.title(), "ch");
        assert_eq!(ri.subscribers().as_deref(), Some("1.2M subscribers"));
    }

    #[test]
    fn a_shorts_shelf_keeps_every_short_in_order() {
        let short = |id: &str| SearchItem::Short {
            title: format!("t{id}"),
            views: None,
            thumbnail: Some(format!("https://p/{id}.jpg")),
            reel: innertube::ReelEntry {
                video_id: id.into(),
                player_params: None,
                params: None,
                thumbnail: None,
                prefetched: None,
                sequence_params: None,
                is_ad: false,
            },
        };
        let shelf = SearchItem::ShortsShelf { title: "Shorts".into(), items: vec![short("a"), short("b")] };
        let ri = ResultItem::from_search_item(&shelf).expect("shelf maps");
        assert!(ri.is_shorts_shelf());
        assert_eq!(ri.title(), "Shorts");
        let ids: Vec<_> = ri.shelf_items().iter().map(ResultItem::video_id).collect();
        assert_eq!(ids, ["a", "b"]);
        assert!(ri.shelf_items()[0].reel().is_some());
        assert!(!ri.shelf_expanded());
    }
}
