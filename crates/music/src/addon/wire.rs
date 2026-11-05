//! The wire shapes of the addon protocol, as BitChord documents and the live addons send.
//!
//! An addon is an ordinary HTTP server answering `/manifest.json`, `/search?q=` and
//! `/stream/{id}` with JSON. Everything here is read-only: the parser is lenient and ignores
//! unknown keys, so an addon publishing more than this is never a parse failure.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Reads a string field an addon may send as `null` as its default. Null and absent say the
/// same thing here, and one null field must not fail the whole answer.
fn null_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// What an addon says it is and what it can do.
///
/// Only `search` is required to be useful: a search row may carry its own `streamURL`, and an
/// addon that declares nothing at all is still asked, since silence is not a claim.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Manifest {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default)]
    pub settings: Vec<Setting>,
    /// `allowDownloads` — `false` keeps the addon to playback only. Absent means allowed.
    #[serde(default, rename = "allowDownloads")]
    pub allow_downloads: Option<Value>,
    /// `checkValidLossless` — only use when the output can carry lossless. Absent means no
    /// such requirement.
    #[serde(default, rename = "checkValidLossless")]
    pub check_valid_lossless: Option<Value>,
}

impl Manifest {
    /// Read by the downloads work that follows; nothing gates on it yet.
    #[allow(dead_code)]
    pub fn downloads_allowed(&self) -> bool {
        self.allow_downloads.as_flag().unwrap_or(true)
    }

    pub fn requires_lossless_output(&self) -> bool {
        self.check_valid_lossless.as_flag().unwrap_or(false)
    }

    /// Whether this addon is worth asking anything.
    pub fn is_playable(&self) -> bool {
        self.resources.is_empty()
            || self
                .resources
                .iter()
                .any(|resource| resource.eq_ignore_ascii_case("search"))
    }

    /// What to show when the user did not name the addon themselves.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            self.id.clone()
        } else {
            self.name.clone()
        }
    }
}

/// One declared setting, reduced to the two things we do with it: the default to send, and
/// the options a `quality` request is matched against.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Setting {
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub options: Vec<SettingOption>,
}

impl Setting {
    /// The default as the string that would go in a query parameter, or `None`.
    pub fn default_value(&self) -> Option<String> {
        self.default.as_query_value()
    }
}

/// One choice in a select. Only the value travels.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SettingOption {
    #[serde(default)]
    pub value: Option<Value>,
}

impl SettingOption {
    pub fn string_value(&self) -> Option<String> {
        self.value.as_query_value()
    }
}

/// A search answer. Every array is optional; only `tracks` is read.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SearchResponse {
    #[serde(default)]
    pub tracks: Vec<AddonTrack>,
}

/// One row of a search answer.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AddonTrack {
    #[serde(default, deserialize_with = "null_string")]
    pub id: String,
    #[serde(default, deserialize_with = "null_string")]
    pub title: String,
    #[serde(default, deserialize_with = "null_string")]
    pub artist: String,
    #[serde(default, deserialize_with = "null_string")]
    pub album: String,
    /// Seconds. Read as a double so an addon sending `240.0` is not a parse failure.
    #[serde(default)]
    pub duration: Option<f64>,
    #[serde(default, rename = "artworkURL")]
    pub artwork_url: Option<String>,
    #[serde(default, rename = "albumArtworkURL")]
    pub album_artwork_url: Option<String>,
    /// `mp3`, `flac`, `aac`, `m4a` — what the row is, when the addon says.
    #[serde(default, deserialize_with = "null_string")]
    pub format: String,
    /// Not in the spec, but several addons send it and it says the same thing.
    #[serde(default, rename = "audioQuality", deserialize_with = "null_string")]
    pub audio_quality: String,
    /// How the recording is mixed: `STEREO`, `DOLBY_ATMOS`, `SONY_360RA`.
    #[serde(default, rename = "audioMode")]
    pub audio_mode: Option<String>,
    #[serde(default, rename = "audioModes")]
    pub audio_modes: Vec<String>,
    /// Some addons state it outright instead of, or as well as, in `audioModes`.
    #[serde(default)]
    pub atmos: Option<bool>,
    /// A stream URL for the row itself: the spec says the host then skips `/stream`.
    #[serde(default, rename = "streamURL")]
    pub stream_url: Option<String>,
}

