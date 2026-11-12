use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    App, Global, Hsla, Pixels, Rgba, SharedString, Task, WindowAppearance,
    WindowBackgroundAppearance, px, rgb, rgba,
};
use i18n::t;
use serde::{Deserialize, Serialize};

use crate::metrics::{Metrics, Rounding, Text};
use crate::palette::CoverPalette;

pub const MIN_FONT: f32 = 10.;
pub const MAX_FONT: f32 = 24.;
pub const MAX_TRANSPARENCY: f32 = 1.;
pub const BACKDROP_TRANSPARENCY: f32 = 0.15;
pub const MIN_LYRICS_SCALE: f32 = 0.6;
pub const MAX_LYRICS_SCALE: f32 = 2.;

const FADE: Duration = Duration::from_millis(320);
const FRAME: Duration = Duration::from_millis(8);

const SURFACE_TINT: f32 = 0.5;
const BORDER_TINT: f32 = 0.4;
const TEXT_TINT: f32 = 0.12;
const MAX_WASH_SATURATION: f32 = 0.7;
const MIN_ACCENT_SATURATION: f32 = 0.6;
const MAX_ACCENT_SATURATION: f32 = 0.85;
/// What coloured body text has to clear against the surface behind it, which is the ratio WCAG
/// asks of small text. A cover's accent is held to it, so a hue the eye reads as bright (a lime
/// green) is pushed darker than one it reads as dim (a navy) rather than both sharing a lightness.
pub const TEXT_CONTRAST: f32 = 4.5;
/// What the accent itself is lifted to before anything is dimmed from it. The headroom is the
/// point: a dimmed row or a past verse steps back toward the surface from here and still lands
/// above `TEXT_CONTRAST`, so the hierarchy never has to be paid for with legibility.
pub const ACCENT_CONTRAST: f32 = 7.0;
/// What a shape, an icon or a large glyph has to clear, which is the ratio WCAG asks of those.
/// Also the floor a decorative dimmed mark steps back to.
pub const SHAPE_CONTRAST: f32 = 3.0;
/// How far a control's plate has to stand off the artwork it is drawn on to be seen as a shape at
/// all. Below this the plate is not weak, it is invisible, so the neutral one takes its place.
const PLATE_CONTRAST: f32 = 1.6;
/// Art lighter than this wears a dark control and art darker than it a light
/// one, which is the whole of what tells a white sleeve from a black one.
const NEUTRAL_PIVOT: f32 = 0.5;
const SYSTEM_FILLS: bool = cfg!(target_os = "windows");
/// Whether the platform can blur the desktop behind a see-through window. Linux and FreeBSD
/// compositors ignore the request, so the setting is not offered there at all.
pub const WINDOW_BLUR: bool = !cfg!(any(target_os = "linux", target_os = "freebsd"));

/// What a fill keeps of itself once the window is fully see-through. The page
/// background has no floor — a clear window is the point — but everything drawn
/// on top of it does, or a hover and a field vanish at the end of the slider. A
/// surface sits over the background and so reads denser than its own alpha; the
/// table head replaces the background rather than stacking on it, which is why
/// it holds the least and still lands in the same place.
const SURFACE_FLOOR: f32 = 0.2;
const HEADER_FLOOR: f32 = 0.15;

/// How much of a fill's alpha survives on a see-through window before the
/// slider takes its share. A surface reads denser than its own alpha because the
/// page is still painted underneath it, which leaves the floor almost inert in
/// the middle of the slider; a flat cut is the one lever that thins a fill
/// across the whole of it. The same backing is what makes the cut safe at the top
/// of the slider — a barely transparent page hides the step on its own. The head
/// is cut harder: it replaces the page rather than sitting on it, and takes the
/// page back only while it is pinned, so at rest it can be nearly glass.
const SURFACE_WEIGHT: f32 = 0.68;
const HEADER_WEIGHT: f32 = 0.5;

