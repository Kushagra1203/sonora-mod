use std::rc::Rc;

use gpui::accesskit::Role;
use gpui::prelude::*;
use gpui::{
    AnyElement, App, ClickEvent, Entity, FontWeight, MouseButton, MouseDownEvent, Pixels,
    SharedString, Window, div, linear_color_stop, linear_gradient, px,
};
use i18n::t;
use music::GenreItem;
use router::{Destination, navigate};
use state::{Origin, Playback};
use ui::{ActiveTheme as _, Artwork, Button, Rail, heading, vacant};

use crate::shared::cards;

/// The width of one tall pick. A portrait card, like the lead row of a music service's home:
/// taller than it is wide, so the page opens on photographs rather than another square of
/// sleeves.
const CARD_WIDTH: Pixels = px(212.);
/// Height over width. Apple's lead cards are a 3:4 portrait.
const CARD_ASPECT: f32 = 4. / 3.;
/// Where the caption's foot begins, as a share of the card's height, so the picture darkens
/// under the words however bright it is.
const FOOT: f32 = 0.42;
/// The group the play button waits on, so it is out until the pointer is over the card.
const GROUP: &str = "top-pick";

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;
type MenuHandler = Rc<dyn Fn(usize, &MouseDownEvent, &mut Window, &mut App)>;

fn card_height() -> Pixels {
    px(CARD_WIDTH / px(1.) * CARD_ASPECT)
}

/// What a pick says: the small line over its name, the name, and who it is by.
struct Face {
    cover: Option<String>,
    eyebrow: SharedString,
    title: SharedString,
    subtitle: SharedString,
}

impl Face {
    fn of(item: &GenreItem) -> Self {
        match item {
            GenreItem::Album(album) => Self {
                cover: album.cover_large.clone().or_else(|| album.cover.clone()),
                eyebrow: i18n::lookup(cards::release_key(album.release_type), None),
                title: SharedString::from(album.name.clone()),
                subtitle: SharedString::from(album.artists.clone()),
            },
            GenreItem::Playlist(playlist) => Self {
                cover: playlist.cover.clone(),
                eyebrow: t!("kind-playlist"),
                title: SharedString::from(playlist.name.clone()),
                subtitle: SharedString::from(playlist.owner.clone()),
            },
            GenreItem::Artist(artist) => Self {
                cover: artist.cover.clone(),
                eyebrow: t!("kind-artist"),
                title: SharedString::from(artist.name.clone()),
                subtitle: SharedString::default(),
            },
            GenreItem::Genre(genre) => Self {
                cover: genre.cover.clone(),
                eyebrow: SharedString::default(),
                title: SharedString::from(genre.name.clone()),
                subtitle: SharedString::default(),
            },
            GenreItem::Track(track) => Self {
                cover: track.cover.clone(),
                eyebrow: t!("kind-song"),
                title: SharedString::from(track.name.clone()),
                subtitle: SharedString::from(track.artists.clone()),
            },
        }
    }
}

/// A lead row of tall cards for the home feed: the picks a listener's own playing has earned,
/// each a portrait sleeve with its name on the picture. It replaces a deck of framed list rows,
/// which read as one more box on a page already made of them.
#[derive(IntoElement)]
pub(crate) struct Picks {
    id: &'static str,
    title: &'static str,
    vacancy: &'static str,
    items: Rc<Vec<GenreItem>>,
    playback: Entity<Playback>,
    loading: bool,
    on_context_menu: Option<MenuHandler>,
}

impl Picks {
    pub(crate) fn mixed(
        id: &'static str,
        items: Rc<Vec<GenreItem>>,
        playback: Entity<Playback>,
    ) -> Self {
        Self {
            id,
            title: "",
            vacancy: "",
            items,
            playback,
            loading: false,
            on_context_menu: None,
        }
    }

    pub(crate) fn title(mut self, key: &'static str) -> Self {
        self.title = key;
        self
    }

    pub(crate) fn vacancy(mut self, key: &'static str) -> Self {
        self.vacancy = key;
        self
    }

