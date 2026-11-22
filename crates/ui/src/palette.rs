use std::f32::consts::TAU;
use std::sync::OnceLock;

use gpui::{App, Hsla, RenderImage, Rgba, SharedString, Task};

use crate::artwork::sample_palette;

const BINS: usize = 24;
const SAMPLES: usize = 6000;
const MIN_ALPHA: u8 = 128;
const MIN_SATURATION: f32 = 0.14;
/// Dark covers still name their hue: the ambient background and the selection tint both
/// follow near-black art, so the floor sits well below picture grey.
const MIN_LIGHTNESS: f32 = 0.10;
const MAX_LIGHTNESS: f32 = 0.94;
/// The saturation and lightness band a pixel must fall in before it may name an
/// accent on its own, rather than as part of a larger muted field. A sleeve with
/// a bright accent on a dark ground is the common case, and counting the ground
/// with the accent averages the two into a colour that belongs to neither.
const VIVID_SATURATION: f32 = 0.34;
const VIVID_MIN_LIGHTNESS: f32 = 0.14;
const VIVID_MAX_LIGHTNESS: f32 = 0.84;
const MIN_SHARE: f32 = 0.01;
/// A peak also needs this much absolute weight, so a lone splash on an
/// otherwise empty image never names a tint no matter the shares.
const MIN_WEIGHT: f32 = 25.;
/// A runner-up hue only counts when it owns this share of the sampled pixels
/// and stands apart from the lead.
const SECONDARY_SHARE: f32 = 0.02;
const SECONDARY_WEIGHT: f32 = 10.;
const SECONDARY_GAP: usize = 3;
const SECONDARY_HUE: f32 = 0.05;
/// How much a pixel's own saturation and lightness count beside the number of
/// pixels sharing its colour. The rest of the weight is its share alone.
const BIAS: f32 = 0.3;
/// The side of the RGB cube the bins cut the artwork into when `cube` is on:
/// sixteen steps a channel, so a dark field and the vivid accent inside it are
/// separate candidates instead of one averaged hue.
const CUBE: usize = 16;
/// The saturation the cover's edge must reach before it names the accent itself. An
/// edge that is grey leaves the picture to say which colour the cover is.
const SEAM_SATURATION: f64 = 0.34;
/// The lightness an edge is read at. An edge brighter than the art it frames says
/// nothing about the art, so it is read as if it were darker.
const SEAM_LIGHTNESS: f64 = 0.34;
/// How far a zone's own colour is mixed toward the median of its pixels. The mean is
/// pulled by a bright corner; the median is what the strip itself looks like.
const SEAM_MEDIAN: f64 = 0.68;
/// The grey a cover with no pixels in its edge leaves behind.
const SEAM_FALLBACK: Rgb = [42, 42, 42];
/// The distance below which a colour's channels are too close to tell a hue apart. A pixel
/// one level off white has no colour in it, but the saturation below reads as fully
/// saturated near either end of the range and the hue names whichever channel happens to
/// be the largest, so a thumbnail's white edge named a saturated teal. A colour this close
/// to grey is read as the grey it is.
const ACHROMATIC: f64 = 2. / 255.;
/// The saturation a colour must reach before it may name an accent on its own.
const SUPPORT_SATURATION: f64 = 0.34;
/// The lightness band a colour must fall in before it may name an accent on its own.
const SUPPORT_MIN_LIGHTNESS: f64 = 0.14;
const SUPPORT_MAX_LIGHTNESS: f64 = 0.84;
/// How close a colour must sit to the edge's own hue to be preferred over one far from
/// it, and the weights it carries for sitting near it and for sitting away.
const SUPPORT_NEAR: f64 = 48.;
const SUPPORT_NEAR_WEIGHT: f64 = 1.18;
const SUPPORT_FAR_WEIGHT: f64 = 0.82;
/// The number of pixels the search for a supporting colour walks. A cover larger than
/// this is read on a stride, so the work does not grow with the picture.
const SUPPORT_PIXELS: f64 = 160_000.;
/// The number of pixels a colour must hold before it may name an accent on its own, so
/// a lone splash on an otherwise empty cover never names a tint. The floor sits well
/// under what any deliberate accent covers and well over the stray colour a picture
/// carries in its compression.
const SUPPORT_MIN_WEIGHT: f64 = 25.;
/// The saturation the accent wears. A cover's own colour may be paler than the accent it
/// names, so the accent is never less saturated than this.
const ACCENT_SATURATION: f64 = 0.82;
/// The lightness the accent wears, and the one it takes from a pale edge.
const ACCENT_LIGHTNESS: f64 = 0.61;
const ACCENT_BRIGHT_LIGHTNESS: f64 = 0.48;
/// The edge lightness above which the accent is darkened, so a pale edge does not wash
/// it out.
const ACCENT_BRIGHT: f64 = 0.72;
/// The saturation below which a cover with no colour in it reads as having no hue.
const MONOCHROME_SATURATION: f64 = 0.18;

