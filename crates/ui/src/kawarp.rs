//! The immersive background: the playing cover bent like wet paint.
//!
//! This is the Kawarp effect, ported from Orchard's own shader. The cover is reduced to a
//! small square, its darks tinted, and blurred with five passes of a four-tap Kawase kernel;
//! two octaves of simplex noise then bend that square's coordinates, so the picture behind
//! the window bends and mixes the way a line of wet paint does.
//!
//! On the wgpu renderer the bend runs in a fragment shader: the prepared square is uploaded
//! once per cover and every frame is one draw call with an updated clock, so a moving field
//! costs no CPU work and no texture upload. Backends without that hook work the bend out on
//! the CPU at a fraction of the window's size and let the renderer scale it back up, which is
//! what the effect looks like anyway: nothing in it is sharper than the blur it is drawn from.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::prelude::*;
use gpui::{
    Context, ObjectFit, Render, RenderImage, SharedString, Task, WarpParams, Window, canvas, div,
    img,
};
use i18n::t;
use image::{Frame, RgbaImage};

use crate::artwork::{ArtworkCache, resource};
use crate::motion::animates;

/// Whether the renderer can bend an image in its own shader. The wgpu backend is the only one
/// with the hook; everywhere else the bend is worked out here.
const GPU: bool = cfg!(any(target_os = "linux", target_os = "freebsd"));

/// The edge of the square the artwork is reduced to before it is bent.
const SOURCE: usize = 128;
/// The colour the artwork's darks are tinted toward. It is not black, so the picture keeps
/// a floor of colour under the page rather than dropping into it.
const TINT: [f32; 3] = [0.024, 0.04, 0.028];
/// How much of the tint a fully black pixel takes.
const TINT_STRENGTH: f32 = 0.42;
/// How many times the four-tap Kawase kernel runs over the artwork. Each pass reaches
/// further than the last, so five of them blur the square until no shape survives.
const BLUR_PASSES: usize = 5;
/// How far the bend reaches, as a share of the square.
const INTENSITY: f32 = 0.92;
/// How much of the colour the bend leaves in.
const SATURATION: f32 = 1.24;
/// How bright the bend is.
const BRIGHTNESS: f32 = 1.0;
/// How much of the square the window shows. The square is pushed in so its edges fall
/// outside the window and the bend has room to work.
const ZOOM: f32 = 1.32;
/// How far from the middle the bend fades out.
const FALLOFF: f32 = 0.7;
/// How much the corners of the window are darkened.
const VIGNETTE: f32 = 0.3;
/// How long one cover takes to melt into the next.
const MELT: Duration = Duration::from_millis(1200);
/// The edge the cover is decoded at before it is reduced to the square.
const EDGE: u32 = 544;
/// The share of the window the bend's displacement is worked out at. The noise it reads is
/// smooth enough that the gaps between the points cannot be seen, and the whole field
/// costs a fraction of what a point per pixel would.
const FIELD: usize = 4;
/// The reciprocal of the field's share, for the pixels' own arithmetic.
const INV_FIELD: f32 = 1. / FIELD as f32;
/// How many frames stay in the atlas behind the one on show. A window that is crossfading
/// keeps the frame it held, so a frame has to outlive the one that replaced it.
const HELD: usize = 3;

/// How finely the immersive background is worked out. The field is smooth enough that a
/// coarse one looks much the same as a fine one, so the choice is about what it costs. On a
/// renderer that bends the image in its own shader even the finest field is cheap, so that is
/// the default there and here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BackgroundQuality {
    Low,
    Medium,
    #[default]
    High,
}

impl BackgroundQuality {
    pub const ALL: [Self; 3] = [Self::Low, Self::Medium, Self::High];

    pub fn id(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }

    pub fn from_id(id: &str) -> Self {
        match id {
            "low" => Self::Low,
            "high" => Self::High,
            _ => Self::default(),
        }
    }

    /// The localized name. Resolved at render, never stored.
    pub fn label(self) -> SharedString {
        match self {
            Self::Low => t!("settings-background-quality-low"),
            Self::Medium => t!("settings-background-quality-medium"),
            Self::High => t!("settings-background-quality-high"),
        }
    }

