use std::rc::Rc;

use gpui::accesskit::{Role, Toggled};
use gpui::prelude::*;
use gpui::{
    AnimationExt as _, AnyElement, App, Context, Entity, EventEmitter, FontWeight, Pixels, Render,
    ScrollHandle, SharedString, SpringAnimation, WeakEntity, Window, div, linear_color_stop,
    linear_gradient, px, relative,
};

use crate::chrome::Chrome;
use crate::shared::cells;
use i18n::t;
use music::{SavedArtist, Track};
use router::trail;
use state::{ArtistDetail, Origin, Playback, Sonora};
use ui::ActiveTheme as _;
use ui::motion::Springs;
use ui::{
    Artwork, Button, Card, MIN_CONTENT, Picker, Popovers, Scrollbar, Scroller, Text, effects,
    liquid_capsule, scrolled, snapped,
};

use crate::shared::about::{AboutArtist, about_modal};
use crate::shared::album_grid::CardGrid;
use crate::shared::cards::{self, ReleaseFilter};
use crate::shared::confirm::Confirm;
use crate::shared::hero::{HeroMetaStrip, HeroPlayButton, PageHero};
use crate::shared::menus::artist_menu;
use crate::shared::shelves::{Rail, RailSpec};
use crate::shared::track_card::{StartHandler, TrackCard};
use crate::shared::tracks::{PlaybackStatus, playback_status};
use crate::shared::trouble;
use ui::Rail as Carousel;

/// What the floating controls ask of the window: the two sidebars, which a page cannot reach
/// on its own. The title bar carries the same two on every other screen.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtistEvent {
    ToggleSidebar,
    ToggleSidebarRight,
}

impl EventEmitter<ArtistEvent> for ArtistView {}

/// How many shelves the page draws under the releases, each with a rail of its own.
const SHELVES: usize = 4;
/// How many release placeholders the rail shows while the discography is still loading.
const RELEASE_SKELETONS: usize = 6;
/// The popular songs grid: 3 rows x 4 columns = 12 songs. Borderless, horizontal rail when narrow.
const POPULAR_ROWS: usize = 4;
const POPULAR_COLUMNS: usize = 3;
const POPULAR_SONGS: usize = POPULAR_ROWS * POPULAR_COLUMNS;
const POPULAR_GAP: Pixels = px(12.);
/// The narrowest a popular column may get before the section scrolls sideways instead.
const POPULAR_COLUMN: Pixels = px(260.);
/// The artwork each popular row wears, and the space above and below it. The rows are the page's
/// main shelf, so they are larger and airier than a table row rather than packed into it.
const POPULAR_ART: Pixels = px(80.);
const POPULAR_ROW_PAD: Pixels = px(8.);
/// Where the artist's name sits down the hero, as a share of the page's height, and where the
/// frost begins. The photograph stays sharp above the frost and blurs through the lower half.
const HERO_TOP: f32 = 0.46;
/// The band along the top of the page kept clear for the floating controls, and how far below it
/// the page's content fades away as it scrolls up. The content is gone by the top of the band and
/// only the artwork behind it shows.
const TOP_CLEAR: Pixels = px(72.);
const TOP_FADE: Pixels = px(150.);
const FROST_START: f32 = 0.45;
const FROST_END: f32 = 0.55;
const FROST_TRANSITION: f32 = FROST_END - FROST_START;
/// How much wider than the page the banner is drawn, so it still reaches both edges while the
/// page width it was measured from lags a resize by a frame.
const BANNER_SLACK: f32 = 0.06;
/// The blurs the banner steps through on its way down, each with the share of the banner's
/// height at which it has fully taken over from the one above. The top stays sharp and the
/// bottom half reads as frosted glass.
const BANNER_BLURS: [(f32, Pixels); 6] = [
    (0.48, px(4.)),
    (0.56, px(8.)),
    (0.64, px(13.)),
    (0.73, px(19.)),
    (0.83, px(26.)),
    (0.94, px(34.)),
];
/// How much of the page colour lies over the whole banner, so it sits back behind the page.
const BANNER_DIM: f32 = 0.18;
/// The shade that deepens toward the banner's bottom on top of `BANNER_DIM`: where it starts, as
/// a share of the way down the banner, and how much of the page colour it reaches at the bottom.
const BANNER_SHADE_FROM: f32 = 0.28;
const BANNER_SHADE: f32 = 0.62;
/// The blur over a portrait standing in for a missing banner, enough to leave only its colours.
const BANNER_WASH: Pixels = px(48.);

