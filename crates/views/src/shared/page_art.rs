//! The page's own artwork, published for the panels beside it.
//!
//! An album, playlist or artist page lets its artwork's field sit behind the sidebars, so the
//! panel over it is as light or as dark as the picture is. The sidebar's own ink cannot be
//! chosen from the theme's polarity for that reason: a dark theme over a bright banner needs
//! dark text. This carries the cover rather than a colour, so a panel reads the palette at its
//! own draw — the same frame the wash loads the artwork in — instead of a frame behind it.

use gpui::{App, Global, SharedString};

#[derive(Default)]
struct PageArt {
    cover: Option<SharedString>,
}

impl Global for PageArt {}

/// Publishes the artwork the page is showing, rewritten only when the page moves. The root
/// calls this before the wash draws, so a panel later in the frame finds the cover here.
pub(crate) fn publish(cover: Option<SharedString>, cx: &mut App) {
    let stale = cx
        .try_global::<PageArt>()
        .is_none_or(|art| art.cover != cover);

    if stale {
        cx.set_global(PageArt { cover });
    }
}

/// The artwork the page is showing, if it has one. A caller resolves its palette itself, which
/// is what keeps the panels in step with the frame the wash paints the artwork on.
pub(crate) fn cover(cx: &App) -> Option<SharedString> {
    cx.try_global::<PageArt>().and_then(|art| art.cover.clone())
}
