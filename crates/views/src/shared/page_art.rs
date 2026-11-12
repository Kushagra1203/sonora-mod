//! The page's own artwork, published for the panels beside it.
//!
//! An album, playlist or artist page lets its artwork's field sit behind the sidebars, so the
//! panel over it is as light or as dark as the picture is. The sidebar's own ink cannot be
//! chosen from the theme's polarity for that reason: a dark theme over a bright banner needs
//! dark text. This carries the one measurement that answers it — the mean lightness of the
//! artwork's top-left corner, where the left panel sits — and is rewritten only when the page
//! moves.

use gpui::{App, Global, SharedString};

#[derive(Default)]
struct PageArt {
    cover: Option<SharedString>,
    corner: Option<f32>,
}

impl Global for PageArt {}

/// Publishes the page's artwork and the lightness of the corner the left panel shows. Called
/// from the root every frame, and writes only when the page has changed, so a frame that merely
/// redraws costs a comparison.
pub(crate) fn publish(cover: Option<SharedString>, corner: Option<f32>, cx: &mut App) {
    let stale = cx
        .try_global::<PageArt>()
        .is_none_or(|art| art.cover != cover || art.corner != corner);

    if stale {
        cx.set_global(PageArt { cover, corner });
    }
}

/// The lightness of the page artwork's top-left corner, once the artwork has been drawn. Art
/// that has not been sampled yet answers `None`, and the panel keeps the theme's own ink.
pub(crate) fn corner(cx: &App) -> Option<f32> {
    cx.try_global::<PageArt>().and_then(|art| art.corner)
}