pub(crate) struct ArtistView {
    detail: Entity<ArtistDetail>,
    playback: Entity<Playback>,
    playback_status: PlaybackStatus,
    artist_id: Option<String>,
    release_filter: ReleaseFilter,
    width: Pixels,
    scrollbar: Entity<Scrollbar>,
    about_bar: Entity<Scrollbar>,
    about_open: bool,
    /// The songs the popular grid shows: the provider caps the list at `POPULAR_SONGS`, so the
    /// grid and the play buttons above it both work from the same run.
    popular: Rc<Vec<Track>>,
    me: WeakEntity<Self>,
    popovers: Popovers,
    /// The recommendation rails under the releases, in page order: appears-on.
    rails: Vec<Rail>,
}

impl ArtistView {
    pub(crate) fn new(
        detail: Entity<ArtistDetail>,
        playback: Entity<Playback>,
        cx: &mut Context<Self>,
    ) -> Self {
        let width = MIN_CONTENT;
        let id = cx.entity_id();
        let scrollbar = cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(id));

        cx.subscribe(&detail, |_, _, event, cx| {
            let state::ArtistDetailEvent::Gone(id) = event;
            if router::trail(cx).read(cx).current()
                == router::Destination::Artist(id.clone().into())
            {
                router::replace(router::Destination::Local(router::LibraryTab::Artists), cx);
            }
        })
        .detach();

        cx.observe(&detail, |this, detail, cx| {
            let artist_id = detail.read(cx).id().map(str::to_owned);
            if this.artist_id != artist_id {
                this.artist_id = artist_id;
                this.release_filter = ReleaseFilter::All;
                this.about_open = false;
                this.rails.clear();
                this.scrollbar.update(cx, |bar, cx| {
                    bar.set_max_offset(None, cx);
                    bar.scroll().set_offset(gpui::Point::default());
                });
            }
            this.popular = Rc::new(
                detail
                    .read(cx)
                    .tracks()
                    .iter()
                    .take(POPULAR_SONGS)
                    .cloned()
                    .collect(),
            );
            cx.notify();
        })
        .detach();
        let chrome = Chrome::entity(cx);
        cx.observe(&chrome, |_, _, cx| cx.notify()).detach();

        let library = Sonora::global(cx).library.clone();
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        let current_playback = playback_status(&playback, cx);
        let artist_id = detail.read(cx).id().map(str::to_owned);
        cx.observe(&playback, |this, playback, cx| {
            let current = playback_status(&playback, cx);
            if this.playback_status == current {
                return;
            }
            this.playback_status = current;
            cx.notify();
        })
        .detach();

        let me = cx.entity();