    /// The size the field is worked out at for a window `width` by `height`: a share of the
    /// window, never larger than the tier's cap. The cap shrinks both edges together, so
    /// the field keeps the window's shape.
    fn size(self, width: f32, height: f32) -> (u32, u32) {
        let (scale, cap) = match self {
            Self::Low => (0.25, (640., 360.)),
            Self::Medium => (0.4, (854., 480.)),
            Self::High => (0.6, (1280., 720.)),
        };
        let width = width * scale;
        let height = height * scale;
        // The cap is a ceiling on the field, so a window larger than the tier's cap has both
        // of its edges pulled in by the same share and keeps the window's shape.
        let shrink = (1. / (width / cap.0).max(height / cap.1)).min(1.);
        (
            (width * shrink).round().max(1.) as u32,
            (height * shrink).round().max(1.) as u32,
        )
    }

    /// How many frames a second the field is worked out at.
    fn rate(self) -> f32 {
        match self {
            Self::Low => 20.,
            Self::Medium | Self::High => 30.,
        }
    }
}

/// What a renderer that bends the image itself draws: the prepared square, the one the field
/// is melting out of, and the warp's parameters for this frame.
type Field = (
    Arc<RenderImage>,
    Option<(Arc<RenderImage>, usize)>,
    WarpParams,
);

/// The artwork the bend is worked out from: a small square of colour, already tinted and
/// blurred, which the bend samples as it goes.
pub struct Source {
    pixels: Vec<f32>,
}

impl Source {
    /// The artwork `image` reduced, tinted and blurred into the square the bend samples.
    pub fn of(image: &RenderImage) -> Option<Self> {
        let bytes = image.as_bytes(0)?;
        let size = image.size(0);
        let (width, height) = (size.width.0 as usize, size.height.0 as usize);
        if width == 0 || height == 0 {
            return None;
        }
        // The frames are held in blue, green, red order.
        let mut raw = Vec::with_capacity(width * height * 3);
        for pixel in bytes.as_chunks::<4>().0 {
            raw.push(pixel[2] as f32 / 255.);
            raw.push(pixel[1] as f32 / 255.);
            raw.push(pixel[0] as f32 / 255.);
        }
        let mut pixels = downscale(&raw, width, height);
        tint(&mut pixels);
        blur(&mut pixels);
        Some(Self { pixels })
    }

    /// The colour at a point of the square, taken between the four pixels around it.
    fn sample(&self, x: f32, y: f32) -> [f32; 3] {
        sample(&self.pixels, x, y)
    }

    /// The prepared square as an image a renderer can hold in its atlas. It is built once per
    /// cover; a field that moves only changes the warp's own parameters, so this is never
    /// uploaded again.
    fn image(&self) -> RenderImage {
        let mut bytes = Vec::with_capacity(SOURCE * SOURCE * 4);
        let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u8;
        for pixel in self.pixels.as_chunks::<3>().0 {
            // The atlas holds its frames in blue, green, red order.
            bytes.extend_from_slice(&[
                channel(pixel[2]),
                channel(pixel[1]),
                channel(pixel[0]),
                255,
            ]);
        }
        let image = RgbaImage::from_raw(SOURCE as u32, SOURCE as u32, bytes)
            .expect("the square is SOURCE by SOURCE");
        RenderImage::new(vec![Frame::new(image)])
    }
}

/// The immersive background: the playing cover bent like wet paint behind the window.
///
/// The field follows one cover and melts into the next. A renderer that can bend an image in
/// its own shader is handed the prepared square and the clock, so a frame costs one draw call;
/// everywhere else the field is worked out at a fraction of the window's size and scaled back
/// up by the renderer.
pub struct Immersive {
    /// The cover the field follows.
    cover: Option<SharedString>,
    /// The cover the artwork on show was made from.
    shown: Option<SharedString>,
    /// The cover being reduced right now.
    preparing: Option<SharedString>,
    /// The artwork on show.
    current: Option<Arc<Source>>,
    /// The artwork the field is melting out of.
    previous: Option<Arc<Source>>,
    /// When the melt began.
    melted: Option<Instant>,
    /// The prepared square as an image the shader can sample. Uploaded once per cover and
    /// sampled unchanged for as long as it is on show.
    image: Option<Arc<RenderImage>>,
    /// The image the field is melting out of.
    previous_image: Option<Arc<RenderImage>>,
    /// The frame on show, on a renderer that works the bend out here.
    frame: Option<Arc<RenderImage>>,
    /// The frames still on show somewhere: a window that is crossfading keeps the frame it
    /// held, so a frame stays in the atlas until the one after it has replaced it.
    frames: VecDeque<Arc<RenderImage>>,
    /// The frame being worked out.
    frame_task: Option<Task<()>>,
    /// The artwork being reduced.
    source_task: Option<Task<()>>,
    /// The clock the bend reads.
    clock: f32,
    /// When the last frame was asked for.
    stepped: Instant,
    /// How long the last frame took to work out.
    rendered: Duration,
    /// The size the last frame was worked out at.
    size: (u32, u32),
    /// The quality the field is worked out at.
    quality: BackgroundQuality,
}

