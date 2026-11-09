//! The swell a word earns by being held: a word sung longer than the words around it grows
//! letter by letter, the movement travelling along it rather than pulsing all at once.
//!
//! The thresholds and the shape of the move come from BitChord, which reads a held note as
//! a wave running through the word rather than the word pulsing as a whole.

use std::sync::Arc;
use std::time::Duration;

use music::LyricsWord;

/// Longest word worth running a wave through; past this it reads as a ripple.
const GROW_MAX_CHARS: usize = 7;

/// How long a word of each length has to be held before it qualifies.
const GROW_MIN_SOLO_MS: u64 = 1_100;
const GROW_MIN_SHORT_MS: u64 = 1_360;
const GROW_SHORT_STEP_MS: u64 = 140;
const GROW_MIN_FOUR_MS: u64 = 1_050;
const GROW_MIN_LONG_MS: u64 = 900;
const GROW_MS_PER_CHAR: u64 = 200;

/// How much of the movement is taken back off the later letters of a word.
const GROW_DECAY_LONG_CHARS: usize = 5;
const GROW_DECAY_QUICK_MS: f32 = 1_200.;
const GROW_DECAY_QUICK_FLOOR_MS: f32 = 800.;
const GROW_DECAY_LONG: f32 = 0.4;
const GROW_DECAY_QUICK: f32 = 0.3;
const GROW_DECAY_QUICK_TINY: f32 = 0.1;
const GROW_DECAY_MAX: f32 = 0.7;

/// Each letter starts this far — as a share of the word's own length — after the one before
/// it, so the swell travels rather than pulses.
const GROW_STAGGER: f32 = 0.09;

/// The whole move runs half again as long as the word is held.
const GROW_SPAN: f32 = 1.5;

/// Up by [`GROW_IN`], held to [`GROW_HOLD`], settled by [`GROW_OUT`].
const GROW_IN: f32 = 0.25;
const GROW_HOLD: f32 = 0.30;
const GROW_OUT: f32 = 0.75;

/// Where a letter comes to rest: the same small lift every sung word carries.
const GROW_REST: f32 = 1.;

/// The window over which a longer hold earns more of the treatment.
const GROW_RAMP_MIN_MS: f32 = 400.;
const GROW_RAMP_MAX_MS: f32 = 3_000.;

/// Swell every qualifying letter gets, and the range the hold adds on top.
const GROW_BASE_SHORT: f32 = 0.05;
const GROW_BASE_LONG: f32 = 0.04;
const GROW_SCALE_RANGE: f32 = 0.08;

/// The most any letter swells, which is what the lift is read off.
const GROW_SCALE_CEILING: f32 = 0.1;

/// Taken back off the swell so it stops a hair short of the ceiling.
const GROW_SCALE_TRIM: f32 = 0.98;

/// Lean, as a share of the font size at full swell.
const GROW_SHIFT_EM: f32 = 25. / 34.;

/// The bloom's floor, and what the hold adds to it.
const GROW_BLOOM_FLOOR: f32 = 0.35;
const GROW_BLOOM_RANGE: f32 = 0.45;
const GROW_BLOOM_PACE_MS: f32 = 1_500.;
const GROW_BLOOM_PACE_MAX: f32 = 1.1;
const GROW_BLOOM_SHORT: f32 = 0.85;
const GROW_BLOOM_LONG: f32 = 1.1;

/// A word held this long lifts as far as it is going to.
const GROW_LIFT_PACE_MS: f32 = 2_000.;
const GROW_LIFT_FLOOR: f32 = 0.3;

/// Where one letter of a held word has got to.
#[derive(Clone, Copy, Default)]
pub(crate) struct CharGrowth {
    /// The letter's own size, as a multiple of the size it was laid out at.
    pub(crate) scale: f32,
    /// The letter's lean away from the middle of the word, in ems.
    pub(crate) shift: f32,
    /// The letter's lift above the lift every sung word already carries, in multiples of
    /// the word's own rise. Nothing before the swell, and nothing once it has settled.
    pub(crate) rise: f32,
    /// How brightly the letter glows, from nothing to full.
    pub(crate) bloom: f32,
}

/// A word held long enough to be animated a letter at a time.
///
/// Every letter runs the same three-beat move — swell up and forward, hold, then settle back
/// to the lift every sung word carries — but each one starts a little after the one before
/// it, so the movement travels along the word instead of the whole thing pulsing at once.
///
/// The peaks differ letter by letter as well. Later letters of a long word, and every letter
/// of a word only just held long enough to qualify, move and light less: without that a
/// seven-letter word read as a wave with a tail that kept going after the singer had stopped.
pub(crate) struct GrowingWord {
    /// The word's own text, one letter at a time.
    pub(crate) letters: Vec<char>,
    /// When the word starts being sung.
    pub(crate) start: Duration,
    /// When the word is last held.
    pub(crate) end: Duration,
    scales: Vec<f32>,
    shifts: Vec<f32>,
    rises: Vec<f32>,
    blooms: Vec<f32>,
}