    pub(crate) fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    pub(crate) fn on_context_menu(
        mut self,
        handler: impl Fn(usize, &MouseDownEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_context_menu = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Picks {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *cx.theme();
        let id = self.id;
        let empty = self.items.is_empty();
        let barren = empty && !self.loading;
        let height = card_height();
        let count = self.items.len();

        let cards: Vec<AnyElement> = match empty {
            true => (0..6)
                .map(|_| {
                    div()
                        .flex_none()
                        .w(CARD_WIDTH)
                        .h(height)
                        .rounded(theme.radius)
                        .bg(theme.table_hover)
                        .into_any_element()
                })
                .collect(),
            false => self
                .items
                .iter()
                .enumerate()
                .map(|(place, item)| {
                    pick(
                        id,
                        place,
                        item,
                        count,
                        &self.playback,
                        self.on_context_menu.clone(),
                        cx,
                    )
                })
                .collect(),
        };

        div()
            .w_full()
            .flex()
            .flex_col()
            .gap_3()
            .child({
                let title = i18n::lookup(self.title, None);
                heading(title.clone(), cx)
                    .id(SharedString::from(format!("{id}-heading")))
                    .role(Role::Heading)
                    .aria_level(2)
                    .aria_label(title)
            })
            .when(barren, |this| {
                let vacancy = i18n::lookup(self.vacancy, None);
                this.child(
                    vacant(vacancy.clone(), cx)
                        .id(SharedString::from(format!("{id}-vacancy")))
                        .role(Role::Label)
                        .aria_value(vacancy),
                )
            })
            .when(!barren, |this| {
                this.child(Rail::new(SharedString::from(format!("{id}-rail"))).children(cards))
            })
    }
}

/// One tall pick: the sleeve filling the card, the picture darkening into its foot, and the
/// name on the dark. Tapping it opens the shelf or starts the track, and its play control does
/// the same thing the card does on any other shelf.
fn pick(
    id: &'static str,
    place: usize,
    item: &GenreItem,
    count: usize,
    playback: &Entity<Playback>,
    menu: Option<MenuHandler>,
    cx: &mut App,
) -> AnyElement {
    let theme = *cx.theme();
    let face = Face::of(item);
    let (press, play, playing) = actions(item, playback, cx);

    div()
        .id((id, place))
        .relative()
        .flex_none()
        .w(CARD_WIDTH)
        .h(card_height())
        .rounded(theme.radius)
        .overflow_hidden()
        .group(GROUP)
        .cursor_pointer()
        .role(Role::Button)
        .aria_label(face.title.clone())
        .aria_position_in_set(place + 1)
        .aria_size_of_set(count)
        .when_some(menu, |this, handler| {
            this.on_mouse_down(MouseButton::Right, move |event, window, cx| {
                handler(place, event, window, cx)
            })
        })
        .on_click(move |event, window, cx| press(event, window, cx))
        .child(
            div().absolute().inset_0().child(
                Artwork::new(face.cover.clone())
                    .size(CARD_WIDTH)
                    .wide(card_height())
                    .fallback("icons/music.svg"),
            ),
        )
        // The picture darkens into its foot, so the words read over any cover.
        .child(div().absolute().inset_0().bg(linear_gradient(
            180.,
            linear_color_stop(theme.background.opacity(0.), 1. - FOOT),
            linear_color_stop(theme.background.opacity(0.82), 1.),
        )))
        .child(
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .p_3()
                .child(
                    div()
                        .text_size(theme.text(ui::Text::Small))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.muted_foreground)
                        .truncate()
                        .child(face.eyebrow.clone()),
                )
                .child(
                    div()
                        .text_size(theme.text(ui::Text::Title))
                        .font_weight(FontWeight::BOLD)
                        .truncate()
                        .child(face.title.clone()),
                )
                .when(!face.subtitle.is_empty(), |this| {
                    this.child(
                        div()
                            .text_size(theme.text(ui::Text::Small))
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(face.subtitle.clone()),
                    )
                }),
        )
        .child(
            div()
                .id("pick-play")
                .absolute()
                .right_2()
                .bottom_2()
                .when(!playing, |this| {
                    this.invisible().group_hover(GROUP, |style| style.visible())
                })
                .child(
                    Button::new(SharedString::from(format!("{id}-play-{place}")))
                        .primary()
                        .icon(match playing {
                            true => "icons/pause-filled.svg",
                            false => "icons/play-filled.svg",
                        })
                        .tooltip(match playing {
                            true => "play-pause",
                            false => "common-play",
                        })
                        .rounded_full()
                        .size(px(34.))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click(move |event, window, cx| {
                            cx.stop_propagation();
                            play(event, window, cx);
                        }),
                ),
        )
        .into_any_element()
}

/// What tapping the card does, what its play control does, and whether it is the thing playing.
fn actions(
    item: &GenreItem,
    playback: &Entity<Playback>,
    cx: &App,
) -> (ClickHandler, ClickHandler, bool) {
    match item {
        GenreItem::Album(album) => {
            let origin = Origin::album(album.id.clone()).named(album.name.clone());
            let destination = Destination::Album(SharedString::from(album.id.clone()));
            let playing = playing_from(&origin, playback, cx);
            (page(destination), toggle(origin, playback), playing)
        }
        GenreItem::Playlist(playlist) => {
            let origin = Origin::playlist(playlist.id.clone()).named(playlist.name.clone());
            let destination = Destination::Playlist(SharedString::from(playlist.id.clone()));
            let playing = playing_from(&origin, playback, cx);
            (page(destination), toggle(origin, playback), playing)
        }
        GenreItem::Artist(artist) => {
            let origin = Origin::artist(artist.id.clone()).named(artist.name.clone());
            let destination = Destination::Artist(SharedString::from(artist.id.clone()));
            let playing = playing_from(&origin, playback, cx);
            (page(destination), toggle(origin, playback), playing)
        }
        GenreItem::Track(track) => {
            let seed = track.clone();
            let pressed = track.clone();
            let current = cards::track_status(track, playback, cx).0;
            let radio = playback.clone();
            let toggled = playback.clone();
            let press = Rc::new(move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                let track = pressed.clone();
                radio.update(cx, |playback, cx| playback.play_radio(&track, cx));
            }) as ClickHandler;
            let play = Rc::new(
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| match current {
                    true => toggled.update(cx, |playback, cx| playback.toggle_play(cx)),
                    false => {
                        let seed = seed.clone();
                        toggled.update(cx, |playback, cx| playback.play_radio(&seed, cx))
                    }
                },
            ) as ClickHandler;
            (press, play, current)
        }
        GenreItem::Genre(genre) => {
            let press = page(Destination::Genre(SharedString::from(genre.id.clone())));
            (press.clone(), press, false)
        }
    }
}

fn playing_from(origin: &Origin, playback: &Entity<Playback>, cx: &App) -> bool {
    playback.read(cx).playing_from(origin, cx) == Some(true)
}

fn page(destination: Destination) -> ClickHandler {
    Rc::new(move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        navigate(destination.clone(), cx);
    })
}

fn toggle(origin: Origin, playback: &Entity<Playback>) -> ClickHandler {
    let playback = playback.clone();
    Rc::new(move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        playback.update(cx, |playback, cx| playback.toggle_origin(&origin, cx));
    })
}
