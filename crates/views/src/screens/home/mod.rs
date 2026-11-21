use std::rc::Rc;
use std::time::Instant;

use crate::chrome::Chrome;
use crate::shared::menus::{Item, ItemMenu};
use crate::shared::page_controls::{self, PageEvent};
use gpui::accesskit::Role;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, Context, Entity, EventEmitter, MouseDownEvent, Pixels, Point, Render,
    ScrollHandle, WeakEntity, Window, div, px,
};
use i18n::t;
use music::GenreItem;
use state::{Home, Network, Playback, Sonora};
use ui::{ActiveTheme as _, Arriving as _, Lazy, Mode, Popup, Scrollbar, Scroller};

use crate::shared::cells;
use crate::shared::picks::Picks;
use crate::shared::shelves::Shelves;
use crate::shared::tracks::{PlaybackStatus, playback_status};
use crate::shared::trouble;

const STEADY: Pixels = px(0.5);

pub(crate) struct HomeView {
    home: Entity<Home>,
    playback: Entity<Playback>,
    playback_status: PlaybackStatus,
    shelves: Entity<Shelves>,
    width: Pixels,
    scrollbar: Entity<Scrollbar>,
    /// The submenu state of a track's context menu.
    menus: ItemMenu,
    context_menu: Option<(Item, Point<Pixels>)>,
    /// Skeleton shelves stood on the previous render, to spot the moment data replaces them.
    pending_shown: bool,
    /// When that replacement happened, while the arrival window is open.
    arrived_at: Option<Instant>,
}

impl EventEmitter<PageEvent> for HomeView {}

impl HomeView {
    pub(crate) fn new(
        home: Entity<Home>,
        playback: Entity<Playback>,
        cx: &mut Context<Self>,
    ) -> Self {
        let me = cx.entity_id();
        let playlist_scrollbar = cx.new(|_| Scrollbar::inset().watching(me));
        let menus = ItemMenu::new(playlist_scrollbar, cx);

        cx.observe(&home, |this, _, cx| {
            this.menus.reset(cx);
            this.context_menu = None;
            cx.notify();
        })
        .detach();
        let chrome = Chrome::entity(cx);
        cx.observe(&chrome, |_, _, cx| cx.notify()).detach();

        let library = Sonora::global(cx).library.clone();
        cx.observe(&library, |_, _, cx| cx.notify()).detach();

        let shelves = cx.new(|_| Shelves::new("home-shelf", me, playback.clone()));
        cx.observe(&shelves, |_, _, cx| cx.notify()).detach();

        let current_playback = playback_status(&playback, cx);
        cx.observe(&playback, |this, playback, cx| {
            let current = playback_status(&playback, cx);
            if this.playback_status != current {
                this.playback_status = current;
                cx.notify();
            }
        })
        .detach();

        Self {
            home,
            playback,
            playback_status: current_playback,
            shelves,
            width: Pixels::ZERO,
            scrollbar: cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(me)),
            menus,
            context_menu: None,
            pending_shown: false,
            arrived_at: None,
        }
    }

    /// Tells the feed whether this page is the one on screen, so it lands changes to what is
    /// drawn only while nobody is looking.
    pub(crate) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        self.home
            .update(cx, |home, cx| home.set_visible(visible, cx));
    }

    /// Fetches the feed and the library it mixes from again.
    pub(crate) fn refresh(&mut self, cx: &mut Context<Self>) {
        self.home.update(cx, |home, cx| home.refresh(cx));
    }

    /// Whether Quick picks or the shelves are still on their way.
    pub(crate) fn is_loading(&self, cx: &App) -> bool {
        let home = self.home.read(cx);
        home.is_loading(cx) || home.is_feeding()
    }

    /// The Quick picks lead row: the one row of tall cards on the page, what the provider lists
    /// as played lately and its picks after them, skeletons while they are on their way.
    fn quick_picks(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let items = self.home.read(cx).quick_picks();
        let opened = items.clone();
        let home = cx.entity().downgrade();

        Picks::mixed("quick-picks", items, self.playback.clone())
            .title("home-quick-picks")
            .vacancy("home-quick-picks-empty")
            .loading(self.home.read(cx).is_loading(cx))
            .on_context_menu(move |place, event, _, cx| {
                open_item_menu(&home, &opened, place, event, cx);
            })
            .into_any_element()
    }

    /// The page home shows in place of its shelves: the No connection state the moment the
    /// network is gone, since every row here comes from the provider, and the reason its feed
    /// failed otherwise. Asking for the feed again is the retry, and it starts the pauses
    /// between tries over.
    fn failure(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let reason = match Network::lost(cx) {
            true => None,
            false => Some(self.home.read(cx).error()?.to_owned()),
        };
        let home = self.home.clone();

        Some(
            trouble::lost(
                "home-lost",
                t!("trouble-not-loaded"),
                reason.as_deref(),
                move |_, _, cx| {
                    home.update(cx, |home, cx| home.retry(cx));
                },
                cx,
            )
            .size_full()
            .into_any_element(),
        )
    }
}