/// The words of a line held long enough to swell, with the swell itself.
///
/// Words are numbered the way the line numbers them, so a caller can line the swell up with
/// the words it already has.
pub(crate) fn held_words(words: &[LyricsWord]) -> Vec<(usize, Arc<GrowingWord>)> {
    words
        .iter()
        .enumerate()
        .filter(|(_, word)| qualifies(word))
        .map(|(index, _)| (index, Arc::new(GrowingWord::new(words, index))))
        .collect()
}

/// How long a word is held for the swell: from when it starts being sung until the next word
/// takes over, which is the stretch the karaoke sweep follows as well.
pub(crate) fn held_span(words: &[LyricsWord], index: usize) -> (Duration, Duration) {
    let word = &words[index];
    let start = word.start;
    let sung = word.end.max(start);
    let end = match words.get(index + 1) {
        Some(next) => next.start.max(start).min(sung),
        None => sung,
    };
    (start, end)
}

impl GrowingWord {
    fn new(words: &[LyricsWord], index: usize) -> Self {
        let word = &words[index];
        let letters = word.text.trim().chars().collect::<Vec<_>>();
        let chars = letters.len();
        let (start, end) = held_span(words, index);
        let held = end.saturating_sub(start);
        let held_ms = held.as_secs_f32() * 1_000.;
        // How much of the full treatment this word has earned. Cubed, so the difference
        // between a word held for a second and one held for three is most of the range and
        // everything under a second is barely anything — the alternative reads as every
        // word twitching.
        let earned =
            ((held_ms - GROW_RAMP_MIN_MS) / (GROW_RAMP_MAX_MS - GROW_RAMP_MIN_MS)).clamp(0., 1.);
        let earned = earned * earned * earned;
        let decay = decay_rate(chars, held_ms);
        // A word barely over the line glows less than one held twice as long, and a long
        // word spreads what it has over more letters.
        let bloom_pace = (held_ms / GROW_BLOOM_PACE_MS).min(GROW_BLOOM_PACE_MAX);
        let bloom_spread = match chars {
            0..=3 => GROW_BLOOM_SHORT,
            6.. => GROW_BLOOM_LONG,
            _ => 1.,
        };
        // Short words swell a little more, having fewer letters to do it with.
        let base = match chars {
            0..=3 => GROW_BASE_SHORT,
            _ => GROW_BASE_LONG,
        };
        let lift_pace = (held_ms / GROW_LIFT_PACE_MS).clamp(GROW_LIFT_FLOOR, 1.);

        let mut scales = Vec::with_capacity(chars);
        let mut shifts = Vec::with_capacity(chars);
        let mut rises = Vec::with_capacity(chars);
        let mut blooms = Vec::with_capacity(chars);
        for index in 0..chars {
            let place = match chars > 1 {
                true => index as f32 / (chars - 1) as f32,
                false => 0.,
            };
            let reach = earned * (1. - place * decay);
            let scale = 1. + base + reach * GROW_SCALE_RANGE;
            scales.push(scale * GROW_SCALE_TRIM);
            blooms.push((GROW_BLOOM_FLOOR + reach * GROW_BLOOM_RANGE) * bloom_pace * bloom_spread);
            // The lift is read off the swell rather than set on its own: a letter that grows
            // more rises further, which is what keeps the two reading as one movement.
            rises.push(((scale - 1.) / GROW_SCALE_CEILING) * lift_pace);
            // Letters lean away from the middle of the word as it swells, so it opens
            // outwards rather than every letter sliding the same way.
            let centre = (index as f32 + 0.5) / chars as f32;
            shifts.push((centre - 0.5) * 2. * (scale - 1.) * GROW_SHIFT_EM * GROW_SCALE_TRIM);
        }

        Self {
            letters,
            start,
            end,
            scales,
            shifts,
            rises,
            blooms,
        }
    }

