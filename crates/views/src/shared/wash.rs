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

/// How far past the window's edges the cover is drawn, so the blur's soft border falls outside
/// the frame instead of fading the field out at its own edges.
const BLEED: Pixels = px(100.);
/// How hard the cover is blurred. Wide enough that no shape survives, narrow enough that the
/// renderer still has a few texels of colour to work with.
const BLUR: Pixels = px(84.);
/// How much of the page colour lies over the whole field, so bright art still leaves the hero's
/// text readable.
const SHADE: f32 = 0.30;
/// Where the cover's light starts yielding to the page, and where it has fully become it.
const FADE_FROM: f32 = 0.42;
const FADE_TO: f32 = 0.78;

/// How much of a sidebar's own colour it keeps when it frosts the page wash instead of sitting
/// on it, and how hard it blurs the artwork behind. Kept light: most of the colour comes from
/// the wash itself, and the blur and grain do the diffusing rather than a slab of tint.
pub(crate) const SIDEBAR_FROST_TINT: f32 = 0.18;
pub(crate) const SIDEBAR_FROST_BLUR: Pixels = px(38.);

/// The wash behind a detail page. `cover` is the largest cover the page holds; without one the
/// page keeps the plain theme background.
#[derive(IntoElement)]
pub(crate) struct PageWash {
    cover: Option<String>,
    /// How far below the window's top the wash starts. Zero on a page that drops the title bar,
    /// the bar's height on one that keeps it.
    top: Pixels,
}

impl PageWash {
    pub(crate) fn new(cover: Option<String>) -> Self {
        Self {
            cover,
            top: Pixels::ZERO,
        }
    }

    /// Starts the wash `top` below the window's top edge.
    pub(crate) fn top(mut self, top: Pixels) -> Self {
        self.top = top;
        self
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
        // The cover keeps its own square, so the field shows its top band rather than a zoomed
        // crop of its middle. It is drawn a little past every edge so the blur never pulls in
        // the transparent border, and tall enough to reach below the fold at any aspect.
        let art = (width + BLEED * 2.).max(height + BLEED * 2.);
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

        div()
            .id("page-wash")
            .absolute()
            .top(self.top)
            .left_0()
            .right_0()
            .bottom_0()
            .overflow_hidden()
            .child(
                div()
                    .absolute()
                    .left(relative(0.5))
                    .ml(-(art / 2.))
                    // The cover's top edge starts just past the window, so what shows is the
                    // top of the artwork and the blur's soft start stays out of frame.
                    .top(-BLEED - self.top)
                    .w(art)
                    .h(art)
                    .opacity(clarity)
                    .child(
                        Artwork::new(Some(cover))
                            .size(art)
                            .wide(art)
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
                linear_color_stop(end, FADE_TO),
            )))
            .into_any_element()
    }
}