impl Immersive {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            cover: None,
            shown: None,
            preparing: None,
            current: None,
            previous: None,
            melted: None,
            image: None,
            previous_image: None,
            frame: None,
            frames: VecDeque::new(),
            frame_task: None,
            source_task: None,
            clock: 0.,
            stepped: Instant::now(),
            rendered: Duration::ZERO,
            size: (0, 0),
            quality: BackgroundQuality::default(),
        }
    }

    /// The field follows `cover`, melting into it when it differs from the last one, and is
    /// worked out at `quality`.
    pub fn follow(&mut self, cover: Option<SharedString>, quality: BackgroundQuality) {
        self.quality = quality;
        self.cover = cover;
    }

    /// Whether the field has something to show. A field that has not prepared one yet leaves
    /// the page's own artwork in place rather than showing nothing.
    pub fn ready(&self) -> bool {
        match GPU {
            true => self.image.is_some(),
            false => self.frame.is_some(),
        }
    }

    /// How far the current cover has melted over the previous one, from 0 to 1.
    fn melt(&self) -> f32 {
        self.melted
            .map(|since| since.elapsed().as_secs_f32() / MELT.as_secs_f32())
            .unwrap_or(1.)
            .min(1.)
    }

    /// The size the bend is worked out at, in the target's own pixels.
    ///
    /// A renderer that bends the image itself can afford the finest field, so it gets the
    /// tier that was asked for. One that works the bend out here cannot: the finest field it
    /// ever worked out was the middle one, so that is where it stops.
    fn field_size(&self, window: &Window) -> (u32, u32) {
        let viewport = window.viewport_size();
        let scale = window.scale_factor();
        let quality = match (GPU, self.quality) {
            (false, BackgroundQuality::High) => BackgroundQuality::Medium,
            (_, quality) => quality,
        };
        quality.size(
            viewport.width.as_f32() * scale,
            viewport.height.as_f32() * scale,
        )
    }

    /// Loads the cover the field follows and reduces it to the square the bend is worked
    /// out from. The reduction runs on its own thread: it reads every pixel of the cover
    /// and blurs it five times, which is too much to do while the window waits for it.
    fn prepare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(cover) = self.cover.clone() else {
            return;
        };
        if self.shown.as_ref() == Some(&cover) || self.preparing.as_ref() == Some(&cover) {
            return;
        }
        let cache = ArtworkCache::entity(cx);
        let resource = resource(cover.clone());
        let Some(loaded) = cache.update(cx, |cache, cx| {
            cache.load_at(&resource, EDGE, false, window, cx)
        }) else {
            // The cover is still on its way. The window is told when it lands.
            return;
        };
        let Ok(image) = loaded else {
            return;
        };
        self.preparing = Some(cover.clone());
        self.source_task = Some(cx.spawn(async move |this, cx| {
            let prepared = cx
                .background_spawn(async move {
                    Source::of(&image).map(|source| {
                        let prepared = Arc::new(source.image());
                        (source, prepared)
                    })
                })
                .await;
            this.update(cx, |this, cx| {
                this.source_task = None;
                this.preparing = None;
                let Some((source, prepared)) = prepared else {
                    return;
                };
                // The cover on show melts into the new one. The image it was drawn from is
                // left in the atlas a few frames longer, since the frame on show still
                // refers to it.
                this.previous = this.current.take();
                if let Some(old) = this.previous_image.take() {
                    this.frames.push_back(old);
                }
                this.previous_image = this.image.take();
                this.current = Some(Arc::new(source));
                this.image = Some(prepared);
                this.shown = Some(cover);
                this.melted = Some(Instant::now());
                cx.notify();
            })
            .ok();
        }));
    }

    /// Asks for the next frame, unless one is already on its way. A renderer that bends the
    /// image itself only needs its clock moved on, so the field still ticks at the quality's
    /// rate rather than repainting the whole window every frame.
    fn step(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.frame_task.is_some() || !animates(cx) {
            return;
        }
        if self.current.is_none() {
            return;
        }
        // The cover the field is melting out of is let go once the melt is over, so the
        // steady state samples only the cover on show.
        let melt = self.melt();
        let previous = match melt < 1. {
            true => self.previous.clone(),
            false => {
                self.previous = None;
                match self.previous_image.take() {
                    Some(old) => {
                        self.frames.push_back(old);
                        None
                    }
                    None => None,
                }
            }
        };
        let now = Instant::now();
        let step = now.duration_since(self.stepped).as_secs_f32().min(0.1);
        self.stepped = now;
        let time = self.clock + step;
        self.clock = time;
        let wait = Duration::from_secs_f32(1. / self.quality.rate());
        match GPU {
            true => {
                self.frame_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    this.update(cx, |this, cx| {
                        this.frame_task = None;
                        cx.notify();
                    })
                    .ok();
                }));
            }
            false => {
                let Some(current) = self.current.clone() else {
                    return;
                };
                self.size = self.field_size(window);
                let wait = wait.saturating_sub(self.rendered);
                let (width, height) = self.size;
                let blend = eased(melt);
                self.frame_task = Some(cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(wait).await;
                    let started = Instant::now();
                    let bytes = cx
                        .background_spawn(async move {
                            draw(previous.as_deref(), &current, blend, time, width, height)
                        })
                        .await;
                    let took = started.elapsed();
                    this.update(cx, |this, cx| {
                        this.frame_task = None;
                        this.rendered = took;
                        if let Some(bytes) = bytes {
                            this.push(bytes, (width, height));
                        }
                        cx.notify();
                    })
                    .ok();
                }));
            }
        }
    }

    /// Puts a finished frame on show. The frame before it stays in the atlas a little
    /// longer, since a window that is crossfading keeps the frame it held.
    fn push(&mut self, bytes: Vec<u8>, size: (u32, u32)) {
        let (width, height) = size;
        let Some(image) = RgbaImage::from_raw(width, height, bytes) else {
            return;
        };
        let image = Arc::new(RenderImage::new(vec![Frame::new(image)]));
        self.frames.push_back(image.clone());
        self.frame = Some(image);
    }

    /// What a renderer that bends the image itself draws: the prepared square, the one the
    /// field is melting out of, and the warp's parameters for this frame. The square is in
    /// the atlas already, so only the parameters change from frame to frame.
    fn field(&self, window: &Window) -> Option<Field> {
        let image = self.image.clone()?;
        let melt = self.melt();
        let previous = match melt < 1. {
            true => self.previous_image.clone().map(|previous| (previous, 0)),
            false => None,
        };
        let (width, height) = self.field_size(window);
        Some((
            image,
            previous,
            WarpParams {
                time: self.clock,
                intensity: INTENSITY,
                saturation: SATURATION,
                brightness: BRIGHTNESS,
                zoom: ZOOM,
                vignette: VIGNETTE,
                falloff: FALLOFF,
                blend: eased(melt),
                field: [width as f32, height as f32],
                pad: [0., 0.],
            },
        ))
    }
}

