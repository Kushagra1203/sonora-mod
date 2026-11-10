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
/// How far the page may stand from where the glide last drew it before that move is taken to be
/// someone else's — a new page, a jump, a catalogue that got shorter — rather than the glide's
/// own. Wider than a frame's rounding, far narrower than anything a reader could mean to do.
const ADOPT: Pixels = px(1.5);

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

/// What is moving the page.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Move {
    /// Nothing: the page sits where it is.
    #[default]
    Still,
    /// The platform is drawing the ticks itself, as it does for a trackpad, and the glide is
    /// only watching for the flick the gesture ends on.
    Native,
    /// A wheel tick being eased into place.
    Ease,
    /// A flick carried on until it runs down or reaches the page's own end.
    Coast,
    /// A spring, as a lyric line follows the voice.
    Spring,
}

#[derive(Default)]
struct Drift {
    /// Where the page is drawn. Always held inside the page's own ends.
    shown: Point<Pixels>,
    /// Where an ease is heading. Always held inside the page's own ends.
    target: Point<Pixels>,
    velocity: Point<f32>,
    /// The speed the last gesture left, in pixels a second, for the coast to pick up.
    coast: Point<f32>,
    /// When the wheel last moved, which is what says whether the gesture is over.
    last_tick: Option<Instant>,
    moving: Move,
    armed: bool,
    eased: Option<f32>,
    beat: Option<Instant>,
}

impl Drift {
    /// Puts the glide down where the page is, with nothing left to carry it there.
    fn stop(&mut self, at: Point<Pixels>) {
        self.shown = at;
        self.target = at;
        self.velocity = Point::default();
        self.coast = Point::default();
        self.last_tick = None;
        self.moving = Move::Still;
        self.beat = None;
        self.eased = None;
    }

    /// Keeps the speed the gesture is running at, so that the page can carry on by itself once
    /// the wheel stops.
    ///
    /// The speed is the distance the page actually moved over the time since the last tick, so
    /// a wheel turned slowly says little and a wheel spun hard says a lot — and a tick against
    /// one of the page's ends, which moves nothing, says nothing at all. The running speed is a
    /// blend, so one uneven frame neither invents momentum nor throws it away, and a tick in
    /// the other direction replaces it outright.
    fn tick(&mut self, step: Point<Pixels>, now: Instant) {
        let Some(gap) = self
            .last_tick
            .replace(now)
            .map(|last| now.duration_since(last))
        else {
            self.coast = Point::default();
            return;
        };
        // A pause ends the gesture: the tick after it is a fresh push, and a lone notch is
        // not a flick however far it carries.
        if gap > COAST_TICK || step == Point::default() {
            self.coast = Point::default();
            return;
        }
        let seconds = gap.max(Duration::from_millis(2)).as_secs_f32();
        let instant = point(step.x.as_f32() / seconds, step.y.as_f32() / seconds);
        let blend = |instant: f32, held: f32| match instant == 0. {
            true => held,
            false if instant.signum() != held.signum() => instant,
            false => held + (instant - held) * COAST_BLEND,
        };
        self.coast = point(
            blend(instant.x, self.coast.x).clamp(-COAST_MAX, COAST_MAX),
            blend(instant.y, self.coast.y).clamp(-COAST_MAX, COAST_MAX),
        );
    }
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

    /// Brings the glide onto the page's own position. The region calls this as it draws, so a
    /// move the glide did not make — a new page, a jump, a catalog that got shorter — is
    /// followed rather than fought: the glide takes the page where it is and stops there.
    pub fn sync(&self, scroll: &impl ScrollPosition) {
        let mut drift = self.drift.borrow_mut();
        let landed = held(scroll.offset(), scroll);
        match drift.moving {
            // The platform is moving the page, or nothing is: the page is simply where it is.
            Move::Native | Move::Still => {
                drift.shown = landed;
                drift.target = landed;
            }
            _ => {
                let stray = (landed - drift.shown)
                    .x
                    .as_f32()
                    .abs()
                    .max((landed - drift.shown).y.as_f32().abs());
                if stray > ADOPT.as_f32() {
                    drift.stop(landed);
                }
            }
        }
    }