impl AddonTrack {
    pub fn duration_secs(&self) -> Option<u64> {
        self.duration.filter(|d| *d > 0.0).map(|d| d as u64)
    }

    /// Whichever artwork the addon filled in.
    pub fn artwork(&self) -> Option<&str> {
        self.artwork_url
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or(self
                .album_artwork_url
                .as_deref()
                .filter(|s| !s.trim().is_empty()))
    }

    /// Whether this row is the immersive mix rather than a stereo one. Worth reading on a row
    /// because a catalogue may publish the Atmos mix as its own track, findable only by label.
    /// Labels rows once the immersive work lands.
    #[allow(dead_code)]
    pub fn is_dolby_atmos(&self) -> bool {
        self.atmos == Some(true)
            || contains_atmos(&format!(
                "{} {} {} {}",
                self.audio_quality,
                self.audio_mode.as_deref().unwrap_or_default(),
                self.audio_modes.join(" "),
                self.format
            ))
    }

    /// Whether the row names the hi-res tier (`HI_RES_LOSSLESS`, `hires-96`, `24-bit`), which
    /// decides whether an upgrade tries this row before the CD renditions.
    pub fn is_hi_res(&self) -> bool {
        contains_hi_res(&format!("{} {}", self.audio_quality, self.format))
    }
}

/// What is on the end of a `/stream/{id}` call.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct AddonStream {
    #[serde(default, deserialize_with = "null_string")]
    pub url: String,
    /// Free text: `mp3`, `flac`, `dash`. Describes the audio, not the transport.
    #[serde(default, deserialize_with = "null_string")]
    pub format: String,
    #[serde(default, deserialize_with = "null_string")]
    pub quality: String,
    /// Non-standard spellings of `quality` that addons in the wild send instead.
    #[serde(default, rename = "streamQuality", deserialize_with = "null_string")]
    pub stream_quality: String,
    #[serde(default, rename = "audioQuality", deserialize_with = "null_string")]
    pub audio_quality: String,
    #[serde(default)]
    pub codec: Option<String>,
    #[serde(default, rename = "fileCodec")]
    pub file_codec: Option<String>,
    #[serde(default)]
    pub container: Option<String>,
    #[serde(default, rename = "containerFormat")]
    pub container_format: Option<String>,
    /// `none`, `hls` or `dash` — what the URL points at.
    #[serde(default)]
    pub manifest: Option<String>,
    /// An older spelling some addons send in place of `manifest`.
    #[serde(default, rename = "mediaType")]
    pub media_type: Option<String>,
    #[serde(default, rename = "mimeType")]
    pub mime_type: Option<String>,
    /// `false`, or the name of a DRM scheme. Anything but false is refused.
    #[serde(default)]
    pub encrypted: Option<Value>,
    #[serde(default, rename = "sampleRate")]
    pub sample_rate: Option<f64>,
    #[serde(default, rename = "bitDepth")]
    pub bit_depth: Option<f64>,
    /// Bits per second, per the spec's neighbours; normalised in [Self::kbps].
    #[serde(default)]
    pub bitrate: Option<f64>,
    #[serde(default, rename = "audioMode")]
    pub audio_mode: Option<String>,
    #[serde(default, rename = "audioModes")]
    pub audio_modes: Vec<String>,
    /// Why there is no `url`, when the addon bothered to say.
    #[serde(default)]
    pub error: Option<String>,
}