impl Render for Immersive {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.prepare(window, cx);
        self.step(window, cx);
        if GPU && self.image.is_none() {
            log::debug!(
                "kawarp: no image yet, cover {:?} source {} preparing {:?}",
                self.cover,
                self.current.is_some(),
                self.preparing
            );
        }
        // The frames behind the one on show leave the atlas here, where the window they
        // were drawn in is to hand.
        while self.frames.len() > HELD {
            let Some(old) = self.frames.pop_front() else {
                break;
            };
            cx.drop_image(old, Some(window));
        }

        let field = match GPU {
            true => self.field(window).map(|(image, previous, warp)| {
                canvas(
                    move |bounds, _, _| (bounds, image, previous, warp),
                    |_, (bounds, image, previous, warp), window, _| {
                        window
                            .paint_warped_image(bounds, image, 0, previous, warp)
                            .ok();
                    },
                )
                .size_full()
                .into_any_element()
            }),
            false => self.frame.clone().map(|frame| {
                img(frame)
                    .size_full()
                    .object_fit(ObjectFit::Fill)
                    .into_any_element()
            }),
        };

        div()
            .id("immersive")
            .absolute()
            .inset_0()
            .overflow_hidden()
            .when_some(field, |this, field| this.child(field))
    }
}

/// One frame of the bend, at `width` by `height`, drawn over `current` and melting out of
/// `previous` as `blend` rises.
fn draw(
    previous: Option<&Source>,
    current: &Source,
    blend: f32,
    time: f32,
    width: u32,
    height: u32,
) -> Option<Vec<u8>> {
    let width = width.max(1) as usize;
    let height = height.max(1) as usize;
    let bend = Bend::of(time, width, height);
    let edge = (SOURCE - 1) as f32;
    let mut out = vec![0u8; width * height * 4];
    // The square's own coordinates run across the window in fixed steps, so the division
    // each pixel would otherwise need is worked out once here.
    let uinv = 1. / (width as f32 * ZOOM);
    let u0 = (0.5 - width as f32 * 0.5) * uinv + 0.5;
    let vinv = 1. / (height as f32 * ZOOM);
    let v0 = (0.5 - height as f32 * 0.5) * vinv + 0.5;
    // The corners are darkened by how far they are from the middle of the window, which
    // runs in fixed steps too.
    let cinv = 1. / width as f32;
    let c0 = (0.5 / width as f32) - 0.5;
    let cinv_y = 1. / height as f32;
    let c0_y = (0.5 / height as f32) - 0.5;
    for y in 0..height {
        let v = (v0 + y as f32 * vinv).clamp(0., 1.);
        let cy = c0_y + y as f32 * cinv_y;
        let cyy = cy * cy;
        for x in 0..width {
            let u = (u0 + x as f32 * uinv).clamp(0., 1.);
            let (dx, dy) = bend.at(x, y);
            let warped_u = (u + dx * INTENSITY).clamp(0., 1.) * edge;
            let warped_v = (v + dy * INTENSITY).clamp(0., 1.) * edge;
            let mut color = current.sample(warped_u, warped_v);
            if let Some(previous) = previous {
                let old = previous.sample(warped_u, warped_v);
                for channel in 0..3 {
                    color[channel] = old[channel] + (color[channel] - old[channel]) * blend;
                }
            }
            let cx = c0 + x as f32 * cinv;
            let shade = 1. - (cx * cx + cyy) * VIGNETTE;
            let gray = (color[0] * 0.299 + color[1] * 0.587 + color[2] * 0.114) * shade;
            let pixel = (y * width + x) * 4;
            for channel in 0..3 {
                let saturated = gray + (color[channel] * shade - gray) * SATURATION;
                let value = (saturated * BRIGHTNESS).clamp(0., 1.) * 255.;
                out[pixel + 2 - channel] = value as u8;
            }
            out[pixel + 3] = 255;
        }
    }
    Some(out)
}