/// The palette of a frame that is already decoded, for artwork the cache has in
/// hand. Sampling walks at most `SAMPLES` pixels, so this costs nothing beside
/// the decode that produced the frame.
pub(crate) fn of_image(image: &RenderImage) -> CoverPalette {
    let size = image.size(0);
    let (width, height) = (size.width.0.max(0) as usize, size.height.0.max(0) as usize);

    image
        .as_bytes(0)
        .map(|pixels| sampled(pixels, width, height))
        .unwrap_or_default()
}

/// The dominant hue of the artwork at `url`, or none for near-greyscale art.
pub fn tint(url: impl Into<SharedString>, cx: &mut App) -> Task<Option<Hsla>> {
    let palette = palette(url, cx);

    cx.spawn(async move |_| palette.await.primary)
}

/// What one artwork extraction names: the hue that leads and, when the art
/// carries a real second family, the runner-up. Sampled once per cover and
/// shared by the theme tint and the ambient background, so the two never disagree.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct CoverPalette {
    pub primary: Option<Hsla>,
    pub secondary: Option<Hsla>,
    /// The mean lightness of the art, over every pixel sampled rather than the
    /// coloured ones alone. It is what names a neutral for art with no hue: a
    /// sleeve that is nearly black and one that is nearly white both land here
    /// with no primary, and only this tells them apart.
    pub lightness: f32,
    /// The mean lightness of the art's top-left corner, which is the part the
    /// chrome beside the page shows. A page's field can be light where the
    /// panel sits and dark elsewhere, so the whole frame's mean cannot say
    /// whether text drawn on the panel needs white or black.
    pub corner: f32,
}

/// Both hues of the artwork at `url` in a single pass over its pixels. A cover the
/// artwork cache has drawn answers at once, and any other is decoded small and dropped.
pub fn palette(url: impl Into<SharedString>, cx: &mut App) -> Task<CoverPalette> {
    sample_palette(url.into(), cx)
}

/// The knobs of hue selection. Every field's default is the shipped behaviour;
/// a debug build reads `SN_TINT_*` overrides so a tint can be judged against real
/// artwork by relaunching rather than rebuilding. Release ignores the variables.
struct Tuning {
    /// Bin by colour in RGB space rather than by hue alone; without it a dark
    /// field and the accent inside it average into one mediocre hue.
    cube: bool,
    /// Divide a bin's weight by the square root of its population, so a vivid
    /// minority can outrank a large muted area.
    compress: bool,
    min_saturation: f32,
    min_lightness: f32,
    max_lightness: f32,
    /// The band a pixel must fall in before it may name an accent on its own.
    /// The loose floors above still apply underneath: a vivid pixel has to be
    /// a colour at all before it is a vivid one.
    vivid_saturation: f32,
    vivid_min_lightness: f32,
    vivid_max_lightness: f32,
    /// How much saturation and lightness count beside a colour's share.
    bias: f32,
}

/// The knobs in force for this process: the defaults, and in a debug build
/// whatever the environment overrides the first time a palette is sampled.
fn tuning() -> &'static Tuning {
    static TUNING: OnceLock<Tuning> = OnceLock::new();

    TUNING.get_or_init(Tuning::defaults)
}

