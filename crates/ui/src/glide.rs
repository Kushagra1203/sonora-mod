use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{
    App, EntityId, ListState, Pixels, Point, ScrollHandle, SpringConfig, SpringState, Window,
    point, px,
};

use crate::snapped;

/// A scroll position that can be animated by the shared glide controller.
pub trait ScrollPosition: Clone + 'static {
    fn offset(&self) -> Point<Pixels>;
    fn set_offset(&self, offset: Point<Pixels>);
    fn max_offset(&self) -> Point<Pixels>;
}

impl ScrollPosition for ScrollHandle {
    fn offset(&self) -> Point<Pixels> {
        ScrollHandle::offset(self)
    }
    fn set_offset(&self, offset: Point<Pixels>) {
        ScrollHandle::set_offset(self, offset);
    }
    fn max_offset(&self) -> Point<Pixels> {
        ScrollHandle::max_offset(self)
    }
}

impl ScrollPosition for ListState {
    fn offset(&self) -> Point<Pixels> {
        self.scroll_px_offset_for_scrollbar()
    }
    fn set_offset(&self, offset: Point<Pixels>) {
        self.set_offset_from_scrollbar(offset);
    }
    fn max_offset(&self) -> Point<Pixels> {
        self.max_offset_for_scrollbar()
    }
}

/// How much of the distance to the target the glide covers each step at 180 Hz. Higher arrives
/// sooner; the exponential tail is what reads as the motion settling rather than stopping.
const EASE: f32 = 0.16;
const HERTZ: f32 = 180.;
const STALL: Duration = Duration::from_millis(64);
const REST: Pixels = px(0.5);
const SPRING_REST: Pixels = px(0.05);
/// How far one wheel notch carries beyond the platform's own delta. A wheel reports a whole
/// three lines per notch, which is not enough to cross a page; three times that is. A trackpad
/// delta is already in true pixels and passes through at one.
pub const WHEEL_GAIN: f32 = 3.0;

/// How long the wheel must be still before the coast takes over. A gesture that pauses for
/// longer than this has ended, and a fast one carries on by itself.
const COAST_PAUSE: Duration = Duration::from_millis(90);
/// The longest gap between two ticks that still counts as one gesture. Past this the wheel
/// was resting, and the tick that follows it starts a fresh gesture with no speed of its own.
const COAST_TICK: Duration = Duration::from_millis(90);
/// The speed below which there is nothing to coast on. A deliberate scroll is slower than this
/// and stops where it landed; a flick is faster and carries on. The same speed is what a
/// trackpad has to reach before its own momentum is added to.
const COAST_MIN: f32 = 900.;
/// The speed the coast is capped at, which bounds how far it can carry.
const COAST_MAX: f32 = 7_000.;
/// How quickly the coast runs down, in eights a second. The speed falls by half every
/// hundred and fifty milliseconds or so, so the page slows rather than stopping dead.
const COAST_FRICTION: f32 = 4.5;
/// The speed at which the coast gives up, well under a pixel a frame.
const COAST_STOP: f32 = 40.;
/// How much of a tick's own speed joins the running one. A single slow frame in a flick must
/// not cut the coast short, and a single fast one must not invent momentum.
const COAST_BLEND: f32 = 0.5;

#[derive(Default)]
struct Drift {
    shown: Point<Pixels>,
    target: Point<Pixels>,
    velocity: Point<f32>,
    /// The speed the last gesture ended at, in pixels a second, kept so the coast can pick
    /// it up when the wheel stops.
    coast: Point<f32>,
    /// When the wheel last moved, which is what says whether the gesture is over.
    last_nudge: Option<Instant>,
    /// Whether the page is coasting rather than easing towards a target.
    coasting: bool,
    /// Whether the platform is moving the page itself, as it does for a trackpad, and this
    /// glide is only watching for the end of that gesture.
    native: bool,
    gliding: bool,
    springing: bool,
    armed: bool,
    eased: Option<f32>,
    beat: Option<Instant>,
}

#[derive(Clone)]
pub struct Glide {
    drift: Rc<RefCell<Drift>>,
    pace: f32,
    spring: Option<SpringConfig>,
    watched: Option<EntityId>,
}

