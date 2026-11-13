//! The home page's field: the poster of the song that is playing, blown up and blurred until
//! only its colour survives, drifting slowly off centre behind the page.
//!
//! It is drawn at a fraction of its own size and scaled back up, the way the ambient field
//! is, so the blur and the poster cost a fraction of the pixels and the cover is decoded
//! small: nothing in the field is sharper than the blur, so the upscale shows nothing.
//!
//! The field follows the song the player shows, so the page behind the shelves carries the
//! colour of what is playing without ever showing the picture itself. Its brightness is
//! settled onto the page's own, so a white sleeve does not carry the page away from the
//! colour its text is drawn in.

use std::f32::consts::TAU;
use std::time::Instant;

use gpui::prelude::*;
use gpui::{Context, Entity, Hsla, Pixels, Render, Window, div, point, px};
use state::{Playback, Sonora};
use ui::{ActiveTheme as _, Artwork};

/// How many times smaller the field is drawn than it is shown. The layer is laid out at this
/// fraction of the poster and the compositor scales it back up with a bilinear sample, so
/// the blur and the cover cost a fraction of the pixels they would at the window's own size.
const DOWNSCALE: f32 = 6.;
/// The blur over the field, at its own scale. It is scaled back up with the field, so the
/// poster is blurred by six times this on screen: enough that no shape of it survives.
const BLUR: Pixels = px(6.);
/// How far the field's brightness stands from the page's own: a little above it on a dark
/// page, a little below it on a light one. The poster is dimmed onto it, so a bright sleeve
/// shows its shapes without turning the page into a sheet of white.
const LIFT: f32 = 0.12;
/// Where the poster's centre stands, as a share of the window: off centre, so the page has
/// a side to breathe on.
const CENTRE_X: f32 = 0.64;
const CENTRE_Y: f32 = 0.36;
/// How far the poster wanders from there, as a share of the window, and how long a wander
/// takes. It is slow enough to be felt rather than watched.
const WANDER_X: f32 = 0.05;
const WANDER_Y: f32 = 0.06;
const PERIOD: f32 = 54.;
/// How much bigger than the window the poster is drawn, so it still covers the corners while
/// it wanders off centre.
const COVER: f32 = 1.4;

/// The field behind the home page: the poster of the song that is playing, blurred and
/// drifting. `Root` owns the one entity and paints it under the page.
pub(crate) struct Haze {
    playback: Entity<Playback>,
    started: Instant,
}

impl Haze {
    pub(crate) fn new(playback: Entity<Playback>, cx: &mut Context<Self>) -> Self {
        cx.observe(&playback, |_, _, cx| cx.notify()).detach();

        Self {
            playback,
            started: Instant::now(),
        }
    }
}

impl Render for Haze {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        // Without motion the field holds still: no frame is asked for, so the page settles.
        let animates =
            ui::motion::animates(cx) && Sonora::global(cx).settings.read(cx).ambient_motion();
        let elapsed = match animates {
            true => {
                window.request_animation_frame();
                self.started.elapsed().as_secs_f32()
            }
            false => 0.,
        };
        let cover = self
            .playback
            .read(cx)
            .shown_track(cx)
            .and_then(|track| track.cover);
        // The palette is sampled off the cover as it is decoded, so the field knows how bright
        // the poster is and can settle its own brightness onto the page's.
        let poster = cover
            .as_ref()
            .and_then(|cover| ui::cover_palette(cover, cx))
            .map(|palette| palette.lightness);
        let shade = match poster {
            Some(poster) => shade(poster, theme.background.l),
            // Without a palette the poster is not on screen yet, so the field is the page's
            // own colour until it lands.
            None => 1.,
        };

        let viewport = window.viewport_size();
        let wide = viewport.width.as_f32();
        let high = viewport.height.as_f32();
        // The poster is square and has to cover the window even while it wanders off centre.
        let side = wide.max(high) * COVER;
        let spin = TAU * elapsed / PERIOD;
        let centre_x = (CENTRE_X + WANDER_X * spin.sin()) * wide;
        let centre_y = (CENTRE_Y + WANDER_Y * (spin * 0.83).cos()) * high;
        // The layer is laid out at a fraction of the poster and scaled back up from its own
        // top left corner, so what it paints lands where the poster stands.
        let drawn = side / DOWNSCALE;
        let left = centre_x - side / 2.;
        let top = centre_y - side / 2.;
        // A see-through window keeps its own opacity: the field thins with the page instead of
        // sitting on the wallpaper as an opaque sheet.
        let clarity = match theme.transparent {
            true => theme.background.a,
            false => 1.,
        };
        let page = Hsla {
            a: 1.,
            ..theme.background
        };

        div()
            .id("home-field")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .opacity(clarity)
            .child(div().absolute().inset_0().bg(page))
            .when_some(cover, |this, cover| {
                this.child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(drawn))
                        .h(px(drawn))
                        .layer_scale(DOWNSCALE)
                        .layer_scale_origin(point(0., 0.))
                        .blur(BLUR)
                        .child(
                            Artwork::new(Some(cover))
                                .size(px(drawn))
                                .corner_radius(px(0.))
                                .overlay(),
                        ),
                )
            })
            .child(div().absolute().inset_0().bg(page.opacity(shade)))
            // Every buffer the field passes through holds eight bits a channel, and a gradient
            // this wide and this dark steps through only a few dozen of them, so its steps read
            // as bands that slide with the poster. The dither scatters each step over
            // neighbouring pixels instead.
            .child(ui::grain(window))
            .into_any_element()
    }
}

/// How much of the page colour lies over a poster of `poster` lightness, so the field lands
/// on the page's own brightness: `LIFT` above it on a dark page, `LIFT` below it on a light
/// one. A poster already on the right side of the target is left as it is.
pub(crate) fn shade(poster: f32, background: f32) -> f32 {
    let target = match background > 0.5 {
        true => (background - LIFT).max(0.),
        false => (background + LIFT).min(1.),
    };
    let gap = background - poster;
    if gap.abs() < f32::EPSILON {
        return 0.;
    }

    ((target - poster) / gap).clamp(0., 1.)
}
