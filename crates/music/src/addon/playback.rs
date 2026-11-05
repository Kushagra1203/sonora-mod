//! Playback for addons: what [`crate::engine`] needs that the protocol's own is.
//!
//! An addon resolves a track to an ordinary audio URL and the bytes stream like every other
//! provider's, through [`crate::stream`] and a rodio decoder. DASH and HLS answers (Tidal
//! hi-res arrives as the former) are refused with a message for now rather than played
//! wrong; the segmented path is a later piece of work.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use async_trait::async_trait;

use super::client::{AGENT, Tier};
use super::wire::AddonStream;
use super::{Shared, parse_id};
use crate::aac::Aac;
use crate::engine::{self, Fetch, Loudness};
use crate::stream::{Plain, Reader, Stream};
use crate::{Encoding, PlaybackConfig, PlaybackEvents, PlaybackFactory, Player, Quality};

/// How long the stream resolution and then the preroll may each take before the attempt is
/// given up, so an addon that accepted a request and never answered cannot hold the engine on
/// a silent track forever.
const PATIENCE: Duration = Duration::from_secs(20);

/// A track arriving, with what the addon said about its encoding.
#[derive(Clone)]
pub struct Loaded {
    stream: Stream,
    encoding: Option<Encoding>,
    duration: Option<Duration>,
}

pub struct Factory {
    shared: Arc<Shared>,
}

impl Factory {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self { shared }
    }
}

impl PlaybackFactory for Factory {
    fn start(&self, config: PlaybackConfig) -> (Box<dyn Player>, Box<dyn PlaybackEvents>) {
        engine::start(
            Addons {
                shared: self.shared.clone(),
                tier: tier(config.quality),
            },
            config,
        )
    }
}

struct Addons {
    shared: Arc<Shared>,
    tier: Tier,
}

/// The tier a settings level asks an addon for. Hi-res is asked for as lossless because the
/// progressive lossless stream is the best this provider can play; the addon decides whether
/// it answers louder.
fn tier(quality: Quality) -> Tier {
    match quality {
        Quality::HiRes => Tier::HiRes,
        Quality::Lossless => Tier::Lossless,
        Quality::High => Tier::High,
        Quality::Normal | Quality::Low => Tier::Low,
    }
}

#[async_trait]
impl Fetch for Addons {
    type Loaded = Loaded;
    type Source = rodio::Decoder<Reader>;