impl Default for Glide {
    fn default() -> Self {
        Self::paced(EASE)
    }
}

impl Glide {
    pub fn paced(pace: f32) -> Self {
        Self {
            drift: Rc::default(),
            pace,
            spring: None,
            watched: None,
        }
    }

    pub fn set_pace(&mut self, pace: f32) {
        self.pace = pace;
    }

    pub fn set_spring(&mut self, spring: SpringConfig) {
        self.spring = Some(spring);
    }

    pub fn watch(&mut self, view: EntityId) {
        self.watched = Some(view);
    }

    pub fn sync(&self, scroll: &impl ScrollPosition) {
        let mut drift = self.drift.borrow_mut();
        if !drift.gliding {
            drift.shown = scroll.offset();
            drift.velocity = Point::default();
            drift.coast = Point::default();
            drift.last_nudge = None;
            drift.native = false;
        }
    }

    /// Adds one wheel tick to the glide. `gain` scales the tick's own delta, which is what
    /// lets a wheel notch carry further than the platform's three lines while a trackpad delta
    /// passes through untouched. The tick the platform already applied is put back to the
    /// last drawn offset, so the glide is the only thing moving the region.
    ///
    /// A precise tick — a trackpad — is different: it is drawn exactly where the platform put
    /// it, in the same frame, and is never eased. What is kept of it is only its speed, which
    /// is what a flick coasts on once the fingers lift. A notched tick is eased into place.
    ///
    /// The tick also says how fast the wheel was turning; a slow gesture says little and
    /// stops where it landed, a flick says more and carries on.
    pub fn nudge(
        &self,
        scroll: &impl ScrollPosition,
        gain: f32,
        precise: bool,
        window: &mut Window,
    ) {
        {
            let mut drift = self.drift.borrow_mut();
            let landed = scroll.offset();
            // gpui has already applied this tick to the region by the time this runs.
            let applied = landed - drift.shown;
            let step = match precise {
                true => applied,
                false => applied * gain,
            };
            let now = Instant::now();
            drift.springing = false;
            drift.coasting = false;
            drift.eased = None;
            drift.velocity = Point::default();
            match precise {
                true => {
                    drift.shown = landed;
                    drift.target = landed;
                    drift.gliding = true;
                    drift.native = true;
                    self.remember(&mut drift, applied, now);
                }
                false => {
                    let from = match drift.native {
                        true => drift.shown,
                        false => drift.target,
                    };
                    // A tick the other way starts from where the page is now: the rest of
                    // the old glide was going somewhere the reader no longer wants to be.
                    let reversed = (from.x - drift.shown.x).as_f32() * step.x.as_f32() < 0.
                        || (from.y - drift.shown.y).as_f32() * step.y.as_f32() < 0.;
                    let from = match reversed {
                        true => drift.shown,
                        false => from,
                    };
                    drift.target = held(from + step, scroll);
                    drift.gliding = true;
                    drift.native = false;
                    self.remember(&mut drift, step, now);
                    scroll.set_offset(drift.shown);
                }
            }
        }
        self.schedule_frame(scroll, window);
    }

    /// Keeps the speed the gesture is running at, so that the page can carry on by itself
    /// once the wheel stops.
    ///
    /// The speed is the distance of the tick over the time since the last one, so a wheel
    /// turned slowly says little and a wheel spun hard says a lot. The running speed is a
    /// blend, so one uneven frame neither invents momentum nor throws it away, and a tick in
    /// the other direction replaces it outright.
    fn remember(&self, drift: &mut Drift, step: Point<Pixels>, now: Instant) {
        let Some(gap) = drift
            .last_nudge
            .replace(now)
            .map(|last| now.duration_since(last))
        else {
            drift.coast = Point::default();
            return;
        };
        // A pause ends the gesture: the tick after it is a fresh push, and a lone notch is
        // not a flick however far it carries.
        if gap > COAST_TICK {
            drift.coast = Point::default();
            return;
        }
        let seconds = gap.max(Duration::from_millis(2)).as_secs_f32();
        let instant = point(step.x.as_f32() / seconds, step.y.as_f32() / seconds);
        let blend = |instant: f32, held: f32| match instant == 0. {
            true => held,
            false if instant.signum() != held.signum() => instant,
            false => held + (instant - held) * COAST_BLEND,
        };
        drift.coast = point(
            blend(instant.x, drift.coast.x).clamp(-COAST_MAX, COAST_MAX),
            blend(instant.y, drift.coast.y).clamp(-COAST_MAX, COAST_MAX),
        );
    }

