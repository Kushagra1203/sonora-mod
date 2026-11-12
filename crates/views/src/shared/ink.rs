//! The colours a panel draws its text with when it sits over a page's artwork.
//!
//! An album, playlist or artist page lets its artwork's field reach the panels beside it, so a
//! panel is as light or as dark as the picture behind it. The theme's polarity cannot answer
//! that: a dark theme over a bright banner still needs dark text. Each panel knows its own fill
//! — the left takes its sidebar colour, the right its background — and this works out what that
//! fill shows over the page's corner, then names the two colours the panel needs: the neutral
//! ink, white on a dark panel and black on a light one, and the surface an accent is judged
//! against so a song's own colour stays readable rather than washing out.

use gpui::{App, Hsla, Rgba};
use router::Destination;

use crate::shared::{page_art, wash};

/// The panel lightness at which black overtakes white as the more readable ink. Below it white
/// is the better of the two, above it black, and at the crossover they are equally poor — so
/// there is no middle to get wrong.
const PIVOT: f32 = 0.46;

/// What coloured text has to clear against the panel behind it, the ratio WCAG asks of small
/// text. A song's accent is held to it, so a hue the eye reads as bright is pushed darker (or
/// lighter) than one it reads as dim rather than both sharing a lightness.
pub(crate) const TEXT_CONTRAST: f32 = 4.5;

/// How far a panel's ink falls back for the text that carries less than the words themselves:
/// credits, source lines, and the controls beside them.
pub(crate) const MUTED: f32 = 0.65;

/// What a panel shows, in the colours it takes to draw on it.
#[derive(Clone, Copy)]
pub(crate) struct Panel {
    /// The neutral ink: white on a dark panel, black on a light one.
    pub ink: Hsla,
    /// The panel's own colour as the eye meets it, which an accent is held against.
    pub surface: Hsla,
}

/// The ink and surface of a panel whose own colour is `fill`. On a page without artwork, or
/// while the art has not been sampled yet, the theme's own background is what the panel shows,
/// so its foreground stands. The palette is resolved here rather than published ahead, so the
/// colours change on the same frame the wash paints the artwork on, not a frame behind it.
pub(crate) fn panel(fill: Hsla, theme: &ui::Theme, cx: &App) -> Panel {
    let current = router::trail(cx).read(cx).current();
    let frosted = matches!(
        current,
        Destination::Album(_) | Destination::Playlist(_) | Destination::Artist(_)
    );
    let art = page_art::cover(cx)
        .and_then(|cover| ui::cover_palette(&cover, cx))
        .map(|palette| palette.corner)
        .filter(|_| frosted);
    let lightness = match art {
        Some(art) => {
            let shade = match matches!(current, Destination::Artist(_)) {
                true => wash::ARTIST_SHADE,
                false => wash::SHADE,
            };
            // What the panel shows: its own fill at `SIDEBAR_FROST_TINT` over the page's field.
            let field = art * (1. - shade) + theme.background.l * shade;
            fill.l * wash::SIDEBAR_FROST_TINT + field * (1. - wash::SIDEBAR_FROST_TINT)
        }
        None => theme.background.l,
    };
    let light = lightness > PIVOT;

    // The theme carries two inks: its foreground and its background. Whichever is on the far
    // side of the panel's brightness is the one that reads.
    let ink = match light == (theme.background.l > 0.5) {
        true => theme.foreground,
        false => theme.background,
    };
    let surface = Hsla {
        h: 0.,
        s: 0.,
        l: lightness,
        a: 1.,
    };

    Panel { ink, surface }
}

/// The song's accent moved in lightness — hue and saturation untouched — until it clears
/// `TEXT_CONTRAST` against the panel. A hue the panel already reads is returned as it is, so a
/// dark theme's bright accent stays bright; a bright accent over a light panel is darkened to
/// the nearest shade that reads instead of staying washed out.
pub(crate) fn accent(accent: Hsla, panel: Panel) -> Hsla {
    legible(accent, panel.surface)
}

/// `ink` moved in lightness — hue and saturation untouched — until it reads against `behind`.
/// A colour that already clears the goal is returned unchanged, and one that does not takes the
/// nearest lightness that does, so an accent stays as close to the hue the artwork named as the
/// floor allows.
fn legible(ink: Hsla, behind: Hsla) -> Hsla {
    if contrast(ink, behind) >= TEXT_CONTRAST {
        return ink;
    }

    // Lightness is monotonic in luminance, so each direction can be searched for the lightness
    // closest to the one asked for that still clears the goal.
    let reach = |to: f32| {
        let end = Hsla { l: to, ..ink };
        if contrast(end, behind) < TEXT_CONTRAST {
            return None;
        }

        let (mut clears, mut fails) = (to, ink.l);
        for _ in 0..10 {
            let middle = (clears + fails) / 2.;
            match contrast(Hsla { l: middle, ..ink }, behind) >= TEXT_CONTRAST {
                true => clears = middle,
                false => fails = middle,
            }
        }
        Some(Hsla { l: clears, ..ink })
    };

    let (down, up) = (reach(0.), reach(1.));
    match (down, up) {
        (Some(down), Some(up)) => match (down.l - ink.l).abs() <= (up.l - ink.l).abs() {
            true => down,
            false => up,
        },
        (Some(down), None) => down,
        (None, Some(up)) => up,
        // Neither end of the panel clears the goal: keep the hue and go as far as it can.
        (None, None) => {
            let (black, white) = (Hsla { l: 0., ..ink }, Hsla { l: 1., ..ink });

            match contrast(white, behind) > contrast(black, behind) {
                true => white,
                false => black,
            }
        }
    }
}

/// The contrast ratio between a colour and the surface behind it, from 1 (the same) to 21
/// (black on white).
fn contrast(ink: Hsla, behind: Hsla) -> f32 {
    let (shown, surface) = (luminance(ink), luminance(behind));
    let (bright, dim) = match shown > surface {
        true => (shown, surface),
        false => (surface, shown),
    };

    (bright + 0.05) / (dim + 0.05)
}

/// The relative luminance of an opaque colour, the base WCAG's ratio is built on.
fn luminance(colour: Hsla) -> f32 {
    let rgb = Rgba::from(colour);
    let channel = |channel: f32| match channel <= 0.03928 {
        true => channel / 12.92,
        false => ((channel + 0.055) / 1.055).powf(2.4),
    };

    0.2126 * channel(rgb.r) + 0.7152 * channel(rgb.g) + 0.0722 * channel(rgb.b)
}