impl Tuning {
    fn defaults() -> Self {
        Self {
            cube: false,
            compress: false,
            min_saturation: MIN_SATURATION,
            min_lightness: MIN_LIGHTNESS,
            max_lightness: MAX_LIGHTNESS,
            vivid_saturation: VIVID_SATURATION,
            vivid_min_lightness: VIVID_MIN_LIGHTNESS,
            vivid_max_lightness: VIVID_MAX_LIGHTNESS,
            bias: BIAS,
        }
        .overridden()
    }

    /// Reads the tuning variables a debug build may set. A variable that is
    /// absent or unparseable leaves its default alone.
    fn overridden(self) -> Self {
        #[cfg(debug_assertions)]
        {
            let mut tuning = self;
            tuning.cube = flag("SN_TINT_CUBE").unwrap_or(tuning.cube);
            tuning.compress = flag("SN_TINT_SQRT").unwrap_or(tuning.compress);
            tuning.min_saturation = number("SN_TINT_SAT").unwrap_or(tuning.min_saturation);
            tuning.min_lightness = number("SN_TINT_LMIN").unwrap_or(tuning.min_lightness);
            tuning.max_lightness = number("SN_TINT_LMAX").unwrap_or(tuning.max_lightness);
            tuning.vivid_saturation =
                number("SN_TINT_VIVID_SAT").unwrap_or(tuning.vivid_saturation);
            tuning.vivid_min_lightness =
                number("SN_TINT_VIVID_LMIN").unwrap_or(tuning.vivid_min_lightness);
            tuning.vivid_max_lightness =
                number("SN_TINT_VIVID_LMAX").unwrap_or(tuning.vivid_max_lightness);
            tuning.bias = number("SN_TINT_BIAS").unwrap_or(tuning.bias);
            tuning
        }
        #[cfg(not(debug_assertions))]
        self
    }

    /// Whether a colour is vivid enough to name an accent on its own, rather
    /// than as part of a larger muted field.
    fn is_vivid(&self, colour: Hsla) -> bool {
        colour.s >= self.vivid_saturation
            && colour.l >= self.vivid_min_lightness
            && colour.l <= self.vivid_max_lightness
    }
}