impl AddonStream {
    /// Whichever codec field the addon filled in, lowercased.
    pub fn stated_codec(&self) -> Option<String> {
        self.codec
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or(self.file_codec.as_deref().filter(|s| !s.trim().is_empty()))
            .map(str::to_lowercase)
    }

    /// Whichever container field the addon filled in, lowercased. Read by the DASH work.
    #[allow(dead_code)]
    pub fn stated_container(&self) -> Option<String> {
        self.container
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or(self
                .container_format
                .as_deref()
                .filter(|s| !s.trim().is_empty()))
            .map(str::to_lowercase)
    }

    /// Every free-text quality field at once, for the label readers to scan.
    pub fn quality_text(&self) -> String {
        format!(
            "{} {} {} {}",
            self.quality, self.stream_quality, self.audio_quality, self.format
        )
    }

    /// The codec this stream is in: whichever codec field the addon filled in, or the one its
    /// format and quality labels name. `None` when it says nothing usable.
    pub fn codec_name(&self) -> Option<String> {
        self.stated_codec()
            .or_else(|| codec_from_text(&self.format))
            .or_else(|| {
                codec_from_text(&format!(
                    "{} {} {}",
                    self.quality, self.stream_quality, self.audio_quality
                ))
            })
    }

    /// Whether this is a lossless rendition. A stream that names a lossy codec is never
    /// lossless, whatever else its labels claim; one that names no codec at all is taken at
    /// its word only when a quality label says so.
    pub fn is_lossless(&self) -> bool {
        match self.codec_name().as_deref() {
            Some("flac" | "alac" | "wav" | "aiff" | "pcm") => true,
            Some(_) => false,
            None => {
                let text = self.quality_text().to_lowercase();
                text.contains("lossless") && !text.contains("lossy")
            }
        }
    }

    /// Whether this rendition is behind a DRM scheme. The spec is unambiguous that an addon
    /// must never send a scheme unless the request carried `?drm=`, which we never do.
    pub fn is_encrypted(&self) -> bool {
        let Some(element) = self.encrypted.as_ref() else {
            return false;
        };
        element.as_flag().unwrap_or_else(|| {
            element
                .as_str()
                .map(|s| {
                    let s = s.trim();
                    !s.is_empty()
                        && !s.eq_ignore_ascii_case("false")
                        && !s.eq_ignore_ascii_case("none")
                })
                .unwrap_or(false)
        })
    }

    /// The transport the addon declares: `hls`, `dash`, or `None` when the URL is the audio
    /// itself or the addon says nothing. `format` is consulted last because it usually holds a
    /// codec — `flac`, `mp3` — and only the two words that can *only* mean a transport count.
    pub fn transport(&self) -> Option<Transport> {
        let stated = self
            .manifest
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or(self.media_type.as_deref().filter(|s| !s.trim().is_empty()))
            .or(self
                .format
                .as_str()
                .trim()
                .is_empty()
                .then_some("")
                .filter(|_| false))
            .or((!self.format.trim().is_empty()).then_some(self.format.as_str()))?;
        match stated.trim().to_lowercase().as_str() {
            "hls" | "m3u8" | "application/x-mpegurl" | "application/vnd.apple.mpegurl" => {
                Some(Transport::Hls)
            }
            "dash" | "mpd" | "application/dash+xml" => Some(Transport::Dash),
            _ => None,
        }
    }

    /// The sample rate, from the field if the addon filled it in and from its own quality label
    /// if it did not. Some publishers state kHz where the field is specified in Hz.
    pub fn sample_rate_hz(&self) -> Option<u32> {
        if let Some(rate) = self.sample_rate.filter(|r| *r > 0.0) {
            return Some(if rate < 1000.0 {
                (rate * 1000.0) as u32
            } else {
                rate as u32
            });
        }
        number_before_unit(&self.quality_text(), "khz").map(|value| (value * 1000.0) as u32)
    }

