use std::time::Duration;

use gpui::{
    Animation, AnimationElement, AnimationExt as _, ElementId, IntoElement, Pixels, Styled, point,
    px,
};

use crate::motion::ease_out_cubic;

/// How far an arriving item sits below its resting spot while it fades in.
const RISE: Pixels = px(10.);
/// The pause between two neighbours in a staggered arrival.
const STAGGER: Duration = Duration::from_millis(30);
/// How many neighbours get their own slot before the cap, so a long list never
/// makes its tail wait half a second.
pub(crate) const SLOTS: usize = 12;
/// How long one item's fade and rise takes.
const FADE: Duration = Duration::from_millis(180);
/// The whole arrival clock: every item maps its own slot onto this one span so
/// they all share a single start instant.
pub(crate) const SPAN: Duration = Duration::from_millis(30 * 12 + 180);

/// A staggered entrance for content that just arrived. Wrap the first screenful
/// of a batch — items past `SLOTS` render at rest immediately, and the wrapper
/// must be dropped once the arrival has run, or scrolling replays it.
pub trait Arriving: Sized {
    fn arriving(self, id: impl Into<ElementId>, index: usize) -> AnimationElement<Self>;
}

/// The arrival constants a caller needs to open and close its window.
pub enum Arrival {}

impl Arrival {
    /// How many items keep their own stagger slot.
    pub const SLOTS: usize = SLOTS;
    /// How long the whole stagger runs, before the wrapper must be dropped.
    pub fn span() -> Duration {
        SPAN
    }
}

impl<E: Styled + IntoElement + 'static> Arriving for E {
    fn arriving(self, id: impl Into<ElementId>, index: usize) -> AnimationElement<Self> {
        let delay = index.min(SLOTS) as f32 * STAGGER.as_secs_f32();
        let span = SPAN.as_secs_f32();
        let fade = FADE.as_secs_f32();

        self.with_animation(id, Animation::new(SPAN), move |element, delta| {
            let t = ((delta * span - delay) / fade).clamp(0., 1.);
            let t = ease_out_cubic(t);
            element
                .opacity(t)
                .layer_translate(point(px(0.), RISE * (1. - t)))
        })
    }
}
