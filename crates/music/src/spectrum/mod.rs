//! What is playing, analysed for drawing. `Spectrum` is the visualizer's 32 bands per channel of
//! what reaches the device. `analyzer` reads the music on its way into the equalizer, finely
//! enough to draw behind its response. `feed` carries a stage's samples to either.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::Thread;
use std::time::Duration;

use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;

use feed::{Feed, Heard, Listener};

pub(crate) use analyzer::Probe;
pub use analyzer::{Analyzer, Level, Watch};

mod analyzer;
mod feed;

const N_BANDS: usize = 32;
const FFT_SIZE: usize = 2048;
/// In samples of whatever the tap hears, so a stereo source gets four windows of slack: a
/// device fills its whole buffer in one go after a stall and anything past this is dropped.
const RING_CAPACITY: usize = FFT_SIZE * 16;
const MIN_FREQ: f32 = 100.;
const MAX_FREQ: f32 = 6_000.;
const GAIN: f32 = 8.;
/// The level past which a band is compressed towards the ceiling instead of cut at it. Loud
/// bands that are cut all sit at the same height, and neighbours there draw a flat crest.
const KNEE: f32 = 0.6;
const ATTACK: f32 = 0.9;
const DECAY: f32 = 0.12;
const IDLE_POLL: Duration = Duration::from_millis(4);
/// How long the analyzer sleeps on an empty ring once it has been empty for `QUIET_AFTER` polls
/// in a row, since paused the tap stays silent for hours. The ring holds far more audio.
const QUIET_POLL: Duration = Duration::from_millis(50);
const QUIET_AFTER: u32 = 250;
/// How long the analyzer sleeps on an empty ring while nothing holds a listen. The taps send
/// nothing then, so only a new listen can give it work, and that unparks it at once.
const UNWATCHED_POLL: Duration = Duration::from_millis(250);

/// The band levels of one channel, published by the analyzer thread and read by the UI.
#[derive(Clone)]
struct Lane {
    bands: Arc<Vec<AtomicU32>>,
}

impl Lane {
    fn new() -> Self {
        Self {
            bands: Arc::new((0..N_BANDS).map(|_| AtomicU32::new(0)).collect()),
        }
    }

    /// The band's levels, written into `out`, which is resized to the band's count. The
    /// caller keeps the buffer, so a frame that reads the spectrum allocates nothing.
    fn read(&self, out: &mut Vec<f32>) {
        out.clear();
        out.extend(
            self.bands
                .iter()
                .map(|band| f32::from_bits(band.load(Ordering::Relaxed))),
        );
    }

    fn set(&self, index: usize, value: f32) {
        self.bands[index].store(value.to_bits(), Ordering::Relaxed);
    }
}

/// The running spectrum of what is playing, one band set per channel. A mono source publishes
/// the same levels on both.
#[derive(Clone)]
pub struct Spectrum {
    left: Lane,
    right: Lane,
    /// Whether the taps hear the track before the user's volume rather than after it.
    absolute: Arc<AtomicBool>,
    /// How many `Listening` guards are held. The taps send nothing while there are none.
    listeners: Arc<AtomicUsize>,
    /// The analyzer thread, so a first listen wakes it out of its long wait instead of leaving
    /// the visualizer flat until the wait runs out.
    idle: Arc<Idle>,
}

/// Keeps the taps feeding the analyzer while held. Dropping the last one stops them.
pub struct Listening(Arc<AtomicUsize>);

impl Drop for Listening {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// The analyzer thread's handle on itself, so a listen can wake it while it waits on an empty
/// ring with nothing watching. The wait is a park, so waking it costs one token rather than a
/// channel or a clock of its own.
#[derive(Default)]
struct Idle(Mutex<Option<Thread>>);

impl Idle {
    /// Files the calling thread, which is the analyzer, so `wake` can reach it.
    fn settle(&self) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(std::thread::current());
    }

    /// Sleeps up to `limit`, ending early when `wake` arrives.
    fn wait(&self, limit: Duration) {
        std::thread::park_timeout(limit);
    }