/// A tuning flag: `1`, `true` or `on` turns it on, `0`, `false` or `off` turns it
/// off, and anything else is ignored.
#[cfg(debug_assertions)]
fn flag(name: &str) -> Option<bool> {
    match std::env::var(name)
        .ok()?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

/// A tuning number, ignored when it does not parse.
#[cfg(debug_assertions)]
fn number(name: &str) -> Option<f32> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// How much a bin weighs in the race for the lead: its summed colour weight, or
/// that weight divided by the square root of its population when compression is
/// on, which prices a large muted field below a small vivid one.
fn mass(bin: &Bin, tuning: &Tuning) -> f32 {
    match (tuning.compress, bin.population) {
        (true, population) if population > 0. => bin.weight / population.sqrt(),
        _ => bin.weight,
    }
}

/// A colour as the cover's own bytes carry it.
type Rgb = [u8; 3];

/// A bin of the RGB cube, holding the weight of the pixels that landed in it and the sum
/// of their colour.
#[derive(Clone, Copy, Default)]
struct CubeBin {
    weight: f64,
    sums: [f64; 3],
}

#[derive(Clone, Copy, Default)]
struct Bin {
    weight: f32,
    /// The pixels that landed here, which is what compression prices.
    population: f32,
    x: f32,
    y: f32,
    saturation: f32,
    lightness: f32,
}

impl Bin {
    fn add(&mut self, color: Hsla, weight: f32) {
        let angle = color.h * TAU;
        self.weight += weight;
        self.population += 1.;
        self.x += angle.cos() * weight;
        self.y += angle.sin() * weight;
        self.saturation += color.s * weight;
        self.lightness += color.l * weight;
    }

    fn merge(&mut self, other: &Self) {
        self.weight += other.weight;
        self.population += other.population;
        self.x += other.x;
        self.y += other.y;
        self.saturation += other.saturation;
        self.lightness += other.lightness;
    }

    fn colour(&self) -> Option<Hsla> {
        (self.weight > 0.).then(|| Hsla {
            h: self.y.atan2(self.x).rem_euclid(TAU) / TAU,
            s: (self.saturation / self.weight).clamp(0., 1.),
            l: (self.lightness / self.weight).clamp(0., 1.),
            a: 1.,
        })
    }
}

fn sampled(pixels: &[u8], width: usize, height: usize) -> CoverPalette {
    let tuning = tuning();
    let stride = (pixels.len() / 4 / SAMPLES).max(1);
    let mut bins = [Bin::default(); BINS];
    let mut blocks = match tuning.cube {
        true => vec![Bin::default(); CUBE * CUBE * CUBE],
        false => Vec::new(),
    };
    // The vivid pool holds only the colours bright enough to name an accent on
    // their own. It leads when it has anything to say, and the loose pool
    // answers for a cover that has nothing vivid in it.
    let mut vivid = [Bin::default(); BINS];
    let mut vivid_blocks = match tuning.cube {
        true => vec![Bin::default(); CUBE * CUBE * CUBE],
        false => Vec::new(),
    };
    let mut sampled = 0.;
    let mut lightness = 0.;
    let mut corner = 0.;
    let mut cornered = 0.;

    for (index, &[blue, green, red, alpha]) in
        pixels.as_chunks::<4>().0.iter().enumerate().step_by(stride)
    {
        if alpha < MIN_ALPHA {
            continue;
        }
        sampled += 1.;

        let colour = Hsla::from(Rgba {
            r: red as f32 / 255.,
            g: green as f32 / 255.,
            b: blue as f32 / 255.,
            a: 1.,
        });
        lightness += colour.l;

        // The panel beside the page shows the artwork's left edge, so its own corner is what
        // tells white text from black there. A wide banner and a square sleeve are cropped
        // differently, hence a generous share of the frame rather than a sliver.
        if width > 0 && height > 0 {
            let (x, y) = (index % width, index / width);
            if x * 10 < width * 3 && y * 10 < height * 6 {
                corner += colour.l;
                cornered += 1.;
            }
        }

        if colour.s < tuning.min_saturation
            || colour.l < tuning.min_lightness
            || colour.l > tuning.max_lightness
        {
            continue;
        }

        // On a dark cover the little light colour is the accent, not the
        // black around it: weight lifts with lightness so highlights win.
        let factor = colour.s * (tuning.bias + (1. - tuning.bias) * colour.l);
        let bright = tuning.is_vivid(colour);
        match tuning.cube {
            true => {
                let index = cube_bin(red, green, blue);
                blocks[index].add(colour, factor);
                if bright {
                    vivid_blocks[index].add(colour, factor);
                }
            }
            false => {
                let bin = ((colour.h * BINS as f32) as usize).min(BINS - 1);
                bins[bin].add(colour, factor);
                if bright {
                    vivid[bin].add(colour, factor);
                }
            }
        }
    }

    let mean = match sampled > 0. {
        true => lightness / sampled,
        false => 0.,
    };
    let corner = match cornered > 0. {
        true => corner / cornered,
        false => mean,
    };

    // The accent the cover names: the colour of its edge when that edge carries one,
    // and the most vivid colour in the picture otherwise. A cover with no colour in it
    // names none, and the theme falls back to its own.
    let primary = accent(pixels, width, height);

    // A second family of colour, for the background behind the page to carry. It comes
    // from the vivid pool, which is where a cover's other colour lives, and it has to
    // be a family of its own rather than a shade of the accent.
    let secondary = match tuning.cube {
        true => pick(&vivid_blocks, &blocks, sampled, tuning),
        false => pick(&vivid, &bins, sampled, tuning),
    }
    .and_then(|(pool, peak)| {
        let slots = pool.len();
        let runner = (0..slots)
            .filter(|&index| {
                let apart = match tuning.cube {
                    true => index != peak,
                    false => {
                        index.abs_diff(peak).min(slots - index.abs_diff(peak)) >= SECONDARY_GAP
                    }
                };
                apart
                    && score(pool, index, tuning)
                        >= (sampled * SECONDARY_SHARE).max(SECONDARY_WEIGHT)
            })
            .max_by(|&a, &b| score(pool, a, tuning).total_cmp(&score(pool, b, tuning)));
        runner
            .and_then(|index| {
                let mut cluster = pool[index];
                if !tuning.cube {
                    cluster.merge(&pool[(index + slots - 1) % slots]);
                    cluster.merge(&pool[(index + 1) % slots]);
                }
                cluster.colour()
            })
            .filter(|colour| {
                primary.is_some_and(|lead| {
                    (colour.h - lead.h)
                        .abs()
                        .min(1. - (colour.h - lead.h).abs())
                        >= SECONDARY_HUE
                })
            })
    });

    CoverPalette {
        primary,
        secondary,
        lightness: mean,
        corner,
    }
}

/// The RGB-cube bin a pixel lands in, four bits a channel.
fn cube_bin(red: u8, green: u8, blue: u8) -> usize {
    ((red as usize >> 4) << 8) | ((green as usize >> 4) << 4) | (blue as usize >> 4)
}

/// The bin that leads a pool and clears the bar to name a tint, if any.
fn lead(pool: &[Bin], sampled: f32, tuning: &Tuning) -> Option<usize> {
    let peak = (0..pool.len())
        .max_by(|&a, &b| score(pool, a, tuning).total_cmp(&score(pool, b, tuning)))?;
    (score(pool, peak, tuning) >= (sampled * MIN_SHARE).max(MIN_WEIGHT)).then_some(peak)
}

/// The pool that names the tint and the bin within it that leads. The vivid
/// pool leads when it clears the bar; a cover with nothing vivid in it falls
/// back to the loose pool, so a muted sleeve still gets a hue of its own.
fn pick<'a>(
    vivid: &'a [Bin],
    loose: &'a [Bin],
    sampled: f32,
    tuning: &Tuning,
) -> Option<(&'a [Bin], usize)> {
    if let Some(peak) = lead(vivid, sampled, tuning) {
        return Some((vivid, peak));
    }
    lead(loose, sampled, tuning).map(|peak| (loose, peak))
}