/// The window background a look asks the platform for. Blur needs something to show through,
/// so only a see-through window on a platform that can blur gets it, and only an opaque one
/// can take the platform's own fill.
pub fn backdrop(blur_window: bool, transparent: bool) -> WindowBackgroundAppearance {
    match WINDOW_BLUR && blur_window && transparent {
        true => WindowBackgroundAppearance::Blurred,
        false => match SYSTEM_FILLS && !transparent {
            true => WindowBackgroundAppearance::Opaque,
            false => WindowBackgroundAppearance::Transparent,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub kind: ThemeKind,
    pub rounding: Rounding,
    pub font: f32,
    pub transparent: bool,
    pub transparency: f32,
    /// Whether the frosted treatments are painted. See `ui::blurring`.
    pub blur: bool,
    /// Whether a see-through window asks the platform to blur the desktop behind it.
    pub blur_window: bool,
    /// Whether glass is drawn matte and heavy rather than thin and lens-like. See
    /// `ui::frosted`.
    pub frosted: bool,
    pub tint: Option<Hsla>,
    pub tint_secondary: Option<Hsla>,
}

/// The built-in neutral palettes. Every other theme is a theme file laid over `Dark`, and the
/// adaptive theme tints only these.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThemeKind {
    System,
    Dark,
    Light,
}

impl ThemeKind {
    pub const ALL: [Self; 3] = [Self::System, Self::Dark, Self::Light];

    pub fn id(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn label(self) -> SharedString {
        match self {
            Self::System => t!("theme-system"),
            Self::Dark => t!("theme-dark"),
            Self::Light => t!("theme-light"),
        }
    }

    /// The kind an id names. Any other id, such as a theme file's, names `Dark`, the palette
    /// a theme file is laid over.
    pub fn from_id(id: &str) -> Self {
        match id {
            "system" => Self::System,
            "light" => Self::Light,
            _ => Self::Dark,
        }
    }

    pub fn resolved(self, cx: &App) -> Self {
        match self {
            Self::System => assumed().unwrap_or_else(|| Self::reported(cx)),
            kind => kind,
        }
    }

    pub fn reported(cx: &App) -> Self {
        match cx.window_appearance() {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Self::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::Dark,
        }
    }

    // what system means meanwhile
    pub fn assume(kind: Self) {
        ASSUMED.store(
            match kind {
                Self::Light => 1,
                _ => 2,
            },
            Ordering::Relaxed,
        );
    }

    pub fn assumed() -> Option<Self> {
        assumed()
    }
}

static ASSUMED: AtomicU8 = AtomicU8::new(0);

fn assumed() -> Option<ThemeKind> {
    match ASSUMED.load(Ordering::Relaxed) {
        1 => Some(ThemeKind::Light),
        2 => Some(ThemeKind::Dark),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ThemeOverrides {
    pub background: Option<String>,
    pub foreground: Option<String>,
    pub border: Option<String>,
    pub muted: Option<String>,
    pub overlay: Option<String>,
    pub overlay_foreground: Option<String>,
    pub muted_foreground: Option<String>,
    pub secondary: Option<String>,
    pub secondary_hover: Option<String>,
    pub secondary_active: Option<String>,
    pub primary: Option<String>,
    pub primary_foreground: Option<String>,
    pub primary_hover: Option<String>,
    pub danger: Option<String>,
    pub danger_foreground: Option<String>,
    pub danger_hover: Option<String>,
    pub popover: Option<String>,
    pub popover_foreground: Option<String>,
    pub progress_bar: Option<String>,
    pub selection: Option<String>,
    pub sidebar: Option<String>,
    pub sidebar_accent: Option<String>,
    pub sidebar_border: Option<String>,
    pub title_bar_border: Option<String>,
    pub table_head: Option<String>,
    pub table_head_foreground: Option<String>,
    pub table_row_border: Option<String>,
    pub table_hover: Option<String>,
    pub table_active: Option<String>,
    pub table_active_border: Option<String>,
    pub radius: Option<f32>,
    pub font_size: Option<f32>,
}

impl ThemeOverrides {
    /// Applies valid values from `overlay` and keeps the remaining values.
    pub fn merged(mut self, overlay: &Self) -> Self {
        macro_rules! apply_color {
            ($($field:ident),+ $(,)?) => {
                $(
                    if overlay.$field.as_deref().is_some_and(|value| parse_color(value).is_some()) {
                        self.$field = overlay.$field.clone();
                    }
                )+
            };
        }

        apply_color!(
            background,
            foreground,
            border,
            muted,
            overlay,
            overlay_foreground,
            muted_foreground,
            secondary,
            secondary_hover,
            secondary_active,
            primary,
            primary_foreground,
            primary_hover,
            danger,
            danger_foreground,
            danger_hover,
            popover,
            popover_foreground,
            progress_bar,
            selection,
            sidebar,
            sidebar_accent,
            sidebar_border,
            title_bar_border,
            table_head,
            table_head_foreground,
            table_row_border,
            table_hover,
            table_active,
            table_active_border,
        );
        self.radius = overlay.radius.or(self.radius);
        self.font_size = overlay.font_size.or(self.font_size);
        self
    }

    /// Reports whether a field is one of the color tokens a theme file may set. Radius and font
    /// size are left out because the Appearance pickers own them.
    pub fn is_color(field: &str) -> bool {
        matches!(
            field,
            "background"
                | "foreground"
                | "border"
                | "muted"
                | "overlay"
                | "overlay_foreground"
                | "muted_foreground"
                | "secondary"
                | "secondary_hover"
                | "secondary_active"
                | "primary"
                | "primary_foreground"
                | "primary_hover"
                | "danger"
                | "danger_foreground"
                | "danger_hover"
                | "popover"
                | "popover_foreground"
                | "progress_bar"
                | "selection"
                | "sidebar"
                | "sidebar_accent"
                | "sidebar_border"
                | "title_bar_border"
                | "table_head"
                | "table_head_foreground"
                | "table_row_border"
                | "table_hover"
                | "table_active"
                | "table_active_border"
        )
    }

    /// Returns the first color field whose value is not hexadecimal RGB or RGBA.
    pub fn invalid_color(&self) -> Option<&'static str> {
        macro_rules! check {
            ($($field:ident),+ $(,)?) => {
                $(
                    if self.$field.as_deref().is_some_and(|value| parse_color(value).is_none()) {
                        return Some(stringify!($field));
                    }
                )+
            };
        }

        check!(
            background,
            foreground,
            border,
            muted,
            overlay,
            overlay_foreground,
            muted_foreground,
            secondary,
            secondary_hover,
            secondary_active,
            primary,
            primary_foreground,
            primary_hover,
            danger,
            danger_foreground,
            danger_hover,
            popover,
            popover_foreground,
            progress_bar,
            selection,
            sidebar,
            sidebar_accent,
            sidebar_border,
            title_bar_border,
            table_head,
            table_head_foreground,
            table_row_border,
            table_hover,
            table_active,
            table_active_border,
        );
        None
    }
}

/// What a control wears when it takes its colours from a cover rather than
/// from the theme: the fill, the fill it lifts to on hover, and the glyph on
/// top of both.
#[derive(Clone, Copy)]
pub struct CoverFill {
    pub background: Hsla,
    pub hover: Hsla,
    pub foreground: Hsla,
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub background: Hsla,
    pub foreground: Hsla,
    pub border: Hsla,
    pub muted: Hsla,
    pub overlay: Hsla,
    pub overlay_foreground: Hsla,
    pub muted_foreground: Hsla,
    pub secondary: Hsla,
    /// The hover and press fills. A hover shades the surface under it rather than replacing
    /// it, so a frosted menu or a table over artwork still reads through the row the pointer
    /// is on. Each colour is lifted to land where the old near-solid one did over the page it
    /// usually covers, so only what shows through a hover moved, not its contrast.
    pub secondary_hover: Hsla,
    pub secondary_active: Hsla,
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub primary_hover: Hsla,
    pub danger: Hsla,
    pub danger_foreground: Hsla,
    pub danger_hover: Hsla,
    pub popover: Hsla,
    pub popover_foreground: Hsla,
    pub progress_bar: Hsla,
    pub selection: Hsla,
    pub sidebar: Hsla,
    /// The fill under a hovered or selected sidebar entry, thinned the way `secondary_hover`
    /// is and for the same reason.
    pub sidebar_accent: Hsla,
    pub sidebar_border: Hsla,
    pub title_bar_border: Hsla,
    pub table_head: Hsla,
    pub table_head_foreground: Hsla,
    pub table_row_border: Hsla,
    pub table_hover: Hsla,
    pub table_active: Hsla,
    pub table_active_border: Hsla,
    pub radius: Pixels,
    pub font_size: Pixels,
    pub metrics: Metrics,
    pub transparent: bool,
    /// Whether the frosted treatments are painted. Read it through `ui::blurring` rather
    /// than reaching for the field, so every site asks the same question.
    pub blur: bool,
    /// Whether a see-through window asks the platform to blur the desktop behind it. Only
    /// `ui::backdrop` reads it.
    pub blur_window: bool,
    /// Whether glass is drawn frosted rather than liquid: a thick matte fill over a wide
    /// blur, with none of the thin lens at the rim. Read it through `ui::frosted`.
    pub frosted: bool,
    pub tint: Option<Hsla>,
    /// The cover's runner-up hue, carried beside the lead tint for two-colour
    /// surfaces like the fullscreen ambient background. The selection itself stays single.
    pub tint_secondary: Option<Hsla>,
}

impl Global for Theme {}

#[derive(Default)]
struct Transition {
    task: Option<Task<()>>,
}

impl Global for Transition {}

impl Theme {
    pub fn dark() -> Self {
        Self {
            background: rgb(0x0a0a0a).into(),
            foreground: rgb(0xfafafa).into(),
            border: rgba(0x50505066).into(),
            muted: rgb(0x262626).into(),
            overlay: rgba(0x0000008c).into(),
            overlay_foreground: rgb(0xfafafa).into(),
            muted_foreground: rgb(0x909090).into(),
            secondary: rgb(0x171717).into(),
            secondary_hover: rgba(0x36363666).into(),
            secondary_active: rgba(0x4d4d4d66).into(),
            primary: rgb(0xfafafa).into(),
            primary_foreground: rgb(0x171717).into(),
            primary_hover: rgba(0xe5e5e5b3).into(),
            danger: rgb(0x7f1d1d).into(),
            danger_foreground: rgb(0xfef2f2).into(),
            danger_hover: rgba(0x8b2020b3).into(),
            popover: rgb(0x141414).into(),
            popover_foreground: rgb(0xfafafa).into(),
            progress_bar: rgb(0xf5f5f5).into(),
            selection: rgb(0x1d4ed8).into(),
            sidebar: rgb(0x0a0a0a).into(),
            sidebar_accent: rgba(0x50505066).into(),
            sidebar_border: rgb(0x262626).into(),
            title_bar_border: rgb(0x262626).into(),
            table_head: rgba(0x171717cc).into(),
            table_head_foreground: rgb(0x8a8a8a).into(),
            table_row_border: rgba(0x262626b3).into(),
            table_hover: rgba(0x3b3b3b66).into(),
            table_active: rgba(0x1e40af33).into(),
            table_active_border: rgb(0x1d4ed8).into(),
            radius: px(6.),
            font_size: px(14.),
            metrics: Metrics::default(),
            transparent: false,
            blur: false,
            blur_window: false,
            frosted: false,
            tint: None,
            tint_secondary: None,
        }
    }

    pub fn light() -> Self {
        Self {
            background: rgb(0xfafafa).into(),
            foreground: rgb(0x171717).into(),
            border: rgba(0x9b9b9b66).into(),
            muted: rgb(0xe5e5e5).into(),
            overlay: rgba(0x0000008c).into(),
            overlay_foreground: rgb(0xfafafa).into(),
            muted_foreground: rgb(0x606060).into(),
            secondary: rgb(0xf5f5f5).into(),
            secondary_hover: rgba(0xd5d5d566).into(),
            secondary_active: rgba(0xb7b7b766).into(),
            primary: rgb(0x171717).into(),
            primary_foreground: rgb(0xfafafa).into(),
            primary_hover: rgba(0x262626b3).into(),
            danger: rgb(0xb91c1c).into(),
            danger_foreground: rgb(0xfef2f2).into(),
            danger_hover: rgba(0x991b1bb3).into(),
            popover: rgb(0xffffff).into(),
            popover_foreground: rgb(0x171717).into(),
            progress_bar: rgb(0x262626).into(),
            selection: rgb(0x2563eb).into(),
            sidebar: rgb(0xf5f5f5).into(),
            sidebar_accent: rgba(0xcdcdcd66).into(),
            sidebar_border: rgb(0xd4d4d4).into(),
            title_bar_border: rgb(0xd4d4d4).into(),
            table_head: rgba(0xf5f5f5e6).into(),
            table_head_foreground: rgb(0x666666).into(),
            table_row_border: rgba(0xd4d4d4b3).into(),
            table_hover: rgba(0xe8e8e866).into(),
            table_active: rgba(0x2563eb1f).into(),
            table_active_border: rgb(0x2563eb).into(),
            radius: px(6.),
            font_size: px(14.),
            metrics: Metrics::default(),
            transparent: false,
            blur: false,
            blur_window: false,
            frosted: false,
            tint: None,
            tint_secondary: None,
        }
    }

    pub fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::System | ThemeKind::Dark => Self::dark(),
            ThemeKind::Light => Self::light(),
        }
    }

    pub fn tinted(mut self, tint: Hsla) -> Self {
        let dark = self.background.l < 0.5;

        macro_rules! wash {
            ($strength:expr, $($field:ident),+ $(,)?) => {
                $(self.$field = wash(self.$field, tint, $strength);)+
            };
        }

        wash!(
            SURFACE_TINT,
            background,
            secondary,
            secondary_hover,
            secondary_active,
            muted,
            popover,
            sidebar,
            sidebar_accent,
            table_head,
            table_hover,
        );
        wash!(
            BORDER_TINT,
            border,
            sidebar_border,
            title_bar_border,
            table_row_border,
        );
        wash!(
            TEXT_TINT,
            foreground,
            popover_foreground,
            muted_foreground,
            table_head_foreground,
        );

        let accent = |lightness| Hsla {
            h: tint.h,
            s: tint.s.clamp(MIN_ACCENT_SATURATION, MAX_ACCENT_SATURATION),
            l: lightness,
            a: 1.,
        };

        // The lightness above is only a starting point: it says nothing about how bright the
        // colour reads, and green carries several times the luminance of blue at the same
        // lightness. Each accent is therefore nudged in lightness — hue and saturation kept —
        // until it clears the contrast its text needs against the surface it is drawn on.
        self.primary = legible(
            accent(if dark { 0.72 } else { 0.42 }),
            self.background,
            ACCENT_CONTRAST,
        );
        self.primary_hover = legible(
            Hsla {
                a: 0.7,
                ..accent(if dark { 0.82 } else { 0.34 })
            },
            self.background,
            ACCENT_CONTRAST,
        );
        self.primary_foreground = legible(
            Hsla {
                s: tint.s.min(0.25),
                l: if dark { 0.08 } else { 0.98 },
                ..self.primary
            },
            self.primary,
            TEXT_CONTRAST,
        );
        self.progress_bar = self.primary;
        self.selection = self.accent(tint);
        self.table_active = Hsla {
            a: 0.22,
            ..self.selection
        };
        self.table_active_border = self.primary;
        self
    }

    /// The primary fill for a hue the theme is not wearing, so an element can
    /// carry a colour of its own while the theme stays put. Saturation and
    /// lightness are pinned the way `tinted` pins them, then lifted until the
    /// fill separates from the theme's own surface.
    pub fn primary_of(&self, tint: Hsla) -> Hsla {
        let fill = Hsla {
            h: tint.h,
            s: tint.s.clamp(MIN_ACCENT_SATURATION, MAX_ACCENT_SATURATION),
            l: match self.background.l < 0.5 {
                true => 0.72,
                false => 0.42,
            },
            a: 1.,
        };

        legible(fill, self.background, SHAPE_CONTRAST)
    }

    /// The colours a control wears over `cover`, or the theme's own primary when
    /// the art has not been sampled yet. Art that names a hue keeps the accent
    /// weights, so every cover reads at the same strength; art that names none
    /// goes neutral against its own lightness, since a black sleeve and a white
    /// one both want a control the theme's colour would only hide.
    pub fn cover_fill(&self, cover: Option<CoverPalette>) -> CoverFill {
        let Some(cover) = cover else {
            return CoverFill {
                background: self.primary,
                hover: self.primary_hover,
                foreground: self.primary_foreground,
            };
        };
        let dark = self.background.l < 0.5;
        let neutral = || {
            let (background, hover, foreground) = match cover.lightness < NEUTRAL_PIVOT {
                true => (0.88, 0.97, 0.1),
                false => (0.16, 0.06, 0.94),
            };
            let grey = |l, a| Hsla { h: 0., s: 0., l, a };

            CoverFill {
                background: grey(background, 1.),
                hover: grey(hover, 0.7),
                foreground: grey(foreground, 1.),
            }
        };

        let Some(tint) = cover.primary else {
            return neutral();
        };

        let plate = self.primary_of(tint);
        // The control sits on the artwork itself, so a plate as bright as the art around it
        // melts in however strong its hue is. Art that leaves no room for the accent gets the
        // neutral plate, which is chosen for the art's own lightness.
        let art = Hsla {
            h: 0.,
            s: 0.,
            l: cover.lightness,
            a: 1.,
        };
        if contrast(plate, art) < PLATE_CONTRAST {
            return neutral();
        }

        CoverFill {
            background: plate,
            hover: self.primary_hover_of(tint),
            foreground: legible(
                Hsla {
                    s: tint.s.min(0.25),
                    l: match dark {
                        true => 0.08,
                        false => 0.98,
                    },
                    ..plate
                },
                plate,
                TEXT_CONTRAST,
            ),
        }
    }

    /// The fill `primary_of` lifts to on hover, for the same hue. Follows the
    /// theme's own polarity: lighter than the base on a dark theme, darker on a
    /// light one, and less transparent either way.
    pub fn primary_hover_of(&self, tint: Hsla) -> Hsla {
        legible(
            Hsla {
                a: 0.7,
                l: match self.background.l < 0.5 {
                    true => 0.82,
                    false => 0.34,
                },
                ..self.primary_of(tint)
            },
            self.background,
            SHAPE_CONTRAST,
        )
    }

    /// The accent this theme carries once `tint` has washed through it, which
    /// is what `tinted` stores in `selection`. Exposed so a caller can resolve
    /// a cover hue the theme's own fade has not arrived at yet.
    pub fn accent(&self, tint: Hsla) -> Hsla {
        legible(
            Hsla {
                h: tint.h,
                s: tint.s.clamp(MIN_ACCENT_SATURATION, MAX_ACCENT_SATURATION),
                l: match self.background.l < 0.5 {
                    true => 0.44,
                    false => 0.5,
                },
                a: 1.,
            },
            self.background,
            ACCENT_CONTRAST,
        )
    }

    pub fn mixed(&self, other: &Self, delta: f32) -> Self {
        let mut theme = *other;
        macro_rules! mix_color {
            ($($field:ident),+ $(,)?) => {
                $(theme.$field = mix(self.$field, other.$field, delta);)+
            };
        }

        mix_color!(
            background,
            foreground,
            border,
            muted,
            muted_foreground,
            secondary,
            secondary_hover,
            secondary_active,
            primary,
            primary_foreground,
            primary_hover,
            danger,
            danger_foreground,
            danger_hover,
            popover,
            popover_foreground,
            progress_bar,
            selection,
            sidebar,
            sidebar_accent,
            sidebar_border,
            title_bar_border,
            table_head,
            table_head_foreground,
            table_row_border,
            table_hover,
            table_active,
            table_active_border,
        );
        theme
    }

    pub fn with_overrides(mut self, overrides: &ThemeOverrides) -> Self {
        macro_rules! apply_color {
            ($field:ident) => {
                if let Some(value) = overrides.$field.as_deref().and_then(parse_color) {
                    self.$field = value;
                }
            };
        }

        apply_color!(background);
        apply_color!(foreground);
        apply_color!(border);
        apply_color!(muted);
        apply_color!(overlay);
        apply_color!(overlay_foreground);
        apply_color!(muted_foreground);
        apply_color!(secondary);
        apply_color!(secondary_hover);
        apply_color!(secondary_active);
        apply_color!(primary);
        apply_color!(primary_foreground);
        apply_color!(primary_hover);
        apply_color!(danger);
        apply_color!(danger_foreground);
        apply_color!(danger_hover);
        apply_color!(popover);
        apply_color!(popover_foreground);
        apply_color!(progress_bar);
        apply_color!(selection);
        apply_color!(sidebar);
        apply_color!(sidebar_accent);
        apply_color!(sidebar_border);
        apply_color!(title_bar_border);
        apply_color!(table_head);
        apply_color!(table_head_foreground);
        apply_color!(table_row_border);
        apply_color!(table_hover);
        apply_color!(table_active);
        apply_color!(table_active_border);

        if let Some(radius) = overrides.radius {
            self.radius = px(radius.clamp(0., 24.));
        }
        if let Some(font_size) = overrides.font_size {
            self.font_size = px(font_size.clamp(10., 24.));
        }
        self
    }

    /// Thins every fill the window draws, so a see-through window stays one
    /// surface instead of a sheet of glass with opaque slabs floating on it.
    ///
    /// `opacity` is what the window itself keeps. The page background follows it
    /// all the way down; anything painted *over* that background — a field, a
    /// card, a hover — keeps a floor, because those read as layers above the
    /// glass and have to stay legible once the glass is gone entirely. The
    /// table head is flattened first: it is a tint meant to sit on an opaque
    /// page, and veiling the two layers separately is what makes it read solid.
    ///
    /// `popover` is deliberately left out. A menu, a modal or a toast covers
    /// content rather than wallpaper, so thinning it only lets the page bleed
    /// through the thing that was raised to be read.
    fn see_through(&mut self, opacity: f32) {
        self.table_head = veil(
            self.background.blend(self.table_head),
            opacity,
            HEADER_FLOOR,
        )
        .opacity(HEADER_WEIGHT);

        self.background = veil(self.background, opacity, 0.);
        self.sidebar = veil(self.sidebar, opacity, 0.);

        for surface in [
            &mut self.sidebar_accent,
            &mut self.secondary,
            &mut self.secondary_hover,
            &mut self.secondary_active,
            &mut self.muted,
            &mut self.table_hover,
            &mut self.table_active,
        ] {
            *surface = veil(*surface, opacity, SURFACE_FLOOR).opacity(SURFACE_WEIGHT);
        }
    }

    pub fn for_look(look: Look, overrides: &ThemeOverrides) -> Self {
        let base = px(overrides
            .font_size
            .unwrap_or(look.font)
            .clamp(MIN_FONT, MAX_FONT));
        let mut theme = Self::for_kind(look.kind);

        if let Some(tint) = look.tint {
            theme = theme.tinted(tint);
        }
        theme.radius = look.rounding.radius();
        theme = theme.with_overrides(overrides);
        if look.transparent {
            theme.see_through(1. - look.transparency.clamp(0., MAX_TRANSPARENCY));
        }
        theme.font_size = base;
        theme.metrics = Metrics::new(base);
        theme.transparent = look.transparent;
        theme.blur = look.blur;
        theme.blur_window = look.blur_window;
        theme.frosted = look.frosted;
        theme.tint = look.tint;
        theme.tint_secondary = look.tint_secondary;
        theme
    }

    pub fn text(&self, step: Text) -> Pixels {
        px((self.font_size / px(1.) * step.ratio()).round())
    }

    pub fn init(look: Look, overrides: &ThemeOverrides, cx: &mut App) {
        cx.set_global(Self::for_look(resolve(look, cx), overrides));
    }

    pub fn set(look: Look, overrides: &ThemeOverrides, cx: &mut App) {
        cx.default_global::<Transition>().task = None;
        cx.set_global(Self::for_look(resolve(look, cx), overrides));
        cx.refresh_windows();
    }

    pub fn fade(look: Look, overrides: &ThemeOverrides, cx: &mut App) {
        let from = *cx.theme();
        let to = Self::for_look(resolve(look, cx), overrides);

        cx.default_global::<Transition>().task = None;
        cx.set_global(from.mixed(&to, 0.));
        cx.refresh_windows();

        let task = cx.spawn(async move |cx| {
            let start = Instant::now();
            loop {
                cx.background_executor().timer(FRAME).await;

                let delta = start.elapsed().as_secs_f32() / FADE.as_secs_f32();
                let step = match delta >= 1. {
                    true => to,
                    false => from.mixed(&to, ease_out(delta)),
                };
                cx.update(|cx| {
                    cx.set_global(step);
                    cx.refresh_windows();
                });

                if delta >= 1. {
                    break;
                }
            }
        });
        cx.default_global::<Transition>().task = Some(task);
    }
}

/// The contrast ratio between a colour and the surface behind it, from 1 (the same) to 21 (black
/// on white). A translucent colour is composited onto the surface first, since its alpha is part
/// of what decides whether it can be read at all.
pub fn contrast(ink: Hsla, behind: Hsla) -> f32 {
    let shown = luminance(over(ink, behind));
    let surface = luminance(behind);
    let (bright, dim) = match shown > surface {
        true => (shown, surface),
        false => (surface, shown),
    };

    (bright + 0.05) / (dim + 0.05)
}

/// `ink` moved in lightness — hue and saturation untouched — until it reads at `goal` against
/// `behind`. A colour that already clears the goal is returned unchanged, and one that does not
/// takes the nearest lightness that does, so an accent stays as close to the hue the artwork
/// named as the floor allows. Alpha is respected, so a translucent fill is judged as drawn.
pub fn legible(ink: Hsla, behind: Hsla, goal: f32) -> Hsla {
    if contrast(ink, behind) >= goal {
        return ink;
    }

    // Lightness is monotonic in luminance, so each direction can be searched for the lightness
    // closest to the one asked for that still clears the goal.
    let reach = |to: f32| {
        let end = Hsla { l: to, ..ink };
        if contrast(end, behind) < goal {
            return None;
        }

        let (mut clears, mut fails) = (to, ink.l);
        for _ in 0..10 {
            let middle = (clears + fails) / 2.;
            match contrast(Hsla { l: middle, ..ink }, behind) >= goal {
                true => clears = middle,
                false => fails = middle,
            }
        }
        Some(Hsla { l: clears, ..ink })
    };

    let (down, up) = (reach(0.), reach(1.));
    match (down, up) {
        (Some(down), Some(up)) => match (down.l - ink.l).abs() <= (up.l - ink.l).abs() {
            true => down,
            false => up,
        },
        (Some(down), None) => down,
        (None, Some(up)) => up,
        // Neither end of the surface clears the goal: keep the hue and go as far as it can.
        (None, None) => {
            let (black, white) = (Hsla { l: 0., ..ink }, Hsla { l: 1., ..ink });

            match contrast(white, behind) > contrast(black, behind) {
                true => white,
                false => black,
            }
        }
    }
}

/// `ink` stepped `amount` of the way toward `behind` — the way a past verse or an inactive row
/// recedes — but never dropping under `goal`. Hierarchy survives; legibility stops depending on
/// which hue the artwork happened to name.
pub fn faded(ink: Hsla, behind: Hsla, amount: f32, goal: f32) -> Hsla {
    let step = amount.clamp(0., 1.);
    let (ink_rgb, behind_rgb) = (channels(ink), channels(behind));
    let channel = |ink: f32, behind: f32| ink + (behind - ink) * step;
    let dimmed = Hsla {
        a: ink.a,
        ..Hsla::from(Rgba {
            r: channel(ink_rgb.0, behind_rgb.0),
            g: channel(ink_rgb.1, behind_rgb.1),
            b: channel(ink_rgb.2, behind_rgb.2),
            a: 1.,
        })
    };

    legible(dimmed, behind, goal)
}

fn resolve(look: Look, cx: &App) -> Look {
    Look {
        kind: look.kind.resolved(cx),
        ..look
    }
}

/// Thins one fill for a window that keeps `opacity` of itself, leaving `floor`
/// of the fill behind when the window keeps nothing. The fill's own alpha is a
/// factor, not a target, so a tint stays a tint.
fn veil(color: Hsla, opacity: f32, floor: f32) -> Hsla {
    Hsla {
        a: color.a * (opacity + (1. - opacity) * floor),
        ..color
    }
}

fn mix(from: Hsla, to: Hsla, delta: f32) -> Hsla {
    let (from, to) = (Rgba::from(from), Rgba::from(to));
    let channel = |from: f32, to: f32| from + (to - from) * delta;

    Hsla::from(Rgba {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
        a: channel(from.a, to.a),
    })
}

fn ease_out(delta: f32) -> f32 {
    1. - (1. - delta.clamp(0., 1.)).powi(3)
}

fn wash(base: Hsla, tint: Hsla, strength: f32) -> Hsla {
    Hsla {
        h: tint.h,
        s: (base.s + tint.s * strength).min(MAX_WASH_SATURATION),
        l: base.l,
        a: base.a,
    }
}

/// The colour `ink` shows when laid over `behind`, with its alpha taken out.
fn over(ink: Hsla, behind: Hsla) -> Hsla {
    let alpha = ink.a.clamp(0., 1.);
    let (ink, behind) = (channels(ink), channels(behind));
    let channel = |ink: f32, behind: f32| ink * alpha + behind * (1. - alpha);

    Hsla::from(Rgba {
        r: channel(ink.0, behind.0),
        g: channel(ink.1, behind.1),
        b: channel(ink.2, behind.2),
        a: 1.,
    })
}

/// The sRGB channels of a colour, before any alpha is applied.
fn channels(color: Hsla) -> (f32, f32, f32) {
    let chroma = (1. - (2. * color.l - 1.).abs()) * color.s;
    let turn = color.h * 6.;
    let second = chroma * (1. - (turn % 2. - 1.).abs());
    let (red, green, blue) = match turn as u32 % 6 {
        0 => (chroma, second, 0.),
        1 => (second, chroma, 0.),
        2 => (0., chroma, second),
        3 => (0., second, chroma),
        4 => (second, 0., chroma),
        _ => (chroma, 0., second),
    };
    let floor = color.l - chroma / 2.;

    (red + floor, green + floor, blue + floor)
}

/// How bright a colour actually reads. Green carries most of the eye's luminance and blue the
/// least, which is why two hues the same `Hsla::l` apart can sit nowhere near each other in
/// luminance — the whole reason a lightness cannot stand in for a contrast ratio.
fn luminance(color: Hsla) -> f32 {
    let (red, green, blue) = channels(color);
    let linear = |channel: f32| match channel <= 0.04045 {
        true => channel / 12.92,
        false => ((channel + 0.055) / 1.055).powf(2.4),
    };

    0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue)
}