    /// Adds one wheel tick to the glide. `gain` scales the tick's own delta, which is what
    /// lets a wheel notch carry further than the platform's three lines while a trackpad delta
    /// passes through untouched.
    ///
    /// A precise tick — a trackpad — is different: the platform has already drawn it, exactly,
    /// and it is never eased. What is kept of it is only its speed, which is what a flick
    /// coasts on once the fingers lift. A notched tick is eased into place: the platform's own
    /// tick is put back to what was drawn, and the ease carries the page from there.
    ///
    /// The tick also says how fast the wheel was turning; a slow gesture says little and stops
    /// where it landed, a flick says more and carries on.
    pub fn nudge(
        &self,
        scroll: &impl ScrollPosition,
        gain: f32,
        precise: bool,
        window: &mut Window,
    ) {
        {
            let mut drift = self.drift.borrow_mut();
            // Where the platform has put the page, kept inside the page's own ends: a tick
            // against an end moves nothing, and so must leave no speed behind to carry the
            // page away from it.
            let landed = held(scroll.offset(), scroll);
            let moved = landed - drift.shown;
            drift.tick(moved, Instant::now());
            drift.beat = None;
            drift.eased = None;
            drift.velocity = Point::default();
            match precise {
                true => {
                    drift.shown = landed;
                    drift.target = landed;
                    drift.moving = Move::Native;
                }
                false => {
                    let step = moved * gain;
                    let from = match drift.moving {
                        Move::Native | Move::Still => drift.shown,
                        _ => drift.target,
                    };
                    // A tick the other way starts from where the page is now: the rest of the
                    // old glide was going somewhere the reader no longer wants to be.
                    let from = match opposes(from - drift.shown, step) {
                        true => drift.shown,
                        false => from,
                    };
                    drift.target = held(from + step, scroll);
                    drift.moving = Move::Ease;
                    scroll.set_offset(drift.shown);
                }
            }
        }
        self.schedule_frame(scroll, window);
    }

    pub fn aim(&self, scroll: &impl ScrollPosition, to: Point<Pixels>, window: &mut Window) {
        {
            let mut drift = self.drift.borrow_mut();
            if matches!(drift.moving, Move::Still | Move::Native) {
                let at = held(scroll.offset(), scroll);
                drift.shown = at;
            }
            drift.target = held(to, scroll);
            drift.coast = Point::default();
            drift.last_tick = None;
            drift.eased = Some(self.pace);
            let springing = self.spring.is_some();
            if springing != (drift.moving == Move::Spring) {
                drift.velocity = Point::default();
            }
            drift.moving = match springing {
                true => Move::Spring,
                false => Move::Ease,
            };
        }
        self.schedule_frame(scroll, window);
    }

    pub fn jump(&self, scroll: &impl ScrollPosition, to: Point<Pixels>) {
        let landed = held(to, scroll);
        self.drift.borrow_mut().stop(landed);
        scroll.set_offset(landed);
    }

    pub fn stop_spring(&self, scroll: &impl ScrollPosition) -> bool {
        let mut drift = self.drift.borrow_mut();
        if drift.moving != Move::Spring {
            return false;
        }
        drift.stop(held(scroll.offset(), scroll));
        true
    }

    pub fn goal(&self, scroll: &impl ScrollPosition) -> Point<Pixels> {
        let drift = self.drift.borrow();

        match drift.moving {
            Move::Still => scroll.offset(),
            _ => drift.target,
        }
    }

