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
    AnyElement, App, Pixels, RenderOnce, Window, div, linear_color_stop, linear_gradient, px,
    relative,
};
use ui::{ActiveTheme as _, Artwork};

/// How far past the window's edges the cover is drawn, so the blur's soft border falls outside
/// the frame instead of fading the field out at its own edges.
const BLEED: Pixels = px(100.);
/// How hard the cover is blurred. Wide enough that no shape survives, narrow enough that the
/// renderer still has a few texels of colour to work with.
pub(crate) const BLUR: Pixels = px(84.);
/// How much of the page colour lies over the whole field, so bright art still leaves the hero's
/// text readable. Shared with the chrome that sits over the field and needs to know how bright
/// the result is.
pub(crate) const SHADE: f32 = 0.30;
/// Where the cover's light starts yielding to the page, and where it has fully become it.
const FADE_FROM: f32 = 0.42;
const FADE_TO: f32 = 0.78;
/// How much page colour the leading-edge ink wash lays over the field, where the hero's
/// title sits.
const INK: f32 = 0.26;

/// How much of a sidebar's own colour it keeps when it frosts the page wash instead of sitting
/// on it, and how hard it blurs the artwork behind. Kept light: most of the colour comes from
/// the wash itself, and the blur and grain do the diffusing rather than a slab of tint.
pub(crate) const SIDEBAR_FROST_TINT: f32 = 0.18;
pub(crate) const SIDEBAR_FROST_BLUR: Pixels = px(38.);

/// The lighter shade an artist page's wash keeps: its artwork reaches the fold, so the wash
/// stays close to the artwork's own colour instead of settling into the page behind it.
pub(crate) const ARTIST_SHADE: f32 = 0.12;

/// The wash behind a detail page. `cover` is the largest cover the page holds; without one the
/// page keeps the plain theme background.
#[derive(IntoElement)]
pub(crate) struct PageWash {
    /// Where the field's artwork comes from.
    source: Option<Source>,
    /// How far below the window's top the wash starts. Zero on a page that drops the title bar,
    /// the bar's height on one that keeps it.
    top: Pixels,
    /// Whether the field settles into the page colour toward the bottom. A page whose artwork
    /// already reaches the fold wants the field left alone, so the sidebars frost the artwork's
    /// own colours rather than a darkening gradient.
    settle: bool,
    /// How much of the page colour lies over the whole field.
    shade: f32,
    /// How much page colour the leading-edge ink wash lays over the field. None lays no wash
    /// over it.
    ink: f32,
}

/// Where a wash's artwork comes from.
enum Source {
    /// A cover the page owns, drawn at the field's own size.
    Cover(String),
    /// Artwork the caller has already built at the field's own size, for a field that dissolves
    /// between covers.
    Art(AnyElement),
}

impl PageWash {
    pub(crate) fn new(cover: Option<String>) -> Self {
        Self {
            source: cover.map(Source::Cover),
            top: Pixels::ZERO,
            settle: true,
            shade: SHADE,
            ink: INK,
        }
    }

    /// The wash for artwork the caller has already built, for a field that dissolves between
    /// covers rather than swapping at once. The artwork is drawn at [`PageWash::field_size`].
    pub(crate) fn of(art: Option<AnyElement>) -> Self {
        Self {
            source: art.map(Source::Art),
            top: Pixels::ZERO,
            settle: true,
            shade: SHADE,
            ink: INK,
        }
    }

    /// The size the field's artwork is drawn at, for a caller that builds the artwork itself.
    pub(crate) fn field_size(window: &Window) -> Pixels {
        let viewport = window.viewport_size();
        (viewport.width + BLEED * 2.).max(viewport.height + BLEED * 2.)
    }

    /// Starts the wash `top` below the window's top edge.
    pub(crate) fn top(mut self, top: Pixels) -> Self {
        self.top = top;
        self
    }

    /// Whether the field darkens into the page toward the bottom. Off on a page whose artwork
    /// reaches the fold, so the sidebars frost the artwork alone.
    pub(crate) fn settle(mut self, settle: bool) -> Self {
        self.settle = settle;
        self
    }

    /// How much of the page colour lies over the whole field.
    pub(crate) fn shade(mut self, shade: f32) -> Self {
        self.shade = shade;
        self
    }

    /// How much page colour the leading-edge ink wash lays over the field. None lays no wash
    /// over it, for a page with no title of its own at the leading edge.
    pub(crate) fn ink(mut self, ink: f32) -> Self {
        self.ink = ink;
        self
    }
}

impl RenderOnce for PageWash {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = *cx.theme();
        let Some(source) = self.source else {
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
                    .child(match source {
                        Source::Cover(cover) => Artwork::new(Some(cover))
                            .size(art)
                            .wide(art)
                            .overlay()
                            .soft(true)
                            .blur(BLUR)
                            .into_any_element(),
                        Source::Art(art) => art,
                    }),
            )
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .bg(theme.background.opacity(self.shade * clarity)),
            )
            // An ink wash from the leading edge, where the hero's title and its actions sit,
            // so the words keep their edges over bright art the way the vertical settle keeps
            // them over the lower rows.
            .when(self.ink > 0., |this| {
                this.child(div().absolute().inset_0().bg(linear_gradient(
                    90.,
                    linear_color_stop(theme.background.opacity(self.ink * clarity), 0.),
                    linear_color_stop(theme.background.opacity(0.), 0.5),
                )))
            })
            .when(self.settle, |this| {
                this.child(div().absolute().inset_0().bg(linear_gradient(
                    180.,
                    linear_color_stop(theme.background.opacity(0.), FADE_FROM),
                    linear_color_stop(end, FADE_TO),
                )))
            })
            .child(ui::grain(window))
            .into_any_element()
    }
}