/// How strongly a bin argues for the lead. A hue bin borrows half of each
/// neighbour's, since art smears one colour across two of them.
fn score(pool: &[Bin], index: usize, tuning: &Tuning) -> f32 {
    let slots = pool.len();
    let lead = mass(&pool[index], tuning);
    match tuning.cube {
        true => lead,
        false => {
            let behind = mass(&pool[(index + slots - 1) % slots], tuning);
            let ahead = mass(&pool[(index + 1) % slots], tuning);
            lead + (behind + ahead) * 0.5
        }
    }
}

/// The byte a colour's channel rounds to.
fn channel(value: f64) -> u8 {
    value.round().clamp(0., 255.) as u8
}

/// The hue, saturation and lightness of a colour, as the accent reads them. The hue is in
/// degrees, which is the measure the edge's own hue is compared in. The arithmetic is
/// carried in double precision, so the hue a cover names does not shift with the rounding
/// of a single colour.
fn hsl(colour: Rgb) -> (f64, f64, f64) {
    let red = colour[0] as f64 / 255.;
    let green = colour[1] as f64 / 255.;
    let blue = colour[2] as f64 / 255.;
    let maximum = red.max(green.max(blue));
    let minimum = red.min(green.min(blue));
    let lightness = (maximum + minimum) / 2.;
    let delta = maximum - minimum;
    // A colour this close to grey carries no hue of its own: the saturation below would read
    // as fully saturated near white and near black, and the hue would name whichever channel
    // happens to be the largest.
    if delta <= ACHROMATIC {
        return (0., 0., lightness);
    }
    let saturation = delta / (1. - (2. * lightness - 1.).abs());
    let hue = match maximum == red {
        true => ((green - blue) / delta + 6.) % 6.,
        false => match maximum == green {
            true => (blue - red) / delta + 2.,
            false => (red - green) / delta + 4.,
        },
    };
    (hue * 60. % 360., saturation, lightness)
}