/// The bend at every point of the window, worked out at a fraction of its size.
struct Bend {
    points: Vec<f32>,
    wide: usize,
    high: usize,
}

impl Bend {
    /// The bend `time` seconds into the field, over a window `width` by `height`.
    fn of(time: f32, width: usize, height: usize) -> Self {
        let wide = width.div_ceil(FIELD) + 1;
        let high = height.div_ceil(FIELD) + 1;
        let mut points = vec![0f32; wide * high * 2];
        let t = time * 0.05;
        for y in 0..high {
            for x in 0..wide {
                let (u, v) = coordinates(x * FIELD, y * FIELD, width, height);
                // The large octave moves slowly in broad blobs; the small one adds the
                // detail that keeps the picture from reading as a slide.
                let n1 = noise(u * 0.35 + t, v * 0.35 + t * 0.7);
                let n2 = noise(u * 0.35 - t * 0.8 + 50., v * 0.35 + t * 0.5 + 50.);
                let n3 = noise(u * 0.9 + t * 1.2 + 100., v * 0.9 - t);
                let n4 = noise(u * 0.9 - t, v * 0.9 + t * 1.1 + 100.);
                // The bend reaches furthest from the middle of the window and fades out
                // before the edges, where it would otherwise pull in the square's border.
                let dx = u - 0.5;
                let dy = v - 0.5;
                let weight = 1. - smoothstep(0., FALLOFF, (dx * dx + dy * dy).sqrt());
                let point = (y * wide + x) * 2;
                points[point] = (n1 * 0.65 + n3 * 0.35) * weight;
                points[point + 1] = (n2 * 0.65 + n4 * 0.35) * weight;
            }
        }
        Self { points, wide, high }
    }