        Self {
            popular: Rc::new(
                detail
                    .read(cx)
                    .tracks()
                    .iter()
                    .take(POPULAR_SONGS)
                    .cloned()
                    .collect(),
            ),
            detail,
            playback,
            playback_status: current_playback,
            artist_id,
            release_filter: ReleaseFilter::All,
            width,
            scrollbar,
            about_bar: cx.new(|_| Scrollbar::new(ScrollHandle::new()).watching(id)),
            about_open: false,
            me: me.downgrade(),
            popovers: Popovers::default(),
            rails: Vec::new(),
        }
    }

    fn header(&self, cx: &Context<Self>) -> AnyElement {
        let artist = self.detail.read(cx).artist();
        let title = artist
            .map(|artist| SharedString::from(artist.name.clone()))
            .unwrap_or_default();
        // Both counts the page reports: the listeners a month and, where the provider keeps one,
        // the subscriber count beside it.
        let facts = artist.and_then(|artist| {
            let mut facts = HeroMetaStrip::new();
            let mut any = false;
            if let Some(count) = artist.monthly_listeners {
                let value = cells::count(count);
                facts = facts.text(t!(
                    "artist-monthly-listeners",
                    count = count,
                    value = &value
                ));
                any = true;
            }
            if let Some(count) = artist.subscribers {
                let value = cells::count(count);
                facts = facts.text(t!("artist-subscribers", count = count, value = &value));
                any = true;
            }
            any.then_some(facts)
        });
        let overflow = self.saved_artist(cx).map(|artist| {
            Picker::icon("artist-overflow", &self.popovers, "icons/ellipsis.svg")
                .tooltip("common-more")
                .large()
                .left()
                .menu(artist_menu(artist, self.playback.clone(), true, cx))
        });
        let actions = div()
            .flex()
            .items_center()
            .gap_2()
            .child(
                HeroPlayButton::new(
                    "play-artist",
                    t!("artist-play"),
                    self.popular.as_ref().clone(),
                    self.playback.clone(),
                )
                .from(self.playing_from(cx)),
            )
            .child(
                HeroPlayButton::shuffle(
                    "shuffle-artist",
                    self.popular.as_ref().clone(),
                    self.playback.clone(),
                )
                .from(self.playing_from(cx)),
            )
            .children(self.favorite_button(cx))
            .children(overflow);

        // The banner is the hero here — no square avatar, just name, stats and play buttons on the artwork.
        PageHero::new("artist-hero", title)
            .pending(artist.is_none() && self.detail.read(cx).is_loading())
            .pin(None)
            .cover(None)
            .art(px(0.))
            .title_size(cx.theme().text(Text::Display) * 1.9)
            .when_some(facts, |hero, facts| hero.meta(facts))
            .actions(actions)
            .into_any_element()
    }

    /// The static backdrop image that fills the entire window behind the page.
    /// It never moves — it's the fixed background.
    fn backdrop(&self, inset: Pixels, height: Pixels, cx: &App) -> Option<AnyElement> {
        let artist = self.detail.read(cx).artist()?;
        let (url, wash) = match &artist.banner {
            Some(banner) => (banner.clone(), None),
            None if effects() => (artist.cover_large.clone()?, Some(BANNER_WASH)),
            None => return None,
        };
        let width = (self.width + inset * 2.) * (1. + BANNER_SLACK);
        let theme = cx.theme();
        let spread = BANNER_WASH * 2.;

        Some(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .h(height)
                .overflow_hidden()
                .map(|this| match wash {
                    Some(wash) => this.child(
                        Artwork::new(Some(url.clone()))
                            .size(width + spread * 2.)
                            .wide(height + spread * 2.)
                            .overlay()
                            .absolute()
                            .left(relative(0.5))
                            .ml(-(width / 2. + spread))
                            .top(-spread)
                            .blur(wash),
                    ),
                    None => this
                        .when(effects(), |this| {
                            this.child(
                                Artwork::new(Some(url.clone()))
                                    .size(width + spread * 2.)
                                    .wide(height + spread * 2.)
                                    .overlay()
                                    .absolute()
                                    .left(relative(0.5))
                                    .ml(-(width / 2. + spread))
                                    .top(-spread)
                                    .blur(BANNER_WASH),
                            )
                        })
                        .child(
                            Artwork::new(Some(url))
                                .size(width + spread * 2.)
                                .wide(height + spread * 2.)
                                .overlay()
                                .absolute()
                                .left(relative(0.5))
                                .ml(-(width / 2. + spread))
                                .top(-spread),
                        ),
                })
                .child(
                    div()
                        .absolute()
                        .inset_0()
                        .bg(theme.background.opacity(BANNER_DIM)),
                )
                .into_any_element(),
        )
    }

    /// The frost layer that scrolls with the page.
    /// It starts at 45% of viewport, has a 10% transition zone (45%-55%),
    /// and moves up as the user scrolls.
    fn frost(&self, inset: Pixels, height: Pixels, offset: Pixels, cx: &App) -> Option<AnyElement> {
        let artist = self.detail.read(cx).artist()?;
        let (url, wash) = match &artist.banner {
            Some(banner) => (banner.clone(), None),
            None if effects() => (artist.cover_large.clone()?, Some(BANNER_WASH)),
            None => return None,
        };
        let width = (self.width + inset * 2.) * (1. + BANNER_SLACK);
        let theme = cx.theme();
        let spread = BANNER_WASH * 2.;

        // The frost edge starts at FROST_START and moves up with scroll.
        // offset is how far the page has scrolled (positive = scrolled down).
        let transition_start = height * FROST_START - offset;

        // One blurred copy of the backdrop — constant blur strength.
        let frosted_picture = Artwork::new(Some(url))
            .size(width + spread * 2.)
            .wide(height + spread * 2.)
            .overlay()
            .absolute()
            .left(relative(0.5))
            .ml(-(width / 2. + spread))
            .top(-spread)
            .blur(match wash {
                Some(w) => w,
                None => BANNER_BLURS[BANNER_BLURS.len() - 1].1,
            });

        let shade = div().absolute().inset_0().bg(linear_gradient(
            180.,
            linear_color_stop(theme.background.opacity(0.), BANNER_SHADE_FROM),
            linear_color_stop(theme.background.opacity(BANNER_SHADE), 1.),
        ));

        Some(
            div()
                .absolute()
                .top(transition_start)
                .left_0()
                .right_0()
                .bottom_0()
                .overflow_hidden()
                // Fade in over the transition zone so the edge is soft.
                .fade_edges(height * FROST_TRANSITION, Pixels::ZERO)
                .child(
                    div()
                        .absolute()
                        .top(-transition_start)
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .overflow_hidden()
                        .when(effects(), |this| this.child(frosted_picture))
                        .child(shade),
                )
                .into_any_element(),
        )
    }

    fn saved_artist(&self, cx: &App) -> Option<SavedArtist> {
        let detail = self.detail.read(cx);
        let artist = detail.artist()?;

        Some(SavedArtist {
            id: detail.id()?.to_owned(),
            name: artist.name.clone(),
            cover: artist.cover_large.clone(),
            added_at: None,
        })
    }

    fn favorite_button(&self, cx: &App) -> Option<Button> {
        let theme = *cx.theme();
        // A provider with no followed artists has nothing for this to toggle.
        if !Sonora::global(cx)
            .session
            .read(cx)
            .capabilities()
            .follow_artists
        {
            return None;
        }
        let library = Sonora::global(cx).library.clone();
        let target = self.saved_artist(cx)?;
        let saved = library.read(cx).saved_artist(&target.id);

        let heart = Button::new("artist-toggle-library")
            .outline()
            .icon(match saved {
                true => "icons/heart-filled.svg",
                false => "icons/heart.svg",
            })
            .tooltip(match saved {
                true => "menu-remove-from-library",
                false => "menu-add-to-library",
            })
            .disabled(library.read(cx).pending_artist(&target.id));

        Some(
            match saved {
                true => heart.tint(theme.primary),
                false => heart,
            }
            .on_click(move |_, _, cx| {
                match saved && !Confirm::unstarring(&target.id, cx) {
                    true => Confirm::artists(vec![target.clone()], cx),
                    false => {
                        library.update(cx, |library, cx| library.heart_artist(target.clone(), cx));
                    }
                }
            }),
        )
    }

    /// The Releases shelf: every release that passes the filter, in one row that scrolls
    /// sideways like the page's other shelves. Nothing frames the cards.
    fn releases(&self, _window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        let theme = *cx.theme();
        let detail = self.detail.read(cx);
        let albums = detail.albums();
        // A discography still on its way reads as loading only while nothing of it is up.
        let loading = detail.is_loading() || (albums.is_empty() && detail.is_filling());
        if albums.is_empty() && !loading {
            return None;
        }
        let local = detail.id().is_some_and(music::is_local_id);
        let filters = match loading {
            true => Vec::new(),
            false => cards::release_filters(local, albums.iter().map(|album| album.release_type)),
        };
        let tile = CardGrid::layout(self.width).card;

        let shelf = match loading {
            true => Carousel::new("artist-releases-rail")
                .show_arrows(false)
                .children((0..RELEASE_SKELETONS).map(|index| {
                    Card::skeleton(("artist-release-skeleton", index))
                        .tile(tile)
                        .into_any_element()
                }))
                .into_any_element(),
            false => {
                let shown: Vec<&music::Album> = albums
                    .iter()
                    .filter(|album| self.release_filter.matches(album.release_type))
                    .collect();
                let total = shown.len();
                Carousel::new("artist-releases-rail")
                    .show_arrows(false)
                    .children(shown.into_iter().enumerate().map(|(index, album)| {
                        let id = SharedString::from(format!("artist-release-{}", album.id));
                        cards::album_card(id, album, &self.playback, cx)
                            .tile(tile)
                            .flat()
                            .aria_position_in_set(index + 1)
                            .aria_size_of_set(total)
                            .into_any_element()
                    }))
                    .into_any_element()
            }
        };

        Some(
            div()
                .flex()
                .flex_col()
                .gap_3()
                .pt_6()
                .child({
                    let label = t!("artist-releases");
                    div()
                        .id("artist-releases-heading")
                        .role(Role::Heading)
                        .aria_level(2)
                        .aria_label(label.clone())
                        .text_size(theme.text(Text::Title))
                        .font_weight(FontWeight::BOLD)
                        .child(label)
                })
                .when(!filters.is_empty(), |this| {
                    this.child(
                        div()
                            .flex()
                            .gap_1()
                            .children(filters.into_iter().map(|filter| {
                                Button::new(filter.id())
                                    .label(filter.label())
                                    .small()
                                    .outline()
                                    .selected(self.release_filter == filter)
                                    .aria_toggled(Toggled::from(self.release_filter == filter))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if this.release_filter == filter {
                                            return;
                                        }
                                        this.release_filter = filter;
                                        cx.notify();
                                    }))
                            })),
                    )
                })
                .child(shelf)
                .into_any_element(),
        )
    }

    /// The recommendation rail under the releases: what the artist guests on. A rail
    /// still on its way reads as skeletons only while nothing of it is up.
    fn recommended(
        &self,
        window: &Window,
        notify: &Rc<dyn Fn(&mut App)>,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let grid = CardGrid::layout(self.width);
        let card = grid.card;
        let columns = grid.columns.max(1);
        let detail = self.detail.read(cx);
        let filling = detail.is_filling();
        let appears = detail.appears_on().len();
        let shelves = detail.artist().map(|artist| &artist.shelves);
        let featured = shelves.map_or(0, |shelves| shelves.featured_on.len());
        let kept = shelves.map_or(0, |shelves| shelves.artist.len());
        let fans = shelves.map_or(0, |shelves| shelves.fans.len());
        let name = detail.artist().map(|artist| artist.name.clone());
        let mut rails = Vec::new();

        // What the artist guests on, which only the catalogue fill knows.
        rails.push(match appears {
            0 if filling => Some(Rail::pending(
                t!("artist-appears-on"),
                card,
                columns,
                window,
                cx,
            )),
            0 => None,
            _ => {
                let opened = self.me.clone();
                Some(self.rails[0].render(
                    RailSpec {
                        tag: "artist-appears",
                        place: 0,
                        title: t!("artist-appears-on"),
                        count: appears,
                        tile: card,
                        columns,
                        tabs: None,
                    },
                    window,
                    cx,
                    notify,
                    move |index, _, cx| {
                        let Some(view) = opened.upgrade() else {
                            return div().into_any_element();
                        };
                        let held = view.read(cx);
                        let detail = held.detail.read(cx);
                        let Some(album) = detail.appears_on().get(index) else {
                            return div().into_any_element();
                        };
                        let id = SharedString::from(format!("artist-appears-{}", album.id));
                        cards::album_card(id, album, &held.playback, cx)
                            .tile(card)
                            .flat()
                            .aria_position_in_set(index + 1)
                            .aria_size_of_set(appears)
                            .into_any_element()
                    },
                ))
            }
        });

        // The playlists the artist is featured on, editorial ones from the provider.
        rails.push(match featured {
            0 => None,
            _ => {
                let opened = self.me.clone();
                Some(self.rails[1].render(
                    RailSpec {
                        tag: "artist-featured",
                        place: 1,
                        title: t!("artist-featured-on"),
                        count: featured,
                        tile: card,
                        columns,
                        tabs: None,
                    },
                    window,
                    cx,
                    notify,
                    move |index, _, cx| {
                        let Some(view) = opened.upgrade() else {
                            return div().into_any_element();
                        };
                        let held = view.read(cx);
                        let Some(playlist) = held
                            .detail
                            .read(cx)
                            .artist()
                            .and_then(|artist| artist.shelves.featured_on.get(index))
                        else {
                            return div().into_any_element();
                        };
                        let id = SharedString::from(format!("artist-featured-{}", playlist.id));
                        cards::playlist_card(id, playlist, &held.playback, cx)
                            .tile(card)
                            .flat()
                            .aria_position_in_set(index + 1)
                            .aria_size_of_set(featured)
                            .into_any_element()
                    },
                ))
            }
        });

        // The playlists the artist keeps themself.
        rails.push(match kept {
            0 => None,
            _ => {
                let opened = self.me.clone();
                let title = match &name {
                    Some(name) => t!("artist-playlists-by", name = name.as_str()),
                    None => t!("artist-playlists"),
                };
                Some(self.rails[2].render(
                    RailSpec {
                        tag: "artist-playlists",
                        place: 2,
                        title,
                        count: kept,
                        tile: card,
                        columns,
                        tabs: None,
                    },
                    window,
                    cx,
                    notify,
                    move |index, _, cx| {
                        let Some(view) = opened.upgrade() else {
                            return div().into_any_element();
                        };
                        let held = view.read(cx);
                        let Some(playlist) = held
                            .detail
                            .read(cx)
                            .artist()
                            .and_then(|artist| artist.shelves.artist.get(index))
                        else {
                            return div().into_any_element();
                        };
                        let id = SharedString::from(format!("artist-playlists-{}", playlist.id));
                        cards::playlist_card(id, playlist, &held.playback, cx)
                            .tile(card)
                            .flat()
                            .aria_position_in_set(index + 1)
                            .aria_size_of_set(kept)
                            .into_any_element()
                    },
                ))
            }
        });

        // The artists close to this one, with the listeners their own captions report.
        rails.push(match fans {
            0 => None,
            _ => {
                let opened = self.me.clone();
                Some(self.rails[3].render(
                    RailSpec {
                        tag: "artist-fans",
                        place: 3,
                        title: t!("artist-fans-also-like"),
                        count: fans,
                        tile: card,
                        columns,
                        tabs: None,
                    },
                    window,
                    cx,
                    notify,
                    move |index, _, cx| {
                        let Some(view) = opened.upgrade() else {
                            return div().into_any_element();
                        };
                        let held = view.read(cx);
                        let Some(fan) = held
                            .detail
                            .read(cx)
                            .artist()
                            .and_then(|artist| artist.shelves.fans.get(index))
                        else {
                            return div().into_any_element();
                        };
                        let saved = SavedArtist {
                            id: fan.id.clone(),
                            name: fan.name.clone(),
                            cover: fan.cover.clone(),
                            added_at: None,
                        };
                        let id = SharedString::from(format!("artist-fan-{}", fan.id));
                        let card = cards::artist_card(id, &saved, &held.playback, cx)
                            .tile(card)
                            .flat()
                            .aria_position_in_set(index + 1)
                            .aria_size_of_set(fans);
                        match fan.listeners {
                            Some(count) => {
                                let value = cells::count(count);
                                card.meta(t!(
                                    "artist-monthly-listeners",
                                    count = count,
                                    value = &value
                                ))
                            }
                            None => card,
                        }
                        .into_any_element()
                    },
                ))
            }
        });

        rails
            .into_iter()
            .flatten()
            .map(|rail| div().pt_6().child(rail).into_any_element())
            .collect()
    }

    fn playing_from(&self, cx: &App) -> Option<Origin> {
        let detail = self.detail.read(cx);
        let name = detail.artist()?.name.clone();

        Some(Origin::artist(detail.id()?).named(name))
    }

    /// The popular songs as a borderless grid, three rows by four columns, that turns into a rail
    /// when the page is too narrow to hold all four columns. Nothing frames the rows: no card
    /// border, no separators, just the songs over the frost.
    fn popular(&self, width: Pixels, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let tracks = self.popular.clone();
        let from = self.playing_from(cx);
        let playback = self.playback.clone();
        let loading = self.detail.read(cx).is_loading();
        let column = ((width - POPULAR_GAP * (POPULAR_COLUMNS - 1) as f32)
            / POPULAR_COLUMNS as f32)
            .max(POPULAR_COLUMN);

        let mut columns = Vec::with_capacity(POPULAR_COLUMNS);
        for column_index in 0..POPULAR_COLUMNS {
            let mut rows = Vec::with_capacity(POPULAR_ROWS);
            for row_index in 0..POPULAR_ROWS {
                // Row-major, so the first row reads as the artist's first four songs.
                let place = row_index * POPULAR_COLUMNS + column_index;
                if place >= tracks.len() {
                    if loading {
                        rows.push(
                            Card::skeleton(("artist-popular-skeleton", place))
                                .art(POPULAR_ART)
                                .py(POPULAR_ROW_PAD)
                                .into_any_element(),
                        );
                    }
                    continue;
                }
                let queued = tracks.clone();
                let origin = from.clone();
                let player = playback.clone();
                let start: StartHandler = Rc::new(move |place, cx| {
                    player.update(cx, |playback, cx| {
                        playback.start(queued.as_ref().clone(), place, origin.clone(), cx)
                    });
                });
                rows.push(
                    TrackCard::new("artist-popular", place, tracks.clone(), playback.clone())
                        .detailed(true)
                        .start(Some(start))
                        .render(cx)
                        .art(POPULAR_ART)
                        .py(POPULAR_ROW_PAD)
                        .into_any_element(),
                );
            }
            columns.push(
                div()
                    .flex()
                    .flex_none()
                    .flex_col()
                    .w(column)
                    .children(rows)
                    .into_any_element(),
            );
        }

        let body = match tracks.is_empty() && !loading {
            true => ui::vacant(i18n::lookup("artist-popular-empty", None), cx).into_any_element(),
            false => Carousel::new("artist-popular-rail")
                .show_arrows(false)
                .children(columns)
                .into_any_element(),
        };

        div()
            .flex()
            .flex_col()
            .gap_3()
            .pt_4()
            .child({
                let label = t!("artist-popular");
                div()
                    .id("artist-popular-heading")
                    .role(Role::Heading)
                    .aria_level(2)
                    .aria_label(label.clone())
                    .text_size(theme.text(Text::Title))
                    .font_weight(FontWeight::BOLD)
                    .child(label)
            })
            .child(body)
            .into_any_element()
    }

    fn about(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let artist = self.detail.read(cx).artist()?;
        let card = AboutArtist::new("artist-about", artist.name.clone())
            .cover(artist.cover_large.clone())
            .biography(artist.biography.clone())
            .on_open(cx.listener(|this, _, _, cx| {
                this.about_open = true;
                cx.notify();
            }));

        Some(div().pt_6().child(card).into_any_element())
    }

    /// Closes the About the artist dialog when it is open, and says whether it was.
    pub(crate) fn escape(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.about_open {
            return false;
        }
        self.about_open = false;
        cx.notify();
        true
    }

    fn about_dialog(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.about_open {
            return None;
        }
        let artist = self.detail.read(cx).artist()?;

        Some(
            about_modal(
                artist.name.clone().into(),
                artist.biography.clone(),
                None,
                &self.about_bar,
                cx,
            )
            .action(
                Button::new("artist-about-close")
                    .label(t!("common-dismiss"))
                    .primary()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.about_open = false;
                        cx.notify();
                    })),
            )
            .on_dismiss(cx.listener(|this, _, _, cx| {
                this.about_open = false;
                cx.notify();
            }))
            .into_any_element(),
        )
    }

    /// The page an artist shows instead of its hero and its tables when it cannot be read: the
    /// No connection state as soon as the network is gone, whatever this page happens to hold,
    /// and the failure of its own load otherwise. Opening the same artist again is the retry.
    fn failure(&self, cx: &Context<Self>) -> Option<AnyElement> {
        let id = self.detail.read(cx).id()?.to_owned();
        let reason = match trouble::unreachable(&id, cx) {
            true => None,
            false => Some(self.detail.read(cx).error()?.to_owned()),
        };
        let detail = self.detail.clone();

        Some(
            trouble::lost(
                "artist-lost",
                t!("trouble-not-loaded"),
                reason.as_deref(),
                move |_, _, cx| {
                    let id = id.clone();
                    detail.update(cx, |detail, cx| detail.open(&id, cx));
                },
                cx,
            )
            .size_full()
            .into_any_element(),
        )
    }
}