    pub fn aim(&self, scroll: &impl ScrollPosition, to: Point<Pixels>, window: &mut Window) {
        {
            let mut drift = self.drift.borrow_mut();
            if !drift.gliding {
                drift.shown = scroll.offset();
            }
            drift.target = held(to, scroll);
            drift.gliding = true;
            drift.coasting = false;
            drift.native = false;
            drift.coast = Point::default();
            drift.last_nudge = None;
            let springing = self.spring.is_some();
            if drift.springing != springing {
                drift.velocity = Point::default();
            }
            drift.springing = springing;
            drift.eased = Some(self.pace);
        }
        self.schedule_frame(scroll, window);
    }

    pub fn jump(&self, scroll: &impl ScrollPosition, to: Point<Pixels>) {
        let landed = {
            let mut drift = self.drift.borrow_mut();
            drift.target = held(to, scroll);
            drift.shown = drift.target;
            drift.gliding = false;
            drift.springing = false;
            drift.coasting = false;
            drift.native = false;
            drift.coast = Point::default();
            drift.last_nudge = None;
            drift.velocity = Point::default();
            drift.beat = None;
            drift.eased = None;
            drift.shown
        };
        scroll.set_offset(landed);
    }

    pub fn stop_spring(&self, scroll: &impl ScrollPosition) -> bool {
        let mut drift = self.drift.borrow_mut();
        if !drift.springing {
            return false;
        }
        drift.shown = scroll.offset();
        drift.target = drift.shown;
        drift.velocity = Point::default();
        drift.gliding = false;
        drift.springing = false;
        drift.native = false;
        drift.beat = None;
        drift.eased = None;
        true
    }

    pub fn goal(&self, scroll: &impl ScrollPosition) -> Point<Pixels> {
        let drift = self.drift.borrow();

        match drift.gliding {
            true => drift.target,
            false => scroll.offset(),
        }
    }

    pub fn presentation(&self, scroll: &impl ScrollPosition) -> Point<Pixels> {
        let drift = self.drift.borrow();

        match drift.gliding && drift.springing {
            true => drift.shown - scroll.offset(),
            false => Point::default(),
        }
    }

    fn schedule_frame(&self, scroll: &impl ScrollPosition, window: &mut Window) {
        {
            let mut drift = self.drift.borrow_mut();
            if drift.armed {
                return;
            }
            drift.armed = true;
        }

        let glide = self.clone();
        let scroll = scroll.clone();
        window.on_next_frame(move |window, cx| glide.step(&scroll, window, cx));
    }

