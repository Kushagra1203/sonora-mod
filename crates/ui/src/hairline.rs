use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, App, Div, Interactivity, Pixels, StyleRefinement, Styled, Window,
    div, px, relative,
};

use crate::motion::{animates, ease_out_cubic};
use crate::theme::ActiveTheme as _;

/// The hairline's height. A fixed hairline, not a scaled one.
const TRACK: Pixels = px(2.);
/// Loads shorter than this never show the hairline at all, so quick refreshes
/// never flicker.
const DELAY: Duration = Duration::from_millis(150);
/// The hairline's own fade in.
const FADE: Duration = Duration::from_millis(180);
/// One glide of the segment across the track.
const SWEEP: Duration = Duration::from_millis(1100);
/// The segment's width as a fraction of the track.
const SEGMENT: f32 = 0.4;

/// An indeterminate loading hairline: a two-pixel accent segment gliding along
/// the top of a live page. It stays invisible for [`DELAY`] so fast loads never
/// show it, then fades in over the page without shifting layout. Mount it as an
/// absolutely positioned first child of a relative container.
#[derive(IntoElement)]
pub struct Hairline {
    base: Div,
}

impl Default for Hairline {
    fn default() -> Self {
        Self::new()
    }
}

impl Hairline {
    #[track_caller]
    pub fn new() -> Self {
        Self { base: div() }
    }
}

impl Styled for Hairline {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Hairline {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl RenderOnce for Hairline {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let mut base = self.base;
        let overrides = std::mem::take(base.style());
        let color = cx.theme().selection;

        base = base
            .relative()
            .h(TRACK)
            .w_full()
            .overflow_hidden()
            .rounded_full();
        base.style().refine(&overrides);

        match animates(cx) {
            true => base
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(relative(SEGMENT))
                        .rounded_full()
                        .bg(color.opacity(0.85))
                        .with_animation(
                            "hairline-sweep",
                            Animation::new(SWEEP).repeat().with_max_fps(30.),
                            |segment, delta| {
                                segment.left(relative(SEGMENT * -1. + delta * (1. + SEGMENT)))
                            },
                        ),
                )
                .with_animation(
                    "hairline-show",
                    Animation::new(DELAY + FADE),
                    |hairline, delta| {
                        let span = (DELAY + FADE).as_secs_f32();
                        let t = ((delta * span - DELAY.as_secs_f32()) / FADE.as_secs_f32())
                            .clamp(0., 1.);
                        hairline.opacity(ease_out_cubic(t))
                    },
                )
                .into_any_element(),
            false => base
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .h_full()
                        .w(relative(SEGMENT))
                        .left(relative(0.3))
                        .rounded_full()
                        .bg(color.opacity(0.5)),
                )
                .into_any_element(),
        }
    }
}