    /// The bend at a pixel of the window, read between the four points around it.
    fn at(&self, x: usize, y: usize) -> (f32, f32) {
        let fx = x as f32 * INV_FIELD;
        let fy = y as f32 * INV_FIELD;
        let ix = fx.floor() as usize;
        let iy = fy.floor() as usize;
        let tx = fx - ix as f32;
        let ty = fy - iy as f32;
        let nx = (ix + 1).min(self.wide - 1);
        let ny = (iy + 1).min(self.high - 1);
        let gx = 1. - tx;
        let gy = 1. - ty;
        let a = (iy * self.wide + ix) * 2;
        let b = (iy * self.wide + nx) * 2;
        let c = (ny * self.wide + ix) * 2;
        let d = (ny * self.wide + nx) * 2;
        let w00 = gx * gy;
        let w10 = tx * gy;
        let w01 = gx * ty;
        let w11 = tx * ty;
        (
            self.points[a] * w00
                + self.points[b] * w10
                + self.points[c] * w01
                + self.points[d] * w11,
            self.points[a + 1] * w00
                + self.points[b + 1] * w10
                + self.points[c + 1] * w01
                + self.points[d + 1] * w11,
        )
    }
}

/// The point of the square a pixel of the window reads from, pushed in so the square's
/// edges fall outside the window and the bend has room to work.
fn coordinates(x: usize, y: usize, width: usize, height: usize) -> (f32, f32) {
    let u = ((x as f32 + 0.5) / width as f32 - 0.5) / ZOOM + 0.5;
    let v = ((y as f32 + 0.5) / height as f32 - 0.5) / ZOOM + 0.5;
    (u.clamp(0., 1.), v.clamp(0., 1.))
}

/// The artwork reduced to the square, each pixel the average of the ones it covers.
fn downscale(source: &[f32], width: usize, height: usize) -> Vec<f32> {
    let mut out = vec![0f32; SOURCE * SOURCE * 3];
    for y in 0..SOURCE {
        let y0 = y * height / SOURCE;
        let y1 = (((y + 1) * height) / SOURCE).max(y0 + 1).min(height);
        for x in 0..SOURCE {
            let x0 = x * width / SOURCE;
            let x1 = (((x + 1) * width) / SOURCE).max(x0 + 1).min(width);
            let mut sum = [0f32; 3];
            let mut count = 0.;
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let pixel = (sy * width + sx) * 3;
                    sum[0] += source[pixel];
                    sum[1] += source[pixel + 1];
                    sum[2] += source[pixel + 2];
                    count += 1.;
                }
            }
            let pixel = (y * SOURCE + x) * 3;
            out[pixel] = sum[0] / count;
            out[pixel + 1] = sum[1] / count;
            out[pixel + 2] = sum[2] / count;
        }
    }
    out
}

/// The artwork's darks tinted toward a near-black green, so the bend has a floor of colour
/// to work from and the picture does not go to pure black under the page.
fn tint(pixels: &mut [f32]) {
    for pixel in pixels.as_chunks_mut::<3>().0 {
        let luma = pixel[0] * 0.299 + pixel[1] * 0.587 + pixel[2] * 0.114;
        let t = (luma / 0.5).clamp(0., 1.);
        let mask = (1. - t * t * (3. - 2. * t)) * TINT_STRENGTH;
        for channel in 0..3 {
            pixel[channel] = pixel[channel] * (1. - mask) + TINT[channel] * mask;
        }
    }
}

