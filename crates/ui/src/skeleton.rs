use std::time::Duration;

use gpui::prelude::*;
use gpui::{
    Animation, AnimationExt as _, App, Div, Interactivity, Pixels, SharedString, StyleRefinement,
    Window, div, ease_in_out, hsla, linear_color_stop, linear_gradient, relative,
};

use crate::motion::animates;
use crate::theme::ActiveTheme as _;

const PULSE: Duration = Duration::from_millis(1400);
/// How often the pulse redraws, in frames a second. It is a slow fade over a flat block, and a
/// page shows a dozen of them at once, so it stays well under the display's rate.
const PULSE_FPS: f32 = 30.;
/// One glide of the shimmer's light band across the block.
const SHIMMER: Duration = Duration::from_millis(1600);
/// The band's width as a fraction of the block.
const SHIMMER_BAND: f32 = 0.6;

#[derive(IntoElement)]
pub struct Skeleton {
    base: Div,
    circle: bool,
}

impl Default for Skeleton {
    fn default() -> Self {
        Self::new()
    }
}

impl Skeleton {
    #[track_caller]
    pub fn new() -> Self {
        Self {
            base: div(),
            circle: false,
        }
    }

    pub fn circle(mut self) -> Self {
        self.circle = true;
        self
    }
}

impl Styled for Skeleton {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Skeleton {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl RenderOnce for Skeleton {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { mut base, circle } = self;
        let theme = cx.theme();
        let overrides = std::mem::take(base.style());

        let mut skeleton = base
            .bg(theme.muted)
            .relative()
            .overflow_hidden()
            .when_else(
                circle,
                |this| this.rounded_full(),
                |this| this.rounded(theme.radius),
            )
            .when(animates(cx), |this| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .w(relative(SHIMMER_BAND))
                        .rounded_full()
                        .bg(linear_gradient(
                            90.,
                            linear_color_stop(hsla(0., 0., 1., 0.), 0.),
                            linear_color_stop(hsla(0., 0., 1., 0.09), 1.),
                        ))
                        .with_animation(
                            "skeleton-shimmer",
                            Animation::new(SHIMMER).repeat().with_max_fps(PULSE_FPS),
                            |band, delta| {
                                band.left(relative(
                                    SHIMMER_BAND * -1. + delta * (1. + SHIMMER_BAND),
                                ))
                            },
                        ),
                )
            });
        skeleton.style().refine(&overrides);

        skeleton.with_animation(
            "skeleton",
            Animation::new(PULSE)
                .repeat()
                .with_easing(ease_in_out)
                .with_max_fps(PULSE_FPS),
            |this, delta| {
                let fade = 1. - (delta * std::f32::consts::TAU).cos().abs() * 0.5;
                this.opacity(0.4 + fade * 0.3)
            },
        )
    }
}

#[derive(IntoElement)]
pub struct Initials {
    base: Div,
    name: SharedString,
    size: Pixels,
}

impl Initials {
    #[track_caller]
    pub fn new(name: impl Into<SharedString>, size: Pixels) -> Self {
        Self {
            base: div(),
            name: name.into(),
            size,
        }
    }
}

impl Styled for Initials {
    fn style(&mut self) -> &mut StyleRefinement {
        self.base.style()
    }
}

impl InteractiveElement for Initials {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.base.interactivity()
    }
}

impl RenderOnce for Initials {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            mut base,
            name,
            size,
        } = self;
        let letters = name
            .split_whitespace()
            .filter_map(|word| word.chars().next())
            .take(2)
            .collect::<String>()
            .to_uppercase();

        let theme = cx.theme();
        let overrides = std::mem::take(base.style());

        let mut initials = base
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(size)
            .rounded_full()
            .bg(theme.secondary)
            .text_size(size * 0.34)
            .text_color(theme.muted_foreground)
            .child(letters);
        initials.style().refine(&overrides);
        initials
    }
}