    pub fn presentation(&self, scroll: &impl ScrollPosition) -> Point<Pixels> {
        let drift = self.drift.borrow();

        match drift.moving == Move::Spring {
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
            if drift.moving == Move::Still {
                return;
            }

            let now = Instant::now();
            let elapsed = drift
                .beat
                .replace(now)
                .map(|beat| now.duration_since(beat).min(STALL))
                .unwrap_or(Duration::from_secs_f32(1. / HERTZ));
            let target = held(drift.target, scroll);

            // A trackpad is moving the page itself. Nothing is drawn here until the gesture
            // stops; then a fast one carries on and a slow one simply stops where it is.
            if drift.moving == Move::Native {
                let paused = drift
                    .last_tick
                    .is_some_and(|last| now.duration_since(last) >= COAST_PAUSE);
                if !paused {
                    drop(drift);
                    self.schedule_frame(scroll, window);
                    return;
                }
                let quick = drift.coast.x.abs() >= COAST_MIN || drift.coast.y.abs() >= COAST_MIN;
                match quick {
                    false => {
                        // The page is where the fingers left it, and no further. Its own ends
                        // are written back too, in case the platform took it past one.
                        let at = held(scroll.offset(), scroll);
                        drift.stop(at);
                        drop(drift);
                        scroll.set_offset(at);
                        return;
                    }
                    true => {
                        drift.moving = Move::Coast;
                        drift.target = drift.shown;
                        drift.eased = None;
                    }
                }
            }

            // The wheel has stopped and the gesture was fast enough to be worth carrying on
            // from: the page coasts on its own until it slows to nothing.
            if drift.moving == Move::Ease
                && drift
                    .last_tick
                    .is_some_and(|last| now.duration_since(last) >= COAST_PAUSE)
                && (drift.coast.x.abs() >= COAST_MIN || drift.coast.y.abs() >= COAST_MIN)
            {
                drift.moving = Move::Coast;
                drift.target = drift.shown;
                drift.eased = None;
            }

            let settled = match drift.moving {
                Move::Spring => {
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
                    drift.shown = held(point(px(x.position), px(y.position)), scroll);
                    drift.velocity = point(x.velocity, y.velocity);
                    spring.is_settled(x, target.x.as_f32(), SPRING_REST.as_f32())
                        && spring.is_settled(y, target.y.as_f32(), SPRING_REST.as_f32())
                }
                Move::Coast => {
                    let (shown, coast, ended) = coast_step(
                        drift.shown,
                        drift.coast,
                        elapsed.as_secs_f32(),
                        scroll.max_offset(),
                    );
                    drift.shown = shown;
                    drift.coast = coast;
                    drift.target = drift.shown;
                    ended
                }
                // The two moves that are not drawn here are handled above, and Still returns.
                Move::Ease => {
                    let pace = drift.eased.unwrap_or(EASE);
                    let ease = 1. - (1. - pace).powf(elapsed.as_secs_f32() * HERTZ);
                    let step = target - drift.shown;
                    drift.shown = held(drift.shown + point(step.x * ease, step.y * ease), scroll);
                    step.x.abs() < REST && step.y.abs() < REST
                }
                // Handled above: Still returns, and Native either returns or becomes a coast.
                Move::Still | Move::Native => true,
            };
            match settled {
                true => {
                    let rest = held(target, scroll);
                    drift.stop(rest);
                    rest
                }
                false => match drift.moving {
                    Move::Spring => grid(held(drift.shown, scroll), window),
                    _ => held(drift.shown, scroll),
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

/// One step of a coast: where it leaves the page, the speed it has left, and whether it has
/// ended. The page's own ends stop it — a coast that reached one has nothing left to carry, so
/// it neither presses on against the end nor turns back from it.
fn coast_step(
    shown: Point<Pixels>,
    coast: Point<f32>,
    seconds: f32,
    reach: Point<Pixels>,
) -> (Point<Pixels>, Point<f32>, bool) {
    let next = point(
        coast.x * (-COAST_FRICTION * seconds).exp(),
        coast.y * (-COAST_FRICTION * seconds).exp(),
    );
    let wanted = shown + point(px(next.x * seconds), px(next.y * seconds));
    let moved = clamped(wanted, reach);
    let ended = moved != wanted || (next.x.abs() < COAST_STOP && next.y.abs() < COAST_STOP);
    let coast = match moved != wanted {
        true => Point::default(),
        false => next,
    };
    (moved, coast, ended)
}

/// Whether two movements pull against each other, either axis included.
fn opposes(travel: Point<Pixels>, step: Point<Pixels>) -> bool {
    travel.x.as_f32() * step.x.as_f32() < 0. || travel.y.as_f32() * step.y.as_f32() < 0.
}

fn grid(at: Point<Pixels>, window: &Window) -> Point<Pixels> {
    point(snapped(at.x, window), snapped(at.y, window))
}

fn held(at: Point<Pixels>, scroll: &impl ScrollPosition) -> Point<Pixels> {
    clamped(at, scroll.max_offset())
}

/// `at` held inside the page's own ends, which lie between `-reach` and zero on each axis.
fn clamped(at: Point<Pixels>, reach: Point<Pixels>) -> Point<Pixels> {
    point(
        at.x.clamp(-reach.x.max(Pixels::ZERO), Pixels::ZERO),
        at.y.clamp(-reach.y.max(Pixels::ZERO), Pixels::ZERO),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reach of a page with eight hundred pixels below the fold.
    fn reach() -> Point<Pixels> {
        point(px(0.), px(800.))
    }

    #[test]
    fn a_coast_at_the_top_stops_against_it() {
        // A flick that would carry the page up out of the top: there is nowhere to go, and
        // nothing is left of the flick to carry it anywhere else.
        let (shown, coast, ended) =
            coast_step(point(px(0.), px(0.)), point(0., 5_000.), 1. / 60., reach());

        assert_eq!(shown, point(px(0.), px(0.)));
        assert_eq!(coast, Point::default());
        assert!(ended);
    }

    #[test]
    fn a_coast_at_the_bottom_stops_against_it() {
        let (shown, coast, ended) = coast_step(
            point(px(0.), px(-795.)),
            point(0., -5_000.),
            1. / 30.,
            reach(),
        );

        assert_eq!(shown, point(px(0.), px(-800.)));
        assert_eq!(coast, Point::default());
        assert!(ended);
    }

    #[test]
    fn a_coast_into_the_page_carries_and_slows() {
        let (shown, coast, ended) =
            coast_step(point(px(0.), px(0.)), point(0., -5_000.), 1. / 60., reach());

        assert!(shown.y < px(0.), "the page should have moved down");
        assert!(
            coast.y < 0. && coast.y > -5_000.,
            "the coast should still be going, and slower"
        );
        assert!(!ended);
    }

    #[test]
    fn a_tick_that_moved_nothing_leaves_no_speed() {
        // A wheel turned against the top of the page: each tick is drawn nowhere, so the
        // gesture leaves nothing for a coast to pick up.
        let mut drift = Drift::default();
        let now = Instant::now();
        drift.tick(point(px(0.), px(0.)), now);
        drift.tick(point(px(0.), px(0.)), now + Duration::from_millis(10));

        assert_eq!(drift.coast, Point::default());
    }

    #[test]
    fn a_flick_down_leaves_a_speed_to_carry_it() {
        let mut drift = Drift::default();
        let now = Instant::now();
        drift.tick(point(px(0.), px(-40.)), now);
        drift.tick(point(px(0.), px(-40.)), now + Duration::from_millis(8));

        assert!(
            drift.coast.y < 0.,
            "the page should coast the way it was sent"
        );
    }

    #[test]
    fn the_page_is_held_inside_its_own_ends() {
        assert_eq!(
            clamped(point(px(0.), px(400.)), reach()),
            point(px(0.), px(0.))
        );
        assert_eq!(
            clamped(point(px(0.), px(-900.)), reach()),
            point(px(0.), px(-800.))
        );
        assert_eq!(
            clamped(point(px(0.), px(-300.)), reach()),
            point(px(0.), px(-300.))
        );
    }
}
