use gpui::prelude::*;
use gpui::{App, Div, Hsla, Pixels, div, px};

use crate::theme::ActiveTheme as _;

/// How much of the popover colour a glass control keeps over what it blurs.
pub const GLASS_FILL: f32 = 0.2;
/// How hard a glass control blurs what is behind it.
pub const GLASS_BLUR: Pixels = px(8.);
/// Raised surfaces retain contrast even over bright artwork, while a faint backdrop remains visible.
const POPOVER_FILL: f32 = 0.82;
/// How much of the popover colour a frosted bar keeps, and how far it blurs what passes under.
/// Both are thicker than a menu's, since a bar sits over the page for good and its text has to
/// read over whatever scrolls by.
const BAR_FILL: f32 = 0.86;
const BAR_BLUR: Pixels = px(18.);
/// The bar's fill on a see-through window. The page wash already sits under the bar, so the thick
/// fill would stack on it and leave the bar far darker than the window around it.
const CLEAR_BAR_FILL: f32 = 0.3;

/// Whether a surface that only frosts for the look of it should frost at all. A backdrop blur
/// is the priciest thing the renderer does per frame, so the setting turns those off and each
/// one falls back to a flat fill. The settings header and the fullscreen chrome frost whatever
/// it answers: their haze is what keeps text readable over the content passing under it.
pub fn blurring(cx: &App) -> bool {
    cx.theme().blur
}

/// Styles an element as frosted glass: a faint popover fill over a hard blur of
/// whatever it floats on. Over a flat colour the blur shows nothing, so the fill is all
/// that separates the control from the page there. A site the blur setting reaches asks
/// `blurring` before calling this, since the thin fill needs the wash to read.
pub fn glass<E: Styled>(element: E, cx: &App) -> E {
    let theme = cx.theme();

    element
        .bg(theme.popover.opacity(GLASS_FILL))
        .backdrop_blur(GLASS_BLUR)
}

/// The fill shared by context menus and raised translucent panels. With blur disabled it
/// becomes opaque, since there is no wash behind the text to preserve its contrast.
pub fn popover_fill(cx: &App) -> Hsla {
    let theme = cx.theme();
    match blurring(cx) {
        true => theme.popover.opacity(POPOVER_FILL),
        false => theme.popover,
    }
}

/// Gives a raised panel the same fill, foreground and backdrop blur as a context menu.
pub fn popover_surface<E: Styled>(element: E, cx: &App) -> E {
    let element = element
        .bg(popover_fill(cx))
        .text_color(cx.theme().popover_foreground);
    match blurring(cx) {
        true => frost(element),
        false => element,
    }
}

/// Styles chrome that stays over scrolling content, the player bar and a table's head: a thick
/// popover fill over a wide blur. The caller asks `blurring` first, since without the blur the
/// thin fill would leave the text on the bare page. A see-through window gets a thinner fill so
/// the bar lets the desktop through about as much as the page does.
pub fn bar_surface<E: Styled>(element: E, cx: &App) -> E {
    let theme = cx.theme();
    let fill = match theme.transparent {
        true => CLEAR_BAR_FILL,
        false => BAR_FILL,
    };
    element
        .bg(theme.popover.opacity(fill))
        .backdrop_blur(BAR_BLUR)
}

/// Blurs what is behind an element by the glass width and nothing more, for a hover fill
/// that is already translucent. Only hovers that float over real content go through it;
/// on flat paint the blur shows nothing, so those stay plain fills. The caller asks
/// `blurring` first, since this one has no theme to read.
pub fn frost<E: Styled>(element: E) -> E {
    element.backdrop_blur(GLASS_BLUR)
}

/// How much of the popover colour a liquid-glass surface keeps over what it blurs. Thin
/// enough that the page reads through, thick enough that the surface has a body of its own.
pub const LIQUID_FILL: f32 = 0.12;
/// How hard a liquid-glass surface blurs what is behind it. A narrow blur is deliberate: a
/// gaussian this wide runs at full resolution, while anything past four pixels is halved and
/// halved again until the backdrop is a smear of a few dozen texels. The refraction in the
/// backdrop shader does the work a wider blur would otherwise be asked for.
pub const LIQUID_BLUR: Pixels = px(4.);
/// How hard the backdrop bends at the capsule's rim, from 0 for a plain blur to 1.
pub const LIQUID_BEND: f32 = 1.;
/// The radius a liquid capsule turns its corners with. Big and continuous, so the shape reads
/// as a lens rather than a rounded box.
pub const LIQUID_RADIUS: Pixels = px(30.);
/// The alpha of the lit rim along the glass's top and left edges.
const RIM_LIT: f32 = 0.18;
/// The alpha of the shaded rim along its bottom and right edges.
const RIM_SHADED: f32 = 0.32;

/// A capsule of liquid glass: the thinnest fill over a narrow blur whose edges bend the page
/// like a lens, a bright rim where light lands on the top and left edges, and a shaded rim on
/// the bottom and right. There is deliberately no painted sheen: a fixed highlight that never
/// changes with the page is the one part of the material that reads as a sticker. Content is
/// added as ordinary children of the returned div and paints over the material. With blur
/// turned off the fill becomes opaque, since a thin fill over flat paint leaves text
/// unreadable.
pub fn liquid_capsule(cx: &App) -> Div {
    let theme = cx.theme();
    let blurred = blurring(cx);
    let fill = match blurred {
        true => theme.popover.opacity(LIQUID_FILL),
        false => theme.popover,
    };
    let lit = theme.foreground.opacity(RIM_LIT);
    let shaded = theme.background.opacity(RIM_SHADED);

    div()
        .relative()
        .rounded(LIQUID_RADIUS)
        .squircle()
        .overflow_hidden()
        .bg(fill)
        .when(blurred, |this| {
            this.backdrop_blur(LIQUID_BLUR)
                .backdrop_liquid(LIQUID_BEND)
        })
        .shadow_lg()
        // light lands on the top and left edges, as on the near side of a thick lens
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(LIQUID_RADIUS)
                .squircle()
                .border_t_1()
                .border_l_1()
                .border_color(lit),
        )
        // the far side sits in shade, which grounds the glass against what it floats on
        .child(
            div()
                .absolute()
                .inset_0()
                .rounded(LIQUID_RADIUS)
                .squircle()
                .border_b_1()
                .border_r_1()
                .border_color(shaded),
        )
}
