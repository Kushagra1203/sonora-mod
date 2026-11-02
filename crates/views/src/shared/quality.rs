//! What the stream playing now is encoded as, in the small badge the player shows.
//!
//! The engine reports the encoding it opened, so this reads the same signal the equalizer's
//! path view does rather than guessing from the quality setting the listener chose. Lossless
//! means FLAC, ALAC or PCM at CD depth and rate; hi-res means one of them above 16 bits or
//! above 48 kHz; everything else is a lossy stream and is named by its codec and bit rate.

use gpui::prelude::*;
use gpui::{AnyElement, App, Div, ElementId, Hsla, SharedString, div, px, svg};
use i18n::t;
use music::Encoding;
use state::Playback;
use ui::Text;

/// How good the stream is, which the badge colours by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tier {
    HiRes,
    Lossless,
    Lossy,
}

/// One stream's quality, with the short label and the figures that explain it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Quality {
    pub tier: Tier,
    pub label: SharedString,
    pub detail: SharedString,
}

impl Quality {
    /// The whole thing for a screen reader, since a badge is otherwise two fragments.
    pub fn spoken(&self) -> SharedString {
        SharedString::from(format!("{} · {}", self.label, self.detail))
    }
}

/// What is playing through the local engine, or `None` while nothing is loaded, the engine
/// has not opened the track yet, or another device is the one playing.
pub(crate) fn of(playback: &Playback, cx: &App) -> Option<Quality> {
    if playback.steered(cx).is_some() {
        return None;
    }
    let signal = playback.track_signal()??;
    let encoding = signal.encoding?;
    let rate = signal.rate.max(1);

    let (label, detail, tier) = match encoding {
        Encoding::Flac { depth, .. } => lossless(depth, rate),
        Encoding::Alac { depth } | Encoding::Pcm { depth } => lossless(depth, rate),
        Encoding::Aac { kbps } => lossy("AAC", kbps),
        Encoding::Vorbis { kbps } => lossy("Ogg Vorbis", kbps),
        Encoding::Mp3 { kbps } => lossy("MP3", kbps),
        Encoding::Opus { kbps } => lossy("Opus", kbps),
        // A codec the engine has no name for is not worth a badge, since it cannot say
        // whether the stream is lossless or not.
        Encoding::Other => return None,
    };

    Some(Quality {
        tier,
        label,
        detail,
    })
}

fn lossless(depth: u8, rate: u32) -> (SharedString, SharedString, Tier) {
    let hi_res = depth > 16 || rate > 48_000;
    let label = match hi_res {
        true => t!("quality-hi-res"),
        false => t!("quality-lossless"),
    };
    let detail = t!(
        "quality-detail",
        depth = i64::from(depth),
        rate = rate_label(rate)
    );
    let tier = match hi_res {
        true => Tier::HiRes,
        false => Tier::Lossless,
    };
    (label, detail, tier)
}

fn lossy(codec: &str, kbps: u32) -> (SharedString, SharedString, Tier) {
    (
        t!("quality-lossy"),
        t!(
            "quality-codec",
            codec = codec.to_owned(),
            kbps = i64::from(kbps)
        ),
        Tier::Lossy,
    )
}

/// 44100 reads as 44.1 and 96000 as 96.
fn rate_label(rate: u32) -> String {
    match rate % 1000 {
        0 => (rate / 1000).to_string(),
        _ => format!("{:.1}", f64::from(rate) / 1000.0),
    }
}

fn color(tier: Tier, theme: &ui::Theme) -> Hsla {
    match tier {
        Tier::HiRes | Tier::Lossless => theme.foreground,
        Tier::Lossy => theme.muted_foreground,
    }
}

/// The label alone, for the player bar where the row is one line tall.
pub(crate) fn badge(id: impl Into<ElementId>, quality: &Quality, theme: ui::Theme) -> AnyElement {
    pill(quality, &theme)
        .id(id)
        .role(gpui::accesskit::Role::Label)
        .aria_label(quality.spoken())
        .into_any_element()
}

/// The label and its figures, for the fullscreen player where there is room under the seek
/// bar to say what "lossless" actually means.
pub(crate) fn line(id: impl Into<ElementId>, quality: &Quality, theme: ui::Theme) -> AnyElement {
    div()
        .id(id)
        .role(gpui::accesskit::Role::Label)
        .aria_label(quality.spoken())
        .flex()
        .items_center()
        .justify_center()
        .gap_2()
        .child(pill(quality, &theme))
        .child(
            div()
                .text_size(theme.text(Text::Tiny))
                .text_color(theme.muted_foreground)
                .child(quality.detail.clone()),
        )
        .into_any_element()
}

fn pill(quality: &Quality, theme: &ui::Theme) -> Div {
    let color = color(quality.tier, theme);
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_full()
        .bg(theme.muted)
        .text_size(theme.text(Text::Tiny))
        .text_color(color)
        .child(
            svg()
                .path(icons::path("icons/audio-lines.svg"))
                .size(px(11.))
                .text_color(color),
        )
        .child(quality.label.clone())
}