    /// Wakes the analyzer out of a wait, for work a long one would delay.
    fn wake(&self) {
        if let Some(thread) = self.0.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            thread.unpark();
        }
    }
}

impl Spectrum {
    pub fn new() -> Self {
        Self {
            left: Lane::new(),
            right: Lane::new(),
            absolute: Arc::new(AtomicBool::new(false)),
            listeners: Arc::new(AtomicUsize::new(0)),
            idle: Arc::new(Idle::default()),
        }
    }

    /// Has the taps feed the analyzer for as long as the returned guard is held.
    pub fn listen(&self) -> Listening {
        self.listeners.fetch_add(1, Ordering::Relaxed);
        // The analyzer may be asleep with nothing to do; a first listen gives it something.
        self.idle.wake();
        Listening(self.listeners.clone())
    }

    /// Makes every tap on this spectrum hear the track before the user's volume, so the levels
    /// stop following it. The taps pick the change up at the next frame.
    pub fn set_absolute(&self, absolute: bool) {
        self.absolute.store(absolute, Ordering::Relaxed);
    }

    /// The left channel's bands, written into `out`.
    pub fn left(&self, out: &mut Vec<f32>) {
        self.left.read(out);
    }

    /// The right channel's bands, written into `out`.
    pub fn right(&self, out: &mut Vec<f32>) {
        self.right.read(out);
    }

    /// Both channels folded together, the louder of the two per band, written into `out`.
    pub fn bands(&self, out: &mut Vec<f32>) {
        self.left.read(out);
        for (band, right) in out.iter_mut().zip(self.right.bands.iter()) {
            *band = band.max(f32::from_bits(right.load(Ordering::Relaxed)));
        }
    }

    /// Starts the analyzer thread and returns the tap that feeds it. The tap has to be told
    /// the format of what it is fed through `Tap::format` before the first sample, and again
    /// whenever that changes.
    pub fn attach(&self) -> Tap {
        let (feed, listener) = feed::channel(RING_CAPACITY);
        let target = self.clone();
        let idle = self.idle.clone();
        let spawned = std::thread::Builder::new()
            .name("spectrum".to_owned())
            .spawn(move || {
                idle.settle();
                analyze(listener, target, idle);
            });
        if let Err(error) = spawned {
            log::error!("spectrum: cannot spawn analyzer thread: {error}");
        }
        Tap {
            feed,
            absolute: self.absolute.clone(),
            listeners: self.listeners.clone(),
            on: false,
        }
    }
}

impl Default for Spectrum {
    fn default() -> Self {
        Self::new()
    }
}

/// Where the samples go in. It sits on the source's side of the mixer, so what it hears is the
/// track's own rate and channel count, not the device's, and those can change with the track.
pub struct Tap {
    feed: Feed,
    absolute: Arc<AtomicBool>,
    listeners: Arc<AtomicUsize>,
    /// Whether the frame under way is sent, read once at its first sample.
    on: bool,
}

impl Tap {
    /// Declares the format of the frames that follow. Call it before the first sample and on
    /// every change; the analyzer takes it up from exactly the next frame.
    pub fn format(&mut self, rate: u32, channels: u16) {
        self.feed.format(rate, channels);
    }

    /// Whether the tap wants the samples from before the user's volume is applied.
    pub fn absolute(&self) -> bool {
        self.absolute.load(Ordering::Relaxed)
    }

    /// Takes one sample, `channel` being its place in its frame.
    pub fn push(&mut self, channel: u16, sample: f32) {
        if channel == 0 {
            self.on = self.listeners.load(Ordering::Relaxed) > 0;
        }
        if self.on {
            self.feed.push(channel, sample);
        }
    }
}

/// One channel's window of samples and the smoothing that follows it.
struct Side {
    lane: Lane,
    samples: [f32; FFT_SIZE],
    smoothed: [f32; N_BANDS],
}

impl Side {
    fn new(lane: Lane) -> Self {
        Self {
            lane,
            samples: [0.; FFT_SIZE],
            smoothed: [0.; N_BANDS],
        }
    }
}

