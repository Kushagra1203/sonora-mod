//! The colour field under an album or playlist: the cover, enlarged far past its own size and
//! blurred until only its light survives, then darkened and faded into the page so the hero and
//! the rows sit on one continuous surface. It is painted behind the scroller rather than moving
//! with it, which is what makes the page feel like a place rather than a stack of rows.
//!
//! The field is a static gradient, not a second backdrop blur: it has no rim, no clipping and no
//! interaction with the glass above it, so it cannot reproduce the painted-border problem that
//! another live blur would.

use gpui::prelude::*;
use gpui::{
    App, Pixels, RenderOnce, Window, div, linear_color_stop, linear_gradient, px, relative,
};
use ui::{ActiveTheme as _, Artwork};

/// How much wider and taller than the page the cover is drawn, so its blurred edge never enters
/// the frame.
const OVERSIZE: f32 = 1.5;
/// How hard the cover is blurred. Wide enough that no shape survives, narrow enough that the
/// renderer still has a few texels of colour to work with.
const BLUR: Pixels = px(84.);
/// How much of the page colour lies over the whole field, so bright art still leaves the hero's
/// text readable.
const SHADE: f32 = 0.30;
/// Where the field has fully become the page when the window is opaque.
const FADE_FROM: f32 = 0.55;
/// How much of the page the cover occupies at its largest (before the fade starts).
const COVER_REACH: f32 = 0.60;

/// The wash behind a detail page. `cover` is the largest cover the page holds; without one the
/// page keeps the plain theme background.
#[derive(IntoElement)]
pub(crate) struct PageWash {
    cover: Option<String>,
}

impl PageWash {
    pub(crate) fn new(cover: Option<String>) -> Self {
        Self { cover }
    }
}

impl RenderOnce for PageWash {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *cx.theme();
        let Some(cover) = self.cover else {
            return div()
                .id("page-wash")
                .absolute()
                .inset_0()
                .into_any_element();
        };

        let viewport = window.viewport_size();
        let height = viewport.height;
        let width = viewport.width;
        let art = width * OVERSIZE;
        // The cover is drawn taller than the page so it reaches ~COVER_REACH of the viewport
        // before the fade starts. We oversize horizontally too so the blurred edges never show.
        let tall = height * COVER_REACH * OVERSIZE;
        // A see-through window keeps its own opacity: the field thins with the page instead of
        // sitting on the wallpaper as an opaque sheet.
        let clarity = match theme.transparent {
            true => theme.background.a,
            false => 1.,
        };
        // On an opaque window the field ends in the page colour. On a see-through one it ends
        // in nothing, so the wallpaper still reads through the bottom of the page.
        let end = match theme.transparent {
            true => theme.background.opacity(0.),
            false => theme.background,
        };

        // The wash sits below the title bar. The title bar height is ~theme.metrics.title_bar.
        // We offset the wash down by that amount so it starts right under the title bar.
        let title_bar_h = theme.metrics.title_bar;

        div()
            .id("page-wash")
            .absolute()
            .top(title_bar_h)
            .left_0()
            .right_0()
            .bottom_0()
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left(relative(0.5))
                    .ml(-(art / 2.))
                    // Position the cover so its bottom sits at ~COVER_REACH of the viewport.
                    .top(-(tall - height * COVER_REACH))
                    .w(art)
                    .h(tall)
                    .opacity(clarity)
                    // Fade out from the cover's bottom downward.
                    .fade_edges(Pixels::ZERO, height * (1. - COVER_REACH + 0.08))
                    .child(
                        Artwork::new(Some(cover))
                            .size(art)
                            .wide(tall)
                            .overlay()
                            .soft(true)
                            .blur(BLUR),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .bg(theme.background.opacity(SHADE * clarity)),
            )
            .child(div().absolute().inset_0().bg(linear_gradient(
                180.,
                linear_color_stop(theme.background.opacity(0.), FADE_FROM),
                linear_color_stop(end, 1.),
            )))
            .into_any_element()
    }
}
