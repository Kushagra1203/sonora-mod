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
            tuning.bias = number("SN_TINT_BIAS").unwrap_or(tuning.bias);
            return tuning;
        }
        #[cfg(not(debug_assertions))]
        self
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
        match tuning.cube {
            true => blocks[cube_bin(red, green, blue)].add(colour, factor),
            false => {
                let bin = ((colour.h * BINS as f32) as usize).min(BINS - 1);
                bins[bin].add(colour, factor);
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

    let pool: &[Bin] = match tuning.cube {
        true => &blocks,
        false => &bins,
    };
    let slots = pool.len();

    let peak =
        (0..slots).max_by(|&a, &b| score(pool, a, tuning).total_cmp(&score(pool, b, tuning)));
    let primary =
        peak.filter(|&peak| score(pool, peak, tuning) >= (sampled * MIN_SHARE).max(MIN_WEIGHT));
    let Some(peak) = primary else {
        return CoverPalette {
            lightness: mean,
            corner,
            ..Default::default()
        };
    };

    // A hue bin is a wedge, so its neighbours are the same colour family and
    // fold in with it. A cube block is already one colour and stands alone.
    let mut cluster = pool[peak];
    if !tuning.cube {
        cluster.merge(&pool[(peak + slots - 1) % slots]);
        cluster.merge(&pool[(peak + 1) % slots]);
    }
    let primary = cluster.colour();

    let runner = (0..slots)
        .filter(|&index| {
            let apart = match tuning.cube {
                true => index != peak,
                false => index.abs_diff(peak).min(slots - index.abs_diff(peak)) >= SECONDARY_GAP,
            };
            apart && score(pool, index, tuning) >= (sampled * SECONDARY_SHARE).max(SECONDARY_WEIGHT)
        })
        .max_by(|&a, &b| score(pool, a, tuning).total_cmp(&score(pool, b, tuning)));
    let secondary = runner
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