fn hann_window() -> [f32; FFT_SIZE] {
    let mut window = [0f32; FFT_SIZE];
    for (i, value) in window.iter_mut().enumerate() {
        *value = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32).cos();
    }
    window
}

fn band_edges(rate: f32) -> Vec<usize> {
    let nyquist = rate / 2.;
    let max_freq = MAX_FREQ.min(nyquist);
    let min_freq = MIN_FREQ.min(max_freq * 0.5).max(1.);
    let bin_hz = rate / FFT_SIZE as f32;

    (0..=N_BANDS)
        .map(|i| {
            let t = i as f32 / N_BANDS as f32;
            let freq = min_freq * (max_freq / min_freq).powf(t);
            ((freq / bin_hz) as usize).clamp(1, FFT_SIZE / 2 - 1)
        })
        .collect()
}

fn analyze(mut listener: Listener, spectrum: Spectrum, idle: Arc<Idle>) {
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(FFT_SIZE);
    let window = hann_window();
    let mut edges = Vec::new();
    // One accumulator per side: a source with more than two channels folds its odd lanes left
    // and its even ones right, which is close enough for a visualizer.
    let mut sides = [Side::new(spectrum.left), Side::new(spectrum.right)];
    let mut filled = 0usize;
    let mut buffer = vec![Complex32::default(); FFT_SIZE];
    // Held for the life of the analyzer, since `process` allocates a fresh one on every call.
    let mut scratch = vec![Complex32::default(); fft.get_inplace_scratch_len()];
    let mut empty = 0u32;

    loop {
        let Some(heard) = listener.next() else {
            if listener.abandoned() {
                return;
            }
            empty = empty.saturating_add(1);
            // Nothing watching means the taps send nothing, so only a listen can give the
            // analyzer work, and that unparks it at once. While something watches it keeps a
            // short clock, since a paused track stays silent for hours.
            let wait = match spectrum.listeners.load(Ordering::Relaxed) > 0 {
                true => match empty > QUIET_AFTER {
                    true => QUIET_POLL,
                    false => IDLE_POLL,
                },
                false => UNWATCHED_POLL,
            };
            idle.wait(wait);
            continue;
        };
        let frame = match heard {
            Heard::Frame(frame) => frame,
            Heard::Format(layout) => {
                edges = band_edges(layout.rate.max(1) as f32);
                filled = 0;
                continue;
            }
        };
        empty = 0;

        for (index, side) in sides.iter_mut().enumerate() {
            let mut sum = 0.;
            let mut taken = 0usize;
            for sample in frame.iter().skip(index).step_by(2) {
                sum += sample;
                taken += 1;
            }
            side.samples[filled] = match taken {
                0 => frame.iter().sum::<f32>() / frame.len() as f32,
                taken => sum / taken as f32,
            };
        }
        filled += 1;
        if filled < FFT_SIZE {
            continue;
        }
        filled = 0;

        for side in sides.iter_mut() {
            for ((slot, sample), weight) in buffer.iter_mut().zip(side.samples).zip(&window) {
                *slot = Complex32::new(sample * weight, 0.);
            }
            fft.process_with_scratch(&mut buffer, &mut scratch);

            for (band, edge) in edges.windows(2).enumerate() {
                let lo = edge[0];
                let hi = edge[1].max(lo + 1);
                let magnitude = buffer[lo..hi]
                    .iter()
                    .map(|bin| bin.norm())
                    .fold(0f32, f32::max);
                let target = soften((magnitude * GAIN / (FFT_SIZE as f32 / 2.)).sqrt());
                let rate = match target > side.smoothed[band] {
                    true => ATTACK,
                    false => DECAY,
                };
                side.smoothed[band] += (target - side.smoothed[band]) * rate;
                side.lane.set(band, side.smoothed[band]);
            }
        }
    }
}

/// A band's level on a curve that is linear up to `KNEE` and eases towards 1 above it without
/// reaching it, so two loud bands still land at different heights.
fn soften(level: f32) -> f32 {
    match level <= KNEE {
        true => level,
        false => KNEE + (1. - KNEE) * ((level - KNEE) / (1. - KNEE)).tanh(),
    }
}
