//! The colours a panel draws its text with when it sits over a page's artwork.
//!
//! An album, playlist or artist page lets its artwork's field reach the panels beside it, so a
//! panel is as light or as dark as the picture behind it. The theme's polarity cannot answer
//! that: a dark theme over a bright banner still needs dark text. Each panel knows its own fill
//! — the left takes its sidebar colour, the right its background — and this works out what that
//! fill shows over the page's corner, then names the two colours the panel needs: the neutral
//! ink, white on a dark panel and black on a light one, and the song's accent, stepped down to a
//! darker shade of itself on a light panel so the colour survives rather than washing out.

use gpui::{App, Hsla};
use router::Destination;

use crate::shared::{page_art, wash};

/// The panel lightness at which black overtakes white as the more readable ink. Below it white
/// is the better of the two, above it black, and at the crossover they are equally poor — so
/// there is no middle to get wrong. It is also what separates the two shades the accent wears.
const PIVOT: f32 = 0.46;

/// The lightness the accent steps down to on a light panel. It is the lightness the theme
/// already gives its accent on a light theme, so the same shade appears whichever theme named
/// the song: the lyrics carry two shades at most, the song's own and this darker one.
const LIGHT_ACCENT: f32 = 0.42;

/// How much saturation the shade gains per step down in lightness. Taking lightness down alone
/// mutes a hue — a purple turns grey rather than dark purple — so the colour is pushed back up
/// as it darkens and keeps reading as itself.
const SATURATION_LIFT: f32 = 0.6;

/// How far a panel's ink falls back for the text that carries less than the words themselves:
/// credits, source lines, and the controls beside them.
pub(crate) const MUTED: f32 = 0.65;

/// What a panel shows, in the colours it takes to draw on it.
#[derive(Clone, Copy)]
pub(crate) struct Panel {
    /// The neutral ink: white on a dark panel, black on a light one.
    pub ink: Hsla,
    /// Whether the panel sits on the light side of `PIVOT`, which is what decides the shade the
    /// accent wears rather than any measurement of the art.
    pub light: bool,
}

/// The ink of a panel whose own colour is `fill`. On a page without artwork, or while the art
/// has not been sampled yet, the theme's own background is what the panel shows, so its
/// foreground stands. The palette is resolved here rather than published ahead, so the ink
/// changes on the same frame the field paints the artwork on, not a frame behind it.
pub(crate) fn panel(fill: Hsla, theme: &ui::Theme, cx: &App) -> Panel {
    let current = router::trail(cx).read(cx).current();
    let frosted = matches!(
        current,
        Destination::Home
            | Destination::Album(_)
            | Destination::Playlist(_)
            | Destination::Artist(_)
    );
    let art = page_art::cover(cx)
        .and_then(|cover| ui::cover_palette(&cover, cx))
        .filter(|_| frosted);
    let lightness = match art {
        Some(palette) => {
            // What the panel shows: its own fill at `SIDEBAR_FROST_TINT` over the page's field.
            // The field is the artwork the page stands on, darkened by the wash's own shade,
            // so the corner of the picture is what the panel has to read against.
            let shade = match current {
                Destination::Artist(_) => wash::ARTIST_SHADE,
                _ => wash::SHADE,
            };
            let field = palette.corner * (1. - shade) + theme.background.l * shade;
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

    Panel { ink, light }
}

/// The song's accent as the panel draws it. A dark panel keeps the colour the theme names. A
/// light panel takes it down to the darker shade — and a colour already that dark is left where
/// it is — with the hue kept and a little saturation added back, so the shade reads as dark
/// purple or dark pink rather than a grey of the same hue.
pub(crate) fn accent(accent: Hsla, panel: Panel) -> Hsla {
    match panel.light {
        true => {
            let l = accent.l.min(LIGHT_ACCENT);
            let s = accent.s + (accent.l - l) * SATURATION_LIFT;

            Hsla {
                l,
                s: s.min(1.),
                ..accent
            }
        }
        false => accent,
    }
}