/// The colour of a hue, saturation and lightness.
fn rgb(hue: f64, saturation: f64, lightness: f64) -> Rgb {
    let chroma = (1. - (2. * lightness - 1.).abs()) * saturation;
    let segment = (hue + 360.) % 360. / 60.;
    let secondary = chroma * (1. - (segment % 2. - 1.).abs());
    let values = match segment {
        s if s < 1. => [chroma, secondary, 0.],
        s if s < 2. => [secondary, chroma, 0.],
        s if s < 3. => [0., chroma, secondary],
        s if s < 4. => [0., secondary, chroma],
        s if s < 5. => [secondary, 0., chroma],
        _ => [chroma, 0., secondary],
    };
    let offset = lightness - chroma / 2.;
    [
        channel((values[0] + offset) * 255.),
        channel((values[1] + offset) * 255.),
        channel((values[2] + offset) * 255.),
    ]
}

/// One colour mixed toward another.
fn mix(from: Rgb, to: Rgb, amount: f64) -> Rgb {
    let amount = amount.clamp(0., 1.);
    [
        channel(from[0] as f64 + (to[0] as f64 - from[0] as f64) * amount),
        channel(from[1] as f64 + (to[1] as f64 - from[1] as f64) * amount),
        channel(from[2] as f64 + (to[2] as f64 - from[2] as f64) * amount),
    ]
}

/// A colour darkened to the lightness an edge is read at. An edge brighter than the art
/// it frames says nothing about the art.
fn clamp_lightness(colour: Rgb) -> Rgb {
    let (hue, saturation, lightness) = hsl(colour);
    match lightness <= SEAM_LIGHTNESS {
        true => colour,
        false => rgb(hue, saturation, SEAM_LIGHTNESS),
    }
}

/// The value half the weight of a histogram sits below.
fn median(histogram: &[f64; 256], total: f64) -> u8 {
    let midpoint = total / 2.;
    let mut accumulated = 0.;
    for (value, weight) in histogram.iter().enumerate() {
        accumulated += weight;
        if accumulated >= midpoint {
            return value as u8;
        }
    }
    0
}

/// The colour of one zone of a cover's edge: the mean and the median of its pixels, both
/// darkened to the lightness an edge is read at, mixed toward the median.
fn zone_seam(
    pixels: &[u8],
    width: usize,
    left: usize,
    top: usize,
    zone_width: usize,
    zone_height: usize,
) -> Rgb {
    let mut histograms = [[0.0f64; 256]; 3];
    let mut sums = [0.0f64; 3];
    let mut total = 0.0f64;
    for y in top..top + zone_height {
        let row = match pixels.get(y * width * 4..) {
            Some(row) => row,
            None => break,
        };
        for x in left..left + zone_width {
            let offset = x * 4;
            let Some(pixel) = row.get(offset..offset + 4) else {
                break;
            };
            let (blue, green, red, alpha) = (pixel[0], pixel[1], pixel[2], pixel[3]);
            let alpha = alpha as f64 / 255.;
            if alpha < 0.04 {
                continue;
            }
            total += alpha;
            for (index, value) in [red, green, blue].into_iter().enumerate() {
                sums[index] += value as f64 * alpha;
                histograms[index][value as usize] += alpha;
            }
        }
    }
    if total == 0. {
        return SEAM_FALLBACK;
    }
    let average = clamp_lightness([
        channel(sums[0] / total),
        channel(sums[1] / total),
        channel(sums[2] / total),
    ]);
    let middle = clamp_lightness([
        median(&histograms[0], total),
        median(&histograms[1], total),
        median(&histograms[2], total),
    ]);
    clamp_lightness(mix(average, middle, SEAM_MEDIAN))
}