    fn name(&self) -> &'static str {
        "addon"
    }

    /// Resolves the track through its addon — or through the stream URL its search row
    /// carried, when there was one — and opens the download.
    async fn load(&self, id: &str) -> Result<Loaded> {
        let (key, track_id) = parse_id(id).context("that is not an addon track id")?;
        self.shared.sync();
        let addon = self.shared.addon(key).context(
            "the addon that held this track is no longer configured - search for it again",
        )?;

        let remembered = self
            .shared
            .recall(id)
            .and_then(|row| row.stream_url.clone())
            .filter(|url| !url.trim().is_empty());
        let resolved = match self.shared.take_stream(id) {
            // an upgrade already resolved and checked this stream a moment ago
            Some(stream) => stream,
            None => match remembered {
                Some(url) => AddonStream {
                    url,
                    ..AddonStream::default()
                },
                None => addon
                    .client
                    .stream(track_id, self.tier)
                    .await?
                    .context("this addon does not hold that track")?,
            },
        };

        if let Some(transport) = resolved.transport() {
            bail!(
                "this track is served as a {} manifest (hi-res); addons cannot play segmented \
                 streams yet",
                transport.as_str()
            );
        }
        if resolved.is_encrypted() {
            bail!("this addon answered with a DRM-protected stream, which is refused");
        }

        let response = addon
            .client
            .http()
            .get(&resolved.url)
            .header("User-Agent", AGENT)
            .send()
            .await
            .context("cannot open the addon stream")?;
        if !response.status().is_success() {
            bail!("the addon stream answered HTTP {}", response.status());
        }
        let total = response.content_length();
        let stream =
            tokio::time::timeout(PATIENCE, Stream::pulling(response, total, Plain).primed())
                .await
                .context("the addon stream preroll timed out")??;
        log::debug!(
            "playback: {id} started, {} {}, {:.1} MiB",
            resolved.format.trim(),
            resolved
                .kbps()
                .map(|kbps| format!("{kbps} kbps"))
                .unwrap_or_default(),
            total.unwrap_or_default() as f64 / (1024.0 * 1024.0)
        );
        Ok(Loaded {
            stream,
            encoding: encoding_of(&resolved),
            duration: None,
        })
    }

    fn length(&self, loaded: &Loaded) -> Option<Duration> {
        loaded.duration
    }

    fn loudness(&self, _loaded: &Loaded) -> Option<Loudness> {
        None
    }

    fn encoding(&self, loaded: &Loaded) -> Option<Encoding> {
        loaded.encoding
    }

    async fn downloaded(&self, loaded: &Loaded) {
        loaded.stream.finished().await;
    }

    fn progress(&self, loaded: &Loaded) -> Option<f32> {
        let total = loaded.stream.total().filter(|total| *total > 0)?;
        Some(loaded.stream.arrived() as f32 / total as f32)
    }

    /// Builds a decoder over the stream and places it at `at`, the way Subsonic's does: the
    /// bytes past the preroll are still arriving, so this only reads the header, and a source
    /// of unknown length stays forward-only.
    fn open(&self, id: &str, loaded: &Loaded, at: Duration) -> Option<Self::Source> {
        self.open_near(id, loaded, at).map(|(source, _)| source)
    }

    /// Opens a decoder at `at` and says where it really starts. A copy that could not be placed
    /// is reported from its own beginning rather than from the place asked for, so a hand-over
    /// to it can tell that the song would start again.
    fn open_near(
        &self,
        id: &str,
        loaded: &Loaded,
        at: Duration,
    ) -> Option<(Self::Source, Duration)> {
        let total = loaded.stream.total();
        let mut builder = rodio::Decoder::builder()
            .with_symphonia_decoder::<Aac>()
            .with_data(loaded.stream.reader())
            .with_seekable(total.is_some());
        if let Some(total) = total {
            builder = builder.with_byte_len(total);
        }
        let mut decoder = match builder.build() {
            Ok(decoder) => decoder,
            Err(error) => {
                log::warn!("playback: cannot decode the addon track {id}: {error}");
                return None;
            }
        };
        let mut base = at;
        if !at.is_zero()
            && let Err(error) = rodio::Source::try_seek(&mut decoder, at)
        {
            log::warn!(
                "playback: cannot start the addon track {id} at {}s: {error}",
                at.as_secs()
            );
            base = Duration::ZERO;
        }
        Some((decoder, base))
    }

    /// A decoder opened before the whole track arrived can only read forward, so every seek
    /// starts a fresh one that knows the length by then.
    fn reopen_to_seek(&self) -> bool {
        true
    }
}

/// What the addon said the stream is, for the signal path. Anything it did not say falls back
/// to what the format word alone implies, since a FLAC is a FLAC however tight-lipped the
/// server is.
fn encoding_of(stream: &AddonStream) -> Option<Encoding> {
    let codec = match stream.codec_name() {
        Some(codec) => codec,
        None if stream.is_lossless() => "flac".to_owned(),
        None => return None,
    };
    Some(match codec.as_str() {
        "flac" => Encoding::Flac {
            depth: stream.bits().unwrap_or(16) as u8,
            rate: stream.sample_rate_hz().unwrap_or(44_100),
        },
        "alac" => Encoding::Alac {
            depth: stream.bits().unwrap_or(16) as u8,
        },
        "mp3" => Encoding::Mp3 {
            kbps: stream.kbps().unwrap_or(320),
        },
        "aac" | "m4a" | "mp4" => Encoding::Aac {
            kbps: stream.kbps().unwrap_or(256),
        },
        "opus" => Encoding::Opus {
            kbps: stream.kbps().unwrap_or(160),
        },
        "ogg" | "vorbis" => Encoding::Vorbis {
            kbps: stream.kbps().unwrap_or(160),
        },
        "wav" | "pcm" | "aiff" => Encoding::Pcm { depth: 16 },
        _ => Encoding::Other,
    })
}