/// Five passes of the four-tap Kawase kernel, each one reaching further than the last.
fn blur(pixels: &mut Vec<f32>) {
    let mut scratch = vec![0f32; pixels.len()];
    for pass in 0..BLUR_PASSES {
        let offset = pass as f32 + 0.5;
        for y in 0..SOURCE {
            for x in 0..SOURCE {
                let mut sum = [0f32; 3];
                for (dx, dy) in [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)] {
                    let tap = sample(pixels, x as f32 + dx * offset, y as f32 + dy * offset);
                    sum[0] += tap[0];
                    sum[1] += tap[1];
                    sum[2] += tap[2];
                }
                let pixel = (y * SOURCE + x) * 3;
                scratch[pixel] = sum[0] * 0.25;
                scratch[pixel + 1] = sum[1] * 0.25;
                scratch[pixel + 2] = sum[2] * 0.25;
            }
        }
        std::mem::swap(pixels, &mut scratch);
    }
}

/// The colour at a point of the square, taken between the four pixels around it.
fn sample(pixels: &[f32], x: f32, y: f32) -> [f32; 3] {
    let x = x.clamp(0., (SOURCE - 1) as f32);
    let y = y.clamp(0., (SOURCE - 1) as f32);
    let ix = x.floor() as usize;
    let iy = y.floor() as usize;
    let nx = (ix + 1).min(SOURCE - 1);
    let ny = (iy + 1).min(SOURCE - 1);
    let fx = x - ix as f32;
    let fy = y - iy as f32;
    let gx = 1. - fx;
    let gy = 1. - fy;
    let top_left = (iy * SOURCE + ix) * 3;
    let top_right = (iy * SOURCE + nx) * 3;
    let bottom_left = (ny * SOURCE + ix) * 3;
    let bottom_right = (ny * SOURCE + nx) * 3;
    let mut color = [0f32; 3];
    for channel in 0..3 {
        let top = pixels[top_left + channel] * gx + pixels[top_right + channel] * fx;
        let bottom = pixels[bottom_left + channel] * gx + pixels[bottom_right + channel] * fx;
        color[channel] = top * gy + bottom * fy;
    }
    color
}

/// The noise the bend is made of. Two octaves of it, moving at their own rates, is what
/// makes the picture bend and mix rather than simply slide.
fn noise(x: f32, y: f32) -> f32 {
    const C: [f32; 4] = [0.211_324_87, 0.366_025_42, -0.577_350_26, 0.024_390_243];
    let offset = (x + y) * C[1];
    let ix = (x + offset).floor();
    let iy = (y + offset).floor();
    let skew = (ix + iy) * C[0];
    let x0 = x - ix + skew;
    let y0 = y - iy + skew;
    let (i1x, i1y) = match x0 > y0 {
        true => (1., 0.),
        false => (0., 1.),
    };
    let corners = [
        (x0, y0),
        (x0 + C[0] - i1x, y0 + C[0] - i1y),
        (x0 + C[2], y0 + C[2]),
    ];
    // The lattice is folded into a small range before it is permuted, which keeps the
    // permutation from losing precision on the larger coordinates.
    let mi = mod289(ix);
    let mj = mod289(iy);
    let first = [0., i1y, 1.];
    let second = [0., i1x, 1.];
    let mut total = 0.;
    for (index, (cx, cy)) in corners.iter().enumerate() {
        let mut falloff = (0.5 - (cx * cx + cy * cy)).max(0.);
        falloff *= falloff;
        falloff *= falloff;
        let p = permute(permute(mj + first[index]) + mi + second[index]);
        let gradient = 2. * fract(p * C[3]) - 1.;
        let h = gradient.abs() - 0.5;
        let a = gradient - (gradient + 0.5).floor();
        falloff *= 1.792_842_9 - 0.853_734_73 * (a * a + h * h);
        total += falloff * (a * cx + h * cy);
    }
    130. * total
}

/// The lattice coordinate folded into the range the permutation works in.
fn mod289(x: f32) -> f32 {
    x - (x * (1. / 289.)).floor() * 289.
}

/// The permutation the noise's gradients are read through.
fn permute(x: f32) -> f32 {
    mod289((x * 34. + 1.) * x)
}

/// The part of a number past its whole.
fn fract(x: f32) -> f32 {
    x - x.floor()
}

/// The ramp between two edges, eased at both.
fn smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// How far the melt has come, eased so the two covers meet in the middle rather than
/// sliding past each other.
fn eased(blend: f32) -> f32 {
    0.5 - 0.5 * (blend * std::f32::consts::PI).cos()
}