impl Render for HomeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        // The page runs to the top of the window, so the title bar's buttons float over it
        // instead, and ask the shell for the two sidebars.
        let me = cx.entity();
        let controls = page_controls::floating(
            "home",
            move |event, _, cx| {
                me.update(cx, |_, cx| cx.emit(event));
            },
            cx,
        );

        let page = match self.failure(cx) {
            Some(failure) => div()
                .id("home-failure")
                .role(Role::Alert)
                .size_full()
                .child(failure)
                .into_any_element(),
            None => {
                let available = cells::content_width(window, theme.metrics.inset * 2., cx);
                if (available - self.width).abs() >= STEADY {
                    self.width = available;
                }
                let context_menu = self.context_menu.clone().map(|(item, position)| {
                    let menu = item.menu(&self.menus, self.playback.clone(), false, cx);
                    Popup::new(position, menu).on_close(cx.listener(|this, _, _, cx| {
                        this.context_menu = None;
                        cx.notify();
                    }))
                });

                let view = cx.entity();
                let picks = Lazy::new("home-quick-picks", move |_, cx| {
                    view.update(cx, |this, cx| this.quick_picks(cx))
                });
                let sections = self.home.read(cx).sections();
                let feeding = self.home.read(cx).is_feeding();
                let width = self.width;
                // Skeleton shelves stood on the previous render and real ones now replace
                // them: open the arrival window so the shelves stagger into place once.
                if sections.is_empty() && feeding {
                    self.pending_shown = true;
                } else if self.pending_shown && self.arrived_at.is_none() {
                    self.arrived_at = Some(Instant::now());
                }
                if self
                    .arrived_at
                    .is_some_and(|arrived| arrived.elapsed() > ui::Arrival::span())
                {
                    self.arrived_at = None;
                }
                let arrival = self.arrived_at;
                let mut shelves =
                    self.shelves
                        .update(cx, |shelves, cx| match sections.is_empty() && feeding {
                            true => shelves.pending(width, cx),
                            false => vec![shelves.render(sections, Mode::Grid, width, window, cx)],
                        });
                if let Some(_) = arrival {
                    shelves = shelves
                        .into_iter()
                        .enumerate()
                        .map(|(index, shelf)| match index < ui::Arrival::SLOTS {
                            true => div()
                                .child(shelf)
                                .arriving(("home-shelf", index), index)
                                .into_any_element(),
                            false => shelf,
                        })
                        .collect();
                }

                Scroller::new("home-page", &self.scrollbar)
                    .bottom_inset(crate::chrome::Chrome::bottom(cx))
                    // The content starts below the band the floating controls stand in and
                    // melts away under them as it scrolls up.
                    .fade_top(theme.metrics.inset + page_controls::HEIGHT + theme.metrics.pad)
                    .p(theme.metrics.inset)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_8()
                            .child(picks)
                            .children(shelves),
                    )
                    .when_some(context_menu, |this, menu| this.child(menu))
                    .into_any_element()
            }
        };

        div()
            .relative()
            .size_full()
            .child(page)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .px(theme.metrics.inset)
                    .pt(theme.metrics.inset)
                    .child(controls),
            )
            .into_any_element()
    }
}

fn open_item_menu(
    home: &WeakEntity<HomeView>,
    items: &Rc<Vec<GenreItem>>,
    place: usize,
    event: &MouseDownEvent,
    cx: &mut gpui::App,
) {
    let Some(item) = items.get(place).and_then(Item::of) else {
        return;
    };
    let Some(home) = home.upgrade() else {
        return;
    };
    home.update(cx, |this, cx| {
        this.menus.reset(cx);
        this.context_menu = Some((item, event.position));
        cx.notify();
    });
}