    /// As [Self::sample_rate_hz], for bit depth: the number in `24-bit` when there is no field.
    pub fn bits(&self) -> Option<u32> {
        if let Some(depth) = self.bit_depth.filter(|d| *d > 0.0) {
            return Some(depth as u32);
        }
        number_before_unit(&self.quality_text(), "bit")
            .map(|value| value as u32)
            .filter(|value| (8..=32).contains(value))
    }

    /// The bitrate in kbps, whichever unit the addon used: a FLAC at `1411` and an MP3 at
    /// `320000` are the same claim in different units.
    pub fn kbps(&self) -> Option<u32> {
        if let Some(rate) = self.bitrate.filter(|r| *r > 0.0) {
            return Some(if rate > 3_000.0 {
                (rate / 1000.0) as u32
            } else {
                rate as u32
            });
        }
        number_before_unit(&self.quality_text(), "kbps").map(|value| value as u32)
    }

    /// Whether the addon says this is the immersive mix rather than a stereo one. Read by the
    /// immersive work; nothing renders it yet.
    #[allow(dead_code)]
    pub fn is_dolby_atmos(&self) -> bool {
        contains_atmos(&format!(
            "{} {} {} {}",
            self.quality_text(),
            self.audio_mode.as_deref().unwrap_or_default(),
            self.audio_modes.join(" "),
            self.stated_codec().unwrap_or_default()
        ))
    }
}

/// What a stream URL points at, when it is not the audio file itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Hls,
    Dash,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hls => "hls",
            Self::Dash => "dash",
        }
    }
}

/// A manifest switch, however the addon wrote it: `1`/`0`, `true`/`false`, or either as a
/// string. `None` — "use the default" — for absent, `null`, or anything else.
fn flag_of(element: &Value) -> Option<bool> {
    match element {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => number.as_f64().map(|value| value != 0.0),
        Value::String(text) => match text.trim().to_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => Some(true),
            "0" | "false" | "no" | "off" => Some(false),
            _ => text.trim().parse::<f64>().ok().map(|value| value != 0.0),
        },
        _ => None,
    }
}

/// A JSON scalar as the string that would go in a query parameter. Numbers stay integral
/// (`320`, never `320.0`), since the value is handed to somebody's query-string parser and
/// compared against an options list.
trait QueryValue {
    fn as_query_value(&self) -> Option<String>;
    fn as_flag(&self) -> Option<bool>;
}

impl QueryValue for Option<Value> {
    fn as_query_value(&self) -> Option<String> {
        self.as_ref()?.as_query_value()
    }

    fn as_flag(&self) -> Option<bool> {
        self.as_ref().and_then(flag_of)
    }
}

impl QueryValue for Value {
    fn as_query_value(&self) -> Option<String> {
        match self {
            Value::String(text) => (!text.trim().is_empty()).then(|| text.clone()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        }
    }

    fn as_flag(&self) -> Option<bool> {
        flag_of(self)
    }
}

/// The codec word inside a free-text format or quality field, when it names one.
fn codec_from_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for codec in [
        "flac", "alac", "mp3", "aac", "m4a", "opus", "vorbis", "wav", "aiff", "pcm",
    ] {
        if lower.contains(codec) {
            return Some(codec.to_owned());
        }
    }
    None
}

/// The first number that sits before `unit` in `text`, for the quality labels that carry the
/// only statement of rate, depth or bitrate an addon makes.
fn number_before_unit(text: &str, unit: &str) -> Option<f64> {
    let lower = text.to_lowercase();
    let mut search_from = lower.len();
    while let Some(index) = lower[..search_from].rfind(unit) {
        // The number may sit right against the unit or a space or two away: `96kHz`,
        // `96 kHz`, `1411 kbps`, `24-bit`.
        let mut end = index;
        while end > 0 && lower.as_bytes()[end - 1].is_ascii_whitespace() {
            end -= 1;
        }
        if end > 0 && lower.as_bytes()[end - 1] == b'-' {
            end -= 1;
        }
        let mut start = end;
        while start > 0 {
            let byte = lower.as_bytes()[start - 1];
            if byte.is_ascii_digit() || byte == b'.' {
                start -= 1;
            } else {
                break;
            }
        }
        if start < end
            && let Ok(value) = lower[start..end].parse::<f64>()
        {
            return Some(value);
        }
        search_from = index;
        if search_from == 0 {
            return None;
        }
    }
    None
}