    /// Where the letter at `index` has got to at `position`.
    pub(crate) fn sample(&self, index: usize, position: Duration) -> CharGrowth {
        let span = self.end.saturating_sub(self.start).as_secs_f32().max(1e-6);
        let elapsed =
            position.saturating_sub(self.start).as_secs_f32() - index as f32 * span * GROW_STAGGER;
        let phase = (elapsed / (span * GROW_SPAN)).clamp(0., 1.);
        let peak_scale = self.scales[index];
        // the lift only counts above the lift every sung word already carries, so a word
        // that has settled comes to rest on the line with the rest of it
        let peak_rise = (self.rises[index] - GROW_REST).max(0.);
        match phase {
            _ if phase < GROW_IN => {
                let t = smooth(phase / GROW_IN);
                CharGrowth {
                    scale: 1. + (peak_scale - 1.) * t,
                    shift: self.shifts[index] * t,
                    rise: peak_rise * t,
                    bloom: self.blooms[index] * t,
                }
            }
            _ if phase < GROW_HOLD => CharGrowth {
                scale: peak_scale,
                shift: self.shifts[index],
                rise: peak_rise,
                bloom: self.blooms[index],
            },
            _ if phase < GROW_OUT => {
                let t = smooth((phase - GROW_HOLD) / (GROW_OUT - GROW_HOLD));
                CharGrowth {
                    scale: peak_scale + (1. - peak_scale) * t,
                    shift: self.shifts[index] * (1. - t),
                    rise: peak_rise * (1. - t),
                    bloom: self.blooms[index] * (1. - t),
                }
            }
            _ => CharGrowth {
                scale: 1.,
                shift: 0.,
                rise: 0.,
                bloom: 0.,
            },
        }
    }
}

/// Whether a word is held long enough, and is short enough, to be animated a letter at a
/// time.
///
/// The thresholds are steeper for short words than the length alone suggests: a two-letter
/// word held for a second is a held note, whereas a seven-letter one held for the same
/// second is just being sung at an ordinary pace, and only the first earns the swell.
fn qualifies(word: &LyricsWord) -> bool {
    let text = word.text.trim();
    let length = text.chars().count();
    if length == 0 || length > GROW_MAX_CHARS {
        return false;
    }
    // A hyphenated word is really two words and would break at the hyphen, and a word with
    // a space in it is a phrase rather than a word.
    if text.contains('-') || text.chars().any(char::is_whitespace) {
        return false;
    }
    // Scripts whose letters are not laid out side by side independently are left alone:
    // Han, kana and Hangul draw as blocks, and right-to-left scripts join up, so moving one
    // letter of a word would come apart rather than swell.
    if text
        .chars()
        .any(|letter| block_script(letter) || joined_script(letter))
    {
        return false;
    }
    let held = word
        .end
        .max(word.start)
        .saturating_sub(word.start)
        .as_millis() as u64;
    match length {
        1 => held >= GROW_MIN_SOLO_MS,
        2..=3 => held >= GROW_MIN_SHORT_MS + (length as u64 - 2) * GROW_SHORT_STEP_MS,
        4 => held >= GROW_MIN_FOUR_MS,
        _ => held >= GROW_MIN_LONG_MS && held >= length as u64 * GROW_MS_PER_CHAR,
    }
}

/// How much of the movement is taken back off the later letters of a word.
///
/// Two reasons to hold back. A long word is a lot of letters to run a wave through, and left
/// flat the tail of it is still swelling well after the singer has moved on. A word held only
/// just long enough to qualify has no room for a wave at all, so the whole thing is damped
/// towards a single swell.
fn decay_rate(length: usize, held: f32) -> f32 {
    let long = length > GROW_DECAY_LONG_CHARS;
    let quick = held < GROW_DECAY_QUICK_MS;
    if !long && !quick {
        return 0.;
    }
    let mut strength = 0.;
    if long {
        strength += (((length - GROW_DECAY_LONG_CHARS) as f32) / 5.).min(1.) * GROW_DECAY_LONG;
    }
    if quick {
        let short = (1. - (held - GROW_DECAY_QUICK_FLOOR_MS) / 400.).max(0.);
        strength += short
            * match length > 3 {
                true => GROW_DECAY_QUICK,
                false => GROW_DECAY_QUICK_TINY,
            };
    }
    strength.min(GROW_DECAY_MAX)
}

/// Ease in and out of the ends, so the swell has no corners on it.
fn smooth(fraction: f32) -> f32 {
    fraction * fraction * (3. - 2. * fraction)
}

/// Whether a letter is drawn as a block of its own, which cannot be moved one at a time.
fn block_script(letter: char) -> bool {
    matches!(letter,
        '\u{4E00}'..='\u{9FFF}'
        | '\u{3040}'..='\u{309F}'
        | '\u{30A0}'..='\u{30FF}'
        | '\u{AC00}'..='\u{D7AF}')
}

/// Whether a letter joins up with the one before it, which cannot be moved on its own.
fn joined_script(letter: char) -> bool {
    matches!(letter, '\u{0590}'..='\u{08FF}')
}
