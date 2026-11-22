use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{App, Div, Pixels, SharedString, Styled, Window, div};

use crate::artwork::Artwork;
use crate::motion::ease_in_out_cubic;

/// How long one cover takes to dissolve into the next.
const SPAN: Duration = Duration::from_millis(1200);

/// Keeps the previous cover beside the current one so a change of artwork
/// dissolves over [`SPAN`] instead of popping. The view owns one per artwork
/// slot, feeds [`CoverFade::element`] the current url on every render, and wraps
/// the result in whatever link or menu carries the slot's identity. The old
/// cover sits above and fades out, so the new one shows through as it loads.
#[derive(Default)]
pub struct CoverFade {
    shown: Option<SharedString>,
    previous: Option<(SharedString, Instant)>,
}

impl CoverFade {
    #[track_caller]
    pub fn new() -> Self {
        Self::default()
    }

    /// The layered artwork for `url`, dissolving from the last url this slot showed.
    pub fn element(
        &mut self,
        url: Option<impl Into<SharedString>>,
        size: Pixels,
        window: &mut Window,
        cx: &App,
    ) -> Div {
        let url = url.map(Into::into);
        if url != self.shown {
            if let Some(previous) = self.shown.take() {
                self.previous = Some((previous, Instant::now()));
            }
            self.shown = url.clone();
        }
        if cx.reduce_motion()
            || self
                .previous
                .as_ref()
                .is_some_and(|(_, since)| since.elapsed() >= SPAN)
        {
            self.previous = None;
        }
        if self.previous.is_some() {
            window.request_animation_frame();
        }

        let mut layers = div().size(size).relative();
        layers = layers.child(Artwork::new(url).size(size));
        if let Some((previous, since)) = &self.previous {
            let t = (since.elapsed().as_secs_f32() / SPAN.as_secs_f32()).clamp(0., 1.);
            layers = layers.child(
                Artwork::new(Some(previous.clone()))
                    .size(size)
                    .opacity(1. - ease_in_out_cubic(t)),
            );
        }
        layers
    }
}
