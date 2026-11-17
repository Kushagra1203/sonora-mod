use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{App, EntityId, Window};
use music::{Listening, Spectrum};
use ui::Levels;

/// How long the drawn levels take to close 63% of the gap to the analyzer's. A time constant
/// rather than a share per frame, so the motion is the same on a 60 Hz and a 144 Hz display
/// and a frame that came late moves further instead of falling behind.
const SETTLE: Duration = Duration::from_millis(40);
/// The longest gap between two frames that still counts as one step: past it the levels adopt
/// the target outright, so coming back from a stalled window snaps rather than swoops.
const STALL: Duration = Duration::from_millis(250);
/// How many steps may pass with the view drawing nothing before the drive gives up. The view
/// draws when the levels move, so a quiet stretch is the only thing that tells the drive the
/// view has gone.
const QUIET: u32 = 30;

#[derive(Default)]
struct State {
    shown: Levels,
    /// The analyzer's last reading, kept so a frame reads into it rather than allocating.
    target: Levels,
    spectrum: Spectrum,
    listening: Option<Listening>,
    armed: bool,
    /// How many steps have passed since the view last drew. The drive gives up when the view
    /// has gone quiet for a while, which is how it knows it is no longer on screen.
    quiet: u32,
    visible: bool,
    beat: Option<Instant>,
}

#[derive(Clone, Default)]
pub struct VisualizerDrive {
    state: Rc<RefCell<State>>,
}

impl VisualizerDrive {
    pub fn levels(&self) -> Levels {
        self.state.borrow().shown.clone()
    }

    pub fn show(&self, watch: EntityId, spectrum: Spectrum, window: &mut Window) {
        let mut state = self.state.borrow_mut();
        state.visible = true;
        state.quiet = 0;
        state.listening = Some(spectrum.listen());
        state.spectrum = spectrum;
        if state.armed {
            return;
        }
        state.armed = true;
        state.beat = None;
        drop(state);

        let drive = self.clone();
        window.on_next_frame(move |window, cx| drive.step(watch, window, cx));
    }

    pub fn hide(&self) {
        self.state.borrow_mut().visible = false;
    }

    fn step(&self, watch: EntityId, window: &mut Window, cx: &mut App) {
        let moved = {
            let mut state = self.state.borrow_mut();
            state.quiet = state.quiet.saturating_add(1);
            if !state.visible || state.quiet > QUIET {
                state.armed = false;
                state.listening = None;
                return;
            }
            let spectrum = state.spectrum.clone();
            let now = Instant::now();
            let rate = match state.beat.replace(now) {
                Some(beat) if now.duration_since(beat) < STALL => {
                    1. - (-now.duration_since(beat).as_secs_f32() / SETTLE.as_secs_f32()).exp()
                }
                _ => 1.,
            };
            let State {
                shown, target, ..
            } = &mut *state;
            spectrum.left(&mut target.left);
            spectrum.right(&mut target.right);
            ease(&mut shown.left, &target.left, rate) | ease(&mut shown.right, &target.right, rate)
        };

        // A frame whose levels did not move is a frame that need not be drawn: the bars are
        // where they were, so the page above them has nothing to show either.
        if moved {
            cx.notify(watch);
        }
        let drive = self.clone();
        window.on_next_frame(move |window, cx| drive.step(watch, window, cx));
    }
}

/// Walks `shown` a share of the way towards `target`, adopting it outright when the band
/// count changes. It says whether anything moved.
fn ease(shown: &mut Vec<f32>, target: &[f32], rate: f32) -> bool {
    if shown.len() != target.len() {
        *shown = target.to_vec();
        return true;
    }
    let mut moved = false;
    for (shown, target) in shown.iter_mut().zip(target) {
        let next = *shown + (target - *shown) * rate;
        if next != *shown {
            moved = true;
        }
        *shown = next;
    }
    moved
}