/// The colour of the edge a cover is bound by: the strips along its four edges and the
/// corners they meet, read apart and mixed, with the strips weighted above the corners
/// because a corner is the one place a cover's art is most likely to run out.
fn seam(pixels: &[u8], width: usize, height: usize) -> Rgb {
    let shortest = width.min(height);
    if shortest == 0 {
        return SEAM_FALLBACK;
    }
    // The strips are thin enough to catch the edge itself, and the corners wide enough
    // to survive the anti-aliasing of a picture that runs to the border.
    let strip = shortest.min(((shortest as f64 * 0.007).round() as usize).clamp(1, 7));
    let corner = shortest.min(((shortest as f64 * 0.14).round() as usize).clamp(8, 112));
    let zones = [
        (0, 0, strip, height, 1.0),
        (width - strip, 0, strip, height, 1.0),
        (0, 0, width, strip, 1.0),
        (0, height - strip, width, strip, 1.0),
        (0, 0, corner, corner, 0.5),
        (width - corner, 0, corner, corner, 0.5),
        (width - corner, height - corner, corner, corner, 0.5),
        (0, height - corner, corner, corner, 0.5),
    ];
    let mut sums = [0.0f64; 3];
    let mut weight = 0.0f64;
    for (left, top, zone_width, zone_height, share) in zones {
        let colour = zone_seam(pixels, width, left, top, zone_width, zone_height);
        for (index, value) in colour.into_iter().enumerate() {
            sums[index] += value as f64 * share;
        }
        weight += share;
    }
    clamp_lightness([
        channel(sums[0] / weight),
        channel(sums[1] / weight),
        channel(sums[2] / weight),
    ])
}

/// The hue of the most vivid colour in a cover, weighted so that a bright minority can
/// outrank a large muted field, and favoured when it sits near the edge's own hue.
fn supporting_hue(pixels: &[u8], width: usize, height: usize, fallback: f64) -> Option<f64> {
    let stride = (((width * height) as f64 / SUPPORT_PIXELS).sqrt().floor() as usize).max(1);
    let mut bins = [CubeBin::default(); 4096];
    for y in (0..height).step_by(stride) {
        let row = match pixels.get(y * width * 4..(y + 1) * width * 4) {
            Some(row) => row,
            None => break,
        };
        for pixel in row.as_chunks::<4>().0.iter().step_by(stride) {
            let (blue, green, red, alpha) = (pixel[0], pixel[1], pixel[2], pixel[3]);
            let alpha = alpha as f64 / 255.;
            if alpha < 0.12 {
                continue;
            }
            let key =
                ((red as usize >> 4) << 8) | ((green as usize >> 4) << 4) | (blue as usize >> 4);
            let bin = &mut bins[key];
            bin.weight += alpha;
            bin.sums[0] += red as f64 * alpha;
            bin.sums[1] += green as f64 * alpha;
            bin.sums[2] += blue as f64 * alpha;
        }
    }
    let mut best: Option<(f64, f64)> = None;
    for bin in bins {
        if bin.weight < SUPPORT_MIN_WEIGHT {
            continue;
        }
        let colour = [
            channel(bin.sums[0] / bin.weight),
            channel(bin.sums[1] / bin.weight),
            channel(bin.sums[2] / bin.weight),
        ];
        let (hue, saturation, lightness) = hsl(colour);
        if saturation < SUPPORT_SATURATION
            || lightness < SUPPORT_MIN_LIGHTNESS
            || lightness > SUPPORT_MAX_LIGHTNESS
        {
            continue;
        }
        let apart = (hue - fallback).abs();
        let distance = apart.min(360. - apart);
        let score = bin.weight.sqrt()
            * saturation
            * (0.24 + lightness)
            * if distance <= SUPPORT_NEAR {
                SUPPORT_NEAR_WEIGHT
            } else {
                SUPPORT_FAR_WEIGHT
            };
        if best.is_none_or(|(top, _)| score > top) {
            best = Some((score, hue));
        }
    }
    best.map(|(_, hue)| hue)
}

