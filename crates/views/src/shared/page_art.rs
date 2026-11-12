//! The colour the page's own artwork leads with, published for the chrome to wear.
//!
//! A sidebar beside an artist page should read as that page's colour in shadow, not as the
//! theme's black, or the panel looks like a slab dropped onto the artwork. The wash already
//! samples the artwork for its own field; this only carries the one hue it found so a sibling
//! panel can tint itself with the same colour. It is rewritten only when the page moves.

use gpui::{App, Global, Hsla, SharedString};

#[derive(Default)]
struct PageArt {
    cover: Option<SharedString>,
    tint: Option<Hsla>,
}

impl Global for PageArt {}

/// Publishes the page's artwork and the hue it named. Called from the root every frame, and
/// writes only when the page has changed, so a frame that merely redraws costs a comparison.
pub(crate) fn publish(cover: Option<SharedString>, tint: Option<Hsla>, cx: &mut App) {
    let stale = cx
        .try_global::<PageArt>()
        .is_none_or(|art| art.cover != cover || art.tint != tint);

    if stale {
        cx.set_global(PageArt { cover, tint });
    }
}

/// The hue the page's artwork leads with, once it has been drawn and it has one. Art with no
/// hue of its own answers `None`, and the chrome keeps the theme's surface.
pub(crate) fn tint(cx: &App) -> Option<Hsla> {
    cx.try_global::<PageArt>().and_then(|art| art.tint)
}