    fn step(&self, scroll: &impl ScrollPosition, window: &mut Window, cx: &mut App) {
        let landed = {
            let mut drift = self.drift.borrow_mut();
            drift.armed = false;
            if !drift.gliding {
                return;
            }

            let now = Instant::now();
            let elapsed = drift
                .beat
                .replace(now)
                .map(|beat| now.duration_since(beat).min(STALL))
                .unwrap_or(Duration::from_secs_f32(1. / HERTZ));
            let target = held(drift.target, scroll);
            let springing = drift.springing;

            // A trackpad is moving the page itself. Nothing is drawn here until the gesture
            // stops; then a fast one carries on and a slow one simply stops where it is.
            if drift.native {
                let paused = drift
                    .last_nudge
                    .is_some_and(|last| now.duration_since(last) >= COAST_PAUSE);
                if !paused {
                    drop(drift);
                    self.schedule_frame(scroll, window);
                    return;
                }
                let quick = drift.coast.x.abs() >= COAST_MIN || drift.coast.y.abs() >= COAST_MIN;
                drift.native = false;
                if !quick {
                    drift.gliding = false;
                    drift.coasting = false;
                    drift.coast = Point::default();
                    drift.last_nudge = None;
                    drift.beat = None;
                    return;
                }
                drift.coasting = true;
                drift.target = drift.shown;
                drift.eased = None;
            }

            // The wheel has stopped and the gesture was fast enough to be worth carrying on
            // from: the page coasts on its own until it slows to nothing.
            if !springing
                && !drift.coasting
                && drift
                    .last_nudge
                    .is_some_and(|last| now.duration_since(last) >= COAST_PAUSE)
                && (drift.coast.x.abs() >= COAST_MIN || drift.coast.y.abs() >= COAST_MIN)
            {
                drift.coasting = true;
                drift.target = drift.shown;
                drift.eased = None;
            }
            let settled = if springing {
                let spring = self.spring.expect("a springing glide has a spring");
                let x = spring.step(
                    SpringState {
                        position: drift.shown.x.as_f32(),
                        velocity: drift.velocity.x,
                    },
                    target.x.as_f32(),
                    elapsed.as_secs_f32(),
                );
                let y = spring.step(
                    SpringState {
                        position: drift.shown.y.as_f32(),
                        velocity: drift.velocity.y,
                    },
                    target.y.as_f32(),
                    elapsed.as_secs_f32(),
                );
                drift.shown = point(px(x.position), px(y.position));
                drift.velocity = point(x.velocity, y.velocity);
                spring.is_settled(x, target.x.as_f32(), SPRING_REST.as_f32())
                    && spring.is_settled(y, target.y.as_f32(), SPRING_REST.as_f32())
            } else if drift.coasting {
                let seconds = elapsed.as_secs_f32();
                let next = point(
                    drift.coast.x * (-COAST_FRICTION * seconds).exp(),
                    drift.coast.y * (-COAST_FRICTION * seconds).exp(),
                );
                drift.shown += point(px(next.x * seconds), px(next.y * seconds));
                // the end of the page stops the coast rather than letting it push at it
                let reached = held(drift.shown, scroll);
                if reached != drift.shown {
                    drift.shown = reached;
                    drift.coast = Point::default();
                }
                drift.coast = next;
                drift.target = drift.shown;
                let stopped = next.x.abs() < COAST_STOP && next.y.abs() < COAST_STOP;
                if stopped {
                    drift.coasting = false;
                    drift.coast = Point::default();
                }
                stopped
            } else {
                let pace = drift.eased.unwrap_or(EASE);
                let ease = 1. - (1. - pace).powf(elapsed.as_secs_f32() * HERTZ);
                let step = target - drift.shown;
                drift.shown += point(step.x * ease, step.y * ease);
                step.x.abs() < REST && step.y.abs() < REST
            };
            match settled {
                true => {
                    // The coast has already moved the page to where it stopped, so the
                    // landing is what was last drawn rather than the target as it stood
                    // before this step.
                    drift.shown = drift.target;
                    drift.gliding = false;
                    drift.springing = false;
                    drift.native = false;
                    drift.coasting = false;
                    drift.coast = Point::default();
                    drift.last_nudge = None;
                    drift.velocity = Point::default();
                    drift.beat = None;
                    drift.eased = None;
                    drift.shown
                }
                // layout walks the pixel grid
                false => match springing {
                    true => grid(held(drift.shown, scroll), window),
                    false => held(drift.shown, scroll),
                },
            }
        };

        scroll.set_offset(landed);
        match self.watched {
            Some(view) => cx.notify(view),
            None => window.refresh(),
        }
        self.schedule_frame(scroll, window);
    }
}

fn grid(at: Point<Pixels>, window: &Window) -> Point<Pixels> {
    point(snapped(at.x, window), snapped(at.y, window))
}

fn held(at: Point<Pixels>, scroll: &impl ScrollPosition) -> Point<Pixels> {
    let reach = scroll.max_offset();

    point(
        at.x.clamp(-reach.x.max(Pixels::ZERO), Pixels::ZERO),
        at.y.clamp(-reach.y.max(Pixels::ZERO), Pixels::ZERO),
    )
}