impl Render for ArtistView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(failure) = self.failure(cx) {
            return div()
                .id("artist-failure")
                .role(Role::Alert)
                .relative()
                .size_full()
                .child(failure)
                .into_any_element();
        }

        let theme = *cx.theme();
        let inset = theme.metrics.inset;
        let previous = self.width;
        // Update width for releases grid
        let available = (window.viewport_size().width - inset * 2.).max(MIN_CONTENT);
        if (available - self.width).abs() >= px(0.5) {
            self.width = available;
        }

        let scroll = self.scrollbar.read(cx).scroll().clone();
        let offset = scrolled(&scroll);
        if self.width != previous {
            self.scrollbar
                .update(cx, |bar, cx| bar.set_max_offset(None, cx));
        }

        let head = self.header(cx);
        // The hero is only as tall as the name and play buttons.
        // The full artist image is the static backdrop behind everything.
        let bar = match Sonora::global(cx)
            .settings
            .read(cx)
            .player_bar_translucent()
        {
            true => Pixels::ZERO,
            false => crate::chrome::PlayerBar::height(window, cx),
        };
        let height = snapped((window.viewport_size().height - bar).max(px(320.)), window);
        let backdrop = self.backdrop(inset, height, cx);
        let frost = self.frost(inset, height, offset, cx);
        let tracks = self.popular(self.width, cx);
        let grid = self.releases(window, cx);
        let about = self.about(cx);
        // One rail per shelf the page can carry: what the artist guests on, the playlists it is
        // featured on and keeps, and the artists close to it.
        while self.rails.len() < SHELVES {
            self.rails.push(Rail::new(cx.entity_id()));
        }
        for rail in &self.rails {
            rail.sync();
        }
        let weak = cx.entity().downgrade();
        let notify: Rc<dyn Fn(&mut App)> = Rc::new(move |cx: &mut App| {
            weak.update(cx, |_, cx| cx.notify()).ok();
        });
        let rails = self.recommended(window, &notify, cx);
        let page = Scroller::new("artist-page", &self.scrollbar)
            .bottom_inset(crate::chrome::Chrome::bottom(cx))
            .pl(inset)
            .pt(inset)
            .pb(inset)
            // The hero content starts at HERO_TOP (46% of viewport).
            // The frost layer begins at FROST_START (45%) and moves with scroll.
            .child(
                div()
                    .relative()
                    .pt((height * HERO_TOP - TOP_CLEAR).max(Pixels::ZERO))
                    .child(head),
            )
            .child(tracks)
            .children(grid)
            .children(rails)
            .children(about);

        // The band along the top kept clear for the floating controls: the page starts below it
        // and its content fades in under that, so nothing ever reads over the controls. Only the
        // artwork behind shows through the cleared band.
        let page = div()
            .absolute()
            .top(TOP_CLEAR)
            .left_0()
            .right_0()
            .bottom_0()
            .fade_edges(TOP_FADE, Pixels::ZERO)
            .child(page);

        // Floating hero controls at the very top of the page (like album/playlist pages),
        // standing in for the title bar on this fullbleed page.
        let hero_controls = {
            let theme = *cx.theme();
            let navigation = trail(cx);
            let (can_back, can_forward) = {
                let navigation = navigation.read(cx);
                (navigation.can_go_back(), navigation.can_go_forward())
            };
            let settings = Sonora::global(cx).settings.clone();
            let (sidebar_open, queue_open) = {
                let settings = settings.read(cx);
                (settings.sidebar_open(), settings.sidebar_right_open())
            };

            let back = navigation.clone();
            let forward = navigation.clone();
            let history = liquid_capsule(cx)
                .flex()
                .items_center()
                .child(
                    Button::new("artist-back")
                        .ghost()
                        .icon("icons/chevron-left.svg")
                        .tooltip("nav-back")
                        .size_10()
                        .px_0()
                        .rounded_full()
                        .disabled(!can_back)
                        .on_click(move |_, _, cx| back.update(cx, |nav, cx| nav.back(cx))),
                )
                .child(div().flex_none().w(px(1.)).h(px(14.)).bg(theme.border))
                .child(
                    Button::new("artist-forward")
                        .ghost()
                        .icon("icons/chevron-right.svg")
                        .tooltip("nav-forward")
                        .size_10()
                        .px_0()
                        .rounded_full()
                        .disabled(!can_forward)
                        .on_click(move |_, _, cx| forward.update(cx, |nav, cx| nav.forward(cx))),
                )
                .child(div().flex_none().w(px(1.)).h(px(14.)).bg(theme.border))
                .child(
                    Button::new("artist-refresh")
                        .ghost()
                        .icon("icons/refresh-cw.svg")
                        .tooltip("app-refresh")
                        .size_10()
                        .px_0()
                        .rounded_full()
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(input::Refresh), cx)
                        }),
                );

            let sidebar = liquid_capsule(cx).child(
                Button::new("artist-sidebar")
                    .ghost()
                    .icon(match sidebar_open {
                        true => "icons/panel-left-close.svg",
                        false => "icons/panel-left-open.svg",
                    })
                    .tooltip("nav-sidebar")
                    .size_10()
                    .px_0()
                    .rounded_full()
                    .aria_expanded(sidebar_open)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ArtistEvent::ToggleSidebar)))
                    .with_spring(
                        "artist-sidebar-swing",
                        SpringAnimation::new(Springs::RESPONSIVE)
                            .to(match sidebar_open {
                                true => 1.,
                                false => 0.,
                            })
                            .with_epsilon(0.01),
                        |button, t| button.turned(t * (1. - t)),
                    ),
            );
            let queue = liquid_capsule(cx).child(
                Button::new("artist-sidebar-right")
                    .ghost()
                    .icon(match queue_open {
                        true => "icons/panel-right-close.svg",
                        false => "icons/panel-right-open.svg",
                    })
                    .tooltip("nav-sidebar-right")
                    .size_10()
                    .px_0()
                    .rounded_full()
                    .aria_expanded(queue_open)
                    .on_click(cx.listener(|_, _, _, cx| cx.emit(ArtistEvent::ToggleSidebarRight)))
                    .with_spring(
                        "artist-sidebar-right-swing",
                        SpringAnimation::new(Springs::RESPONSIVE)
                            .to(match queue_open {
                                true => 1.,
                                false => 0.,
                            })
                            .with_epsilon(0.01),
                        |button, t| button.turned(t * (1. - t)),
                    ),
            );

            div()
                .flex()
                .items_center()
                .justify_between()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(sidebar)
                        .child(history),
                )
                .child(queue)
                .into_any_element()
        };

        div()
            .relative()
            .size_full()
            // The backdrop is static — it never moves.
            .children(backdrop)
            // The frost scrolls with the page.
            .children(frost)
            // Hero controls float at the very top, over the backdrop.
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .px(inset)
                    .pt(inset)
                    .child(hero_controls),
            )
            .child(page)
            .children(self.about_dialog(cx))
            .into_any_element()
    }
}