/// How a label spells the hi-res lossless tier.
fn contains_hi_res(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("hi-res")
        || lower.contains("hires")
        || lower.contains("hi_res")
        || lower.contains("hi res")
        || lower.contains("24-bit")
        || lower.contains("24bit")
        || lower.contains("24 bit")
}

/// The spellings an addon uses for an immersive mix, in any free-text field.
fn contains_atmos(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("atmos")
        || lower.contains("dolby")
        || lower.contains("eac3-joc")
        || lower.contains("eac3_joc")
        || lower.contains("ec-3")
        || lower.contains("ec3")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lenient_manifest_parses() {
        let manifest: Manifest = serde_json::from_str(
            r#"{"id":"tidal","name":"Tidal","version":"1.0.0","resources":["search","stream"],
                "settings":[{"key":"quality","default":"LOSSLESS",
                "options":[{"value":"HI_RES"},{"value":"LOSSLESS"}]}]}"#,
        )
        .unwrap();
        assert!(manifest.is_playable());
        assert!(manifest.downloads_allowed());
        assert_eq!(
            manifest.settings[0].default_value().as_deref(),
            Some("LOSSLESS")
        );
        assert_eq!(
            manifest.settings[0].options[1].string_value().as_deref(),
            Some("LOSSLESS")
        );
    }

    #[test]
    fn flags_accept_words_numbers_and_booleans() {
        let manifest: Manifest =
            serde_json::from_str(r#"{"allowDownloads":"false","checkValidLossless":"yes"}"#)
                .unwrap();
        assert!(!manifest.downloads_allowed());
        assert!(manifest.requires_lossless_output());
    }

    #[test]
    fn stream_readers_pick_labels_out_of_free_text() {
        let stream: AddonStream = serde_json::from_str(
            r#"{"url":"https://x/y.flac","format":"flac",
                "quality":"Tidal · Hi-Res FLAC (DASH) 24-bit 96 kHz"}"#,
        )
        .unwrap();
        assert_eq!(stream.sample_rate_hz(), Some(96_000));
        assert_eq!(stream.bits(), Some(24));
        assert!(stream.transport().is_none());
    }

    #[test]
    fn dash_is_read_out_of_the_format_field() {
        let stream: AddonStream = serde_json::from_str(
            r#"{"url":"https://im-fa.manifest.tidal.com/x","format":"dash",
                "quality":"Tidal · Hi-Res FLAC (DASH)","streamQuality":"[Tidal] HI_RES_LOSSLESS"}"#,
        )
        .unwrap();
        assert_eq!(stream.transport(), Some(Transport::Dash));
    }

    #[test]
    fn drm_is_refused_in_every_spelling() {
        for value in [r#"false"#, r#""none""#, r#"0"#] {
            let stream: AddonStream =
                serde_json::from_str(&format!(r#"{{"encrypted":{value}}}"#)).unwrap();
            assert!(!stream.is_encrypted(), "{value} should read as clear");
        }
        for value in [r#"true"#, r#""widevine""#] {
            let stream: AddonStream =
                serde_json::from_str(&format!(r#"{{"encrypted":{value}}}"#)).unwrap();
            assert!(stream.is_encrypted(), "{value} should be refused");
        }
    }

    #[test]
    fn numbers_stay_integral_in_query_values() {
        let setting: Setting = serde_json::from_str(r#"{"key":"quality","default":320}"#).unwrap();
        assert_eq!(setting.default_value().as_deref(), Some("320"));
    }
}