/// The accent a cover names: the hue of its edge when that edge is colourful enough to
/// carry one, and the hue of the most vivid colour in the picture otherwise. A cover with
/// no colour in it names none, and the theme falls back to its own.
fn accent(pixels: &[u8], width: usize, height: usize) -> Option<Hsla> {
    let (edge_hue, edge_saturation, edge_lightness) = hsl(seam(pixels, width, height));
    let supporting = supporting_hue(pixels, width, height, edge_hue);
    // A cover with no colour in it names no hue. It wears the theme's own colour rather
    // than a grey of its own, which reads the same and keeps a black sleeve and a white
    // one from washing a control out.
    if supporting.is_none() && edge_saturation < MONOCHROME_SATURATION {
        return None;
    }
    let hue = match edge_saturation >= SEAM_SATURATION {
        true => edge_hue,
        false => supporting.unwrap_or(edge_hue),
    };
    Some(Hsla {
        h: (hue / 360.) as f32,
        s: edge_saturation.clamp(ACCENT_SATURATION, 1.) as f32,
        l: match edge_lightness > ACCENT_BRIGHT {
            true => ACCENT_BRIGHT_LIGHTNESS,
            false => ACCENT_LIGHTNESS,
        } as f32,
        a: 1.,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(colours: &[([u8; 3], usize)]) -> Vec<u8> {
        colours
            .iter()
            .flat_map(|&([red, green, blue], count)| {
                std::iter::repeat_n([blue, green, red, 255], count).flatten()
            })
            .collect()
    }

    #[test]
    fn finds_the_dominant_hue() {
        let pixels = image(&[([204, 34, 34], 900), ([32, 32, 32], 100)]);
        let colour = sampled(&pixels, 32, 32)
            .primary
            .expect("a red image is colourful");

        assert!(colour.h < 0.02 || colour.h > 0.98, "hue was {}", colour.h);
        assert!(colour.s > 0.5, "saturation was {}", colour.s);
    }

    #[test]
    fn averages_across_the_bin_boundary() {
        let pixels = image(&[([255, 0, 60], 500), ([255, 60, 0], 500)]);
        let colour = sampled(&pixels, 32, 32)
            .primary
            .expect("a red image is colourful");

        assert!(colour.h < 0.02 || colour.h > 0.98, "hue was {}", colour.h);
    }

    #[test]
    fn ignores_greyscale_artwork() {
        let pixels = image(&[([18, 18, 18], 500), ([200, 200, 200], 500)]);

        assert!(sampled(&pixels, 32, 32).primary.is_none());
    }

    #[test]
    fn ignores_a_small_splash_of_colour() {
        let pixels = image(&[([120, 120, 120], 990), ([0, 180, 255], 10)]);

        assert!(sampled(&pixels, 32, 32).primary.is_none());
    }

    #[test]
    fn fragmented_hue_on_black_still_names_it() {
        let pixels = image(&[
            ([170, 110, 220], 200),
            ([140, 70, 200], 200),
            ([120, 20, 20], 100),
            ([8, 8, 8], 500),
        ]);
        let colour = sampled(&pixels, 32, 32)
            .primary
            .expect("smeared purple names purple");

        assert!(colour.h > 0.68 && colour.h < 0.9, "hue was {}", colour.h);
    }

    #[test]
    fn light_accent_wins_on_a_dark_cover() {
        let pixels = image(&[
            ([170, 110, 220], 350),
            ([120, 20, 20], 350),
            ([8, 8, 8], 300),
        ]);
        let colour = sampled(&pixels, 32, 32)
            .primary
            .expect("a dark cover names its light accent");

        assert!(colour.h > 0.7 && colour.h < 0.88, "hue was {}", colour.h);
    }

    #[test]
    fn names_a_runner_up() {
        let pixels = image(&[
            ([220, 40, 40], 600),
            ([40, 120, 220], 300),
            ([120, 120, 120], 100),
        ]);
        let palette = sampled(&pixels, 32, 32);

        assert!(palette.primary.is_some());
        let second = palette.secondary.expect("two families name two hues");
        let gap = (palette.primary.unwrap().h - second.h)
            .abs()
            .min(1. - (palette.primary.unwrap().h - second.h).abs());
        assert!(gap > 0.1, "hues were {gap}");
    }

    #[test]
    fn one_family_names_no_runner_up() {
        let pixels = image(&[([90, 30, 130], 800), ([10, 10, 12], 200)]);
        let palette = sampled(&pixels, 32, 32);

        assert!(palette.primary.is_some());
        assert!(palette.secondary.is_none());
    }
}