fn parse_color(value: &str) -> Option<Hsla> {
    let value = value.trim().strip_prefix('#').unwrap_or(value.trim());
    let parsed = u32::from_str_radix(value, 16).ok()?;
    match value.len() {
        6 => Some(rgb(parsed).into()),
        8 => Some(rgba(parsed).into()),
        _ => None,
    }
}

pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TINT: Hsla = Hsla {
        h: 0.55,
        s: 0.7,
        l: 0.5,
        a: 1.,
    };

    #[test]
    fn tinting_keeps_surface_contrast() {
        let base = Theme::dark();
        let tinted = base.tinted(TINT);

        assert_eq!(tinted.background.l, base.background.l);
        assert_eq!(tinted.secondary.l, base.secondary.l);
        assert_eq!(tinted.background.h, TINT.h);
        assert!(tinted.background.s > base.background.s);
    }

    #[test]
    fn tinting_recolours_accents_per_polarity() {
        let dark = Theme::dark().tinted(TINT);
        let light = Theme::light().tinted(TINT);

        assert_eq!(dark.primary.h, TINT.h);
        assert_eq!(dark.progress_bar, dark.primary);
        assert!(dark.primary.l > dark.background.l);
        assert!(light.primary.l < light.background.l);
    }

    #[test]
    fn mixing_runs_from_one_palette_to_the_other() {
        let from = Theme::dark();
        let to = Theme::light();

        let start = from.mixed(&to, 0.);
        let middle = from.mixed(&to, 0.5);
        let end = from.mixed(&to, 1.);

        assert!((start.background.l - from.background.l).abs() < 0.01);
        assert!((end.background.l - to.background.l).abs() < 0.01);
        assert!(middle.background.l > from.background.l);
        assert!(middle.background.l < to.background.l);
    }

    #[test]
    fn mixing_adopts_the_target_metrics_immediately() {
        let from = Theme::dark();
        let to = Theme::light().tinted(TINT);

        let start = from.mixed(&to, 0.);

        assert_eq!(start.radius, to.radius);
        assert_eq!(start.font_size, to.font_size);
        assert_eq!(start.tint, to.tint);
    }

    #[test]
    fn overrides_win_over_the_tint() {
        let look = Look {
            kind: ThemeKind::Dark,
            rounding: Rounding::Subtle,
            font: 14.,
            transparent: false,
            transparency: 0.,
            blur: false,
            blur_window: false,
            frosted: false,
            tint: Some(TINT),
            tint_secondary: None,
        };
        let overrides = ThemeOverrides {
            background: Some("#101010".to_owned()),
            ..ThemeOverrides::default()
        };

        let theme = Theme::for_look(look, &overrides);

        assert_eq!(theme.background, rgb(0x101010).into());
        assert_eq!(theme.tint, Some(TINT));
    }

    #[test]
    fn every_cover_hue_stays_readable() {
        // The accent keeps the cover's hue, so the ratio it lands at is the hue's own business.
        // A sweep is what proves the lightness solve holds for all of them and not just the one
        // that happens to be playing.
        for step in 0..24 {
            let hue = step as f32 / 24.;
            let tint = Hsla {
                h: hue,
                s: 0.8,
                l: 0.5,
                a: 1.,
            };

            for theme in [Theme::dark().tinted(tint), Theme::light().tinted(tint)] {
                let surface = theme.background;
                assert!(
                    contrast(theme.primary, surface) >= ACCENT_CONTRAST,
                    "hue {hue} on {:?}",
                    theme.background
                );
                assert!(
                    contrast(theme.primary_foreground, theme.primary) >= TEXT_CONTRAST,
                    "glyph for hue {hue}"
                );
                assert!(contrast(theme.selection, surface) >= ACCENT_CONTRAST);
            }
        }
    }

    #[test]
    fn a_dimmed_line_keeps_its_floor() {
        let tint = Hsla {
            h: 0.3,
            s: 0.85,
            l: 0.5,
            a: 1.,
        };

        for theme in [Theme::dark().tinted(tint), Theme::light().tinted(tint)] {
            // A sidebar row is solved against the sidebar it sits in, not the page behind it:
            // the two surfaces carry the same hue but not the same lightness.
            let ink = legible(theme.primary, theme.sidebar, ACCENT_CONTRAST);
            let dimmed = faded(ink, theme.sidebar, 0.6, TEXT_CONTRAST);

            assert!(contrast(dimmed, theme.sidebar) >= TEXT_CONTRAST);
            assert!(contrast(ink, theme.sidebar) >= ACCENT_CONTRAST);
        }
    }

    #[test]
    fn a_bright_hue_is_pushed_darker_than_a_dim_one() {
        // Green reads far brighter than blue at the same lightness, so a light theme has to
        // darken it further to reach the same ratio.
        let green = Theme::light().tinted(Hsla {
            h: 0.3,
            s: 0.85,
            l: 0.5,
            a: 1.,
        });
        let navy = Theme::light().tinted(Hsla {
            h: 0.6,
            s: 0.85,
            l: 0.5,
            a: 1.,
        });

        assert!(green.primary.l < navy.primary.l);
    }
}
