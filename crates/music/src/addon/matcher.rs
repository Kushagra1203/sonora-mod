//! Whether a catalogue's row is the same recording as the track playing, and what to ask that
//! catalogue for in the first place.
//!
//! Ported from BitChord's `TrackMatcher`, which the addon ecosystem is built around: the same
//! title parsing (packaging stripped, version markers kept), the same artist and runtime rules,
//! and the same multi-script query generation. The judgement is split out because both of its
//! failure modes are silent:
//!
//! - **Too loose** and the wrong recording plays under the right title — a cover, a remix, an
//!   hour-long loop — with nothing on screen to say so.
//! - **Too strict** and the addon that genuinely holds the track is quietly never used, which is
//!   what a YouTube title like `罪の香り - Tsumi No Kaori` did against a catalogue that files it
//!   as `Tsumi No Kaori`.
//!
//! The way out of both is to be explicit about *what part of a title carries identity*: the
//! title proper must agree, the take markers (`remix`, `live`, `acoustic`) must agree in both
//! directions, and everything else between brackets — the film, the label, `Official Audio` —
//! is packaging that never vetoes a match. A dash joining a non-Latin head to a Latin tail is
//! the transliteration shape, so the tail is the identity and the head becomes context.

use std::collections::{BTreeSet, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use unicode_normalization::UnicodeNormalization as _;

use super::wire::AddonTrack;
use crate::Track;

/// The recording being looked for, as much of it as the queue knows.
#[derive(Clone, Debug, Default)]
pub(crate) struct Target {
    pub title: String,
    pub artist: String,
    /// Runtime in whole seconds; `None` when the queue row never carried one.
    pub duration_sec: Option<u64>,
    /// Release name, when the queue knows it, to separate catalogue collisions.
    pub album: Option<String>,
    /// Music-video timing includes visual intros and outros that catalogue audio omits.
    pub is_video: bool,
}

impl Target {
    pub(crate) fn of(track: &Track) -> Self {
        Self {
            title: track.name.clone(),
            artist: track.artists.clone(),
            duration_sec: (!track.duration.is_zero()).then_some(track.duration.as_secs()),
            album: (!track.album.trim().is_empty()).then(|| track.album.clone()),
            is_video: false,
        }
    }
}

// ── Asking ───────────────────────────────────────────────────────────────────

/// What to put to a catalogue's search box, best query first.
///
/// The raw title is deliberately not one of them: a YouTube title carries the packaging —
/// `Paniyon Sa (From "Satyamev Jayate")` — and handing that verbatim to a catalogue that lists
/// the track as `Paniyon Sa` is asking it to match on words it has never stored. A title whose
/// head is non-Latin and whose tail is Latin gives the tail as the identity, so
/// `罪の香り - Tsumi No Kaori` is asked for as `Tsumi No Kaori`.
///
/// Two queries, not one: the second drops the artist, for the catalogues that credit a track to
/// the composer or the film rather than the singer and would otherwise score every result down.
/// A kana title gets a third, in romaji, for the catalogues that file it under the other script.
pub(crate) fn queries(target: &Target) -> Vec<String> {
    let title = searchable_title(&target.title, &target.artist);
    if title.trim().is_empty() {
        return Vec::new();
    }
    let artist = primary_artist(&target.artist);
    let mut out = match artist.is_empty() {
        true => vec![title.clone()],
        false => vec![format!("{title} {artist}"), title.clone()],
    };
    let latin = romaji_of(title.trim()).trim().to_owned();
    let other = latin
        .chars()
        .all(|character| LATIN_QUERY_CHARS.contains(character))
        .then_some(latin)
        .filter(|latin| !latin.is_empty() && latin.as_str() != title.as_str())
        .or_else(|| {
            script_mates(&target.title)
                .into_iter()
                .next()
                .filter(|mate| !mate.is_empty() && mate.as_str() != title.as_str())
        });
    if let Some(other) = other
        && !out.contains(&other)
    {
        out.push(other);
    }
    out
}

/// The title with the packaging taken off, version markers kept, and a Latin tail standing in
/// for a non-Latin head.
fn searchable_title(title: &str, artist: &str) -> String {
    let parts = parse_title(title, artist);
    let mut words = parts.words;
    words.extend(parts.versions);
    words.join(" ")
}

/// The first credited artist — who a catalogue is most likely to file the track under.
fn primary_artist(artist: &str) -> String {
    let primary = ARTIST_SEPARATORS
        .split(&normalize(artist))
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    match YOUTUBE_TYPE_LABELS.contains(primary.as_str()) {
        true => String::new(),
        false => primary,
    }
}

// ── Judging ──────────────────────────────────────────────────────────────────

/// How confident this is the same recording, or `None` when it is not one.
///
/// `None` is the common answer and the safe one: it costs an addon its turn, and the stream
/// already playing — usually YouTube, which by definition has the track — keeps going.
fn score(candidate: &AddonTrack, target: &Target) -> Option<i64> {
    let wanted = parse_title(&target.title, &target.artist);
    let got = parse_title(&candidate.title, &candidate.artist);
    if wanted.core.is_empty() || got.core.is_empty() {
        return None;
    }
    // Same word, different script: katakana versus romaji is bridged by transliteration; kanji
    // has no algorithmic reading and stays exact-only. Past that, a guarded containment absorbs
    // minor punctuation and symbol differences, and a bracketed translation of the other
    // script is the same title written twice.
    let title_match = wanted.core == got.core
        || romaji_of(&wanted.core) == romaji_of(&got.core)
        || is_translation_match(&wanted, &got)
        || (wanted.core.chars().count() > 3
            && got.core.chars().count() > 3
            && (wanted.core.contains(&got.core) || got.core.contains(&wanted.core)));
    if !title_match {
        return None;
    }
    // Direction matters both ways round: asking for the album cut must not land on the live
    // take, and asking for the live take must not land on the album cut.
    let album_marks = album_version_markers(&candidate.album);
    let mut effective_versions = got.versions.clone();
    for mark in &album_marks {
        if NOVOCAL_TAKES.contains(mark.as_str()) || wanted.versions.contains(mark) {
            effective_versions.insert(mark.clone());
        }
    }
    if wanted.versions != effective_versions {
        return None;
    }

    let credited = artist_score(&target.artist, &candidate.artist);
    let duration = duration_score(
        target.duration_sec,
        candidate.duration_secs(),
        credited.is_some(),
    )?;
    let artist = match credited {
        Some(artist) => artist,
        // The credits name different people, which film catalogues do when one side files by
        // the composer and the other by the singer. An exact runtime to the second is allowed
        // to stand in for a shared credit, and only an exact one.
        None if !target.is_video && within_seconds(candidate, target, CREDIT_OVERRIDE_SEC) => {
            CREDITS_DISAGREE
        }
        None => return None,
    };
    Some(
        BASE + artist
            + duration
            + album_score(target.album.as_deref(), &candidate.album)
            + context_score(&wanted, &got),
    )
}

/// Every candidate that really is `target`, most confident first.
///
/// Best, not first: a search for a track routinely answers with the single, the album cut, a
/// sped-up edit and a karaoke version in whatever order the backend felt like. Scoring them all
/// and taking the top lets the runtime and the fuller artist credit break that tie. A row the
/// requested artist credited is kept over one that only a matching runtime vouched for.
pub(crate) fn ranked<'a>(candidates: &'a [AddonTrack], target: &Target) -> Vec<&'a AddonTrack> {
    let scored: Vec<(&AddonTrack, i64)> = candidates
        .iter()
        .filter_map(|candidate| score(candidate, target).map(|score| (candidate, score)))
        .collect();
    let credited: Vec<(&AddonTrack, i64)> = scored
        .iter()
        .filter(|(candidate, _)| artist_score(&target.artist, &candidate.artist).is_some())
        .copied()
        .collect();
    let mut scored = match credited.is_empty() {
        true => scored,
        false => credited,
    };
    scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
    scored.into_iter().map(|(candidate, _)| candidate).collect()
}

/// Whether `candidate` states a runtime, and one within `seconds` of `target`'s. Both halves
/// are requirements: a candidate that does not say how long it is cannot be checked.
fn within_seconds(candidate: &AddonTrack, target: &Target, seconds: u64) -> bool {
    let (Some(wanted), Some(got)) = (target.duration_sec, candidate.duration_secs()) else {
        return false;
    };
    wanted.abs_diff(got) <= seconds
}

// ── Title ────────────────────────────────────────────────────────────────────

/// A title split into the part that is the recording's identity and the parts that are the
/// listing's.
#[derive(Clone, Debug)]
struct TitleParts {
    /// The title proper, lowercased and folded, one entry per word.
    words: Vec<String>,
    /// `words` with everything but letters and digits removed — what identity is compared on.
    core: String,
    /// Markers that mean a different take of the same song: `remix`, `live`, `acoustic`.
    versions: BTreeSet<String>,
    /// Words dropped with the packaging, in the order they were read. A scoring hint, never a
    /// veto.
    context: Vec<String>,
}

fn parse_title(raw: &str, artist: &str) -> TitleParts {
    let mut versions = BTreeSet::new();
    let mut context: Vec<String> = Vec::new();
    let mut text = normalize(raw);

    // A guest credit printed in Portuguese is a feat. credit, and `part` alone is a version
    // marker: left to the bracket pass it comes back out as the query "… part".
    text = strip_pt_br_featuring(&text);

    // Bracketed asides, innermost first: "(From "Satyamev Jayate")", "[Official Audio]".
    for _ in 0..BRACKET_PASSES {
        if !BRACKETED.is_match(&text) {
            break;
        }
        text = BRACKETED
            .replace_all(&text, |captures: &regex::Captures| {
                classify(&captures[1], &mut versions, &mut context);
                " "
            })
            .into_owned();
    }
    // An unbalanced bracket — a title truncated mid-aside — takes the rest with it rather than
    // leaving half an aside in the core.
    if let Some(open) = text.find(['(', '[']) {
        let rest = text[open..].to_owned();
        classify(&rest, &mut versions, &mut context);
        text.truncate(open);
    }

    // Dash- and pipe-separated tails: "Paniyon Sa - Satyamev Jayate", "Song | Official Video".
    // The head is normally the title, but the "Artist - Title" upload convention inverts that,
    // and a mixed-script head with no Latin in it is the transliteration shape: catalogues file
    // the track under the Latin tail, so the tail becomes the identity and the head is context.
    for _ in 0..DASH_PASSES {
        let Some(dash) = DASH.find(&text) else {
            break;
        };
        let head = text[..dash.start()].to_owned();
        let tail = text[dash.end()..].to_owned();
        let head_is_artist = is_artist_name(&head, artist);
        let latin_tail =
            !has_filing_latin(&head) && has_filing_latin(&tail) && !is_artist_name(&tail, artist);
        if head_is_artist || latin_tail {
            classify(&head, &mut versions, &mut context);
            text = tail;
        } else {
            classify(&tail, &mut versions, &mut context);
            text = head;
        }
    }

    // A feat. credit belongs to the artist field wherever a catalogue chooses to print it.
    text = FEATURING.replace(&text, " ").into_owned();

    // CJK languages do not use spaces between words, so their titles keep the raw character
    // sequence intact rather than being split into fragments.
    let contains_cjk = text.chars().any(is_cjk);

    let mut words: Vec<String> = WORD_SPLIT
        .split(&text)
        .map(|part| NON_ALNUM.replace_all(part, "").into_owned())
        .filter(|word| !word.is_empty() && !JOINING_WORDS.contains(word.as_str()))
        .collect();
    // An upload's trailing label, printed without brackets to hang it on, and the unambiguous
    // take markers, which belong in versions so both spellings of one take meet.
    while words.len() > 1 {
        let last = words.last().cloned().unwrap_or_default();
        if TRAILING_NOISE.contains(last.as_str()) {
            words.pop();
        } else if TRAILING_TAKES.contains(last.as_str()) {
            versions.insert(last);
            words.pop();
        } else {
            break;
        }
    }

    let core = match contains_cjk {
        true => CJK_STRIP.replace_all(&text, "").into_owned(),
        false => words.concat(),
    };

    TitleParts {
        words,
        core,
        versions,
        context,
    }
}

/// Files one dropped segment under `versions` or `context`. A segment naming a take is
/// identity; everything else is packaging, with `Album Version` and `Radio Edit` standing for
/// the ordinary release rather than a take of their own.
fn classify(segment: &str, versions: &mut BTreeSet<String>, context: &mut Vec<String>) {
    let words: Vec<String> = WORD_SPLIT
        .split(segment)
        .map(|part| NON_ALNUM.replace_all(part, "").into_owned())
        .filter(|word| !word.is_empty())
        .collect();
    if words.is_empty() {
        return;
    }
    if NEUTRAL_SEGMENTS.contains(words.concat().as_str()) {
        return;
    }
    let marks: Vec<String> = words
        .iter()
        .filter(|word| VERSION_WORDS.contains(word.as_str()))
        .cloned()
        .collect();
    if !marks.is_empty() {
        versions.extend(marks);
        return;
    }
    for word in words {
        let kept = (word.chars().count() > 2 || word.chars().any(is_cjk))
            && !NOISE_WORDS.contains(word.as_str());
        if kept && !context.contains(&word) {
            context.push(word);
        }
    }
}

/// Drops a Portuguese guest credit — "O Mundo É Mal (part. Clara Lima)" — which is a feat.
/// credit spelled differently. A numeral or roman numeral after the marker is still the title
/// ("Part. 2", "Part II"), and `parte` is a different word, so both stay.
fn strip_pt_br_featuring(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0usize;
    for captures in PT_BR_FEATURING.captures_iter(text) {
        let (Some(whole), Some(marker), Some(tail)) =
            (captures.get(0), captures.get(1), captures.get(2))
        else {
            continue;
        };
        let dotted = marker.as_str().trim_end().ends_with('.');
        if dotted && starts_with_number_word(tail.as_str()) {
            continue;
        }
        out.push_str(&text[last..whole.start()]);
        out.push(' ');
        last = whole.end();
    }
    out.push_str(&text[last..]);
    out
}

/// Whether the first word is a plain number or a roman numeral, which after `part.` makes the
/// marker part of the title rather than a guest credit.
fn starts_with_number_word(text: &str) -> bool {
    let word = text
        .trim_start()
        .split(|character: char| character.is_whitespace() || character == ')' || character == ']')
        .next()
        .unwrap_or_default();
    !word.is_empty()
        && (word.chars().all(|character| character.is_ascii_digit())
            || word
                .chars()
                .all(|character| matches!(character.to_ascii_lowercase(), 'i' | 'v' | 'x')))
}

/// Whether a bracketed aside is the same title in another script rather than packaging around
/// it: one core CJK-only, the other Latin, and the other's aside is that core in full.
fn is_translation_match(wanted: &TitleParts, got: &TitleParts) -> bool {
    let wanted_cjk = wanted.core.chars().any(is_cjk) && !has_filing_latin(&wanted.core);
    let got_cjk = got.core.chars().any(is_cjk) && !has_filing_latin(&got.core);
    let wanted_latin = has_filing_latin(&wanted.core);
    let got_latin = has_filing_latin(&got.core);
    if !((wanted_cjk && got_latin) || (got_cjk && wanted_latin)) {
        return false;
    }
    wanted.context.concat() == got.core || got.context.concat() == wanted.core
}

/// Whether `text` is nothing but (part of) `artist` — the "Artist - Title" upload shape.
fn is_artist_name(text: &str, artist: &str) -> bool {
    if artist.trim().is_empty() {
        return false;
    }
    let words: Vec<String> = WORD_SPLIT
        .split(text)
        .map(|part| NON_ALNUM.replace_all(part, "").into_owned())
        .filter(|word| !word.is_empty())
        .collect();
    if words.is_empty() {
        return false;
    }
    let credited: HashSet<String> = WORD_SPLIT
        .split(&normalize(artist))
        .map(|part| NON_ALNUM.replace_all(part, "").into_owned())
        .filter(|word| !word.is_empty())
        .collect();
    words.iter().all(|word| credited.contains(word))
}

/// NFKC plus lowercase: full-width alphanumerics fold to ASCII, CJK brackets to ASCII parens
/// so the bracket pass handles `「title」` like `(title)`, and the letters themselves survive.
fn normalize(text: &str) -> String {
    let folded: String = text.nfkc().collect();
    let folded = folded.to_lowercase();
    let folded = folded.replace('&', " and ");
    let folded = CJK_OPEN.replace_all(&folded, "(");
    CJK_CLOSE.replace_all(&folded, ")").into_owned()
}

/// Latin that actually files the track: a head holding nothing but a take marker carries no
/// filing, so the unmatchable script is not kept as the identity with it.
fn has_filing_latin(text: &str) -> bool {
    WORD_SPLIT
        .split(text)
        .map(|part| NON_ALNUM.replace_all(part, "").into_owned())
        .filter(|word| !word.is_empty())
        .any(|word| {
            LATIN.is_match(&word)
                && !VERSION_WORDS.contains(word.as_str())
                && !NOISE_WORDS.contains(word.as_str())
                && !TRAILING_NOISE.contains(word.as_str())
                && !JOINING_WORDS.contains(word.as_str())
        })
}

/// Both spellings when a dash joins the same word in two scripts, either order. Empty unless
/// the two sides transliterate to each other, so a film packaging never qualifies.
fn script_mates(raw: &str) -> Vec<String> {
    let text = normalize(raw);
    let Some(dash) = DASH.find(&text) else {
        return Vec::new();
    };
    let head = searchable_title(&text[..dash.start()], "");
    let tail = searchable_title(&text[dash.end()..], "");
    if head.is_empty() || tail.is_empty() || head == tail {
        return Vec::new();
    }
    match romaji_of(&head) == tail || romaji_of(&tail) == head {
        true => vec![head, tail],
        false => Vec::new(),
    }
}

// ── Artist ───────────────────────────────────────────────────────────────────

/// Points for the credit agreeing, or `None` when it disagrees. A side with no credit at all
/// scores zero rather than failing: there is nothing to disagree with, and the title has
/// already had to match.
fn artist_score(wanted: &str, got: &str) -> Option<i64> {
    let want = artist_names(wanted);
    let have = artist_names(got);
    if want.is_empty() || have.is_empty() {
        return Some(0);
    }
    let shared = want.iter().any(|w| have.iter().any(|h| same_artist(w, h)));
    if !shared {
        return None;
    }
    Some(match want == have {
        true => ARTIST_EXACT,
        false => ARTIST_SHARED,
    })
}

/// The credited artists, each as its own list of words, so containment is checked on whole
/// names: "Queen" is inside "Queensrÿche" as text and is not one of its artists.
fn artist_names(value: &str) -> HashSet<Vec<String>> {
    ARTIST_SEPARATORS
        .split(&normalize(value))
        .map(|name| {
            WORD_SPLIT
                .split(name)
                .map(|part| NON_ALNUM.replace_all(part, "").into_owned())
                .filter(|word| word.chars().count() > 1)
                .collect::<Vec<_>>()
        })
        .filter(|words| !words.is_empty())
        .collect()
}

fn same_artist(a: &[String], b: &[String]) -> bool {
    run_of(a, b) || run_of(b, a)
}

/// Whether `outer` contains `inner` as a run of whole words.
fn run_of(outer: &[String], inner: &[String]) -> bool {
    if inner.is_empty() || inner.len() > outer.len() {
        return false;
    }
    (0..=outer.len() - inner.len()).any(|at| outer[at..at + inner.len()] == *inner)
}

// ── Duration ─────────────────────────────────────────────────────────────────

/// Points for the runtimes agreeing, or `None` when they are too far apart to be the same
/// recording. The strongest signal available, and the one that catches what titles cannot: the
/// ten-minute loop, the album-side upload, the snippet.
fn duration_score(wanted: Option<u64>, got: Option<u64>, allow_video_drift: bool) -> Option<i64> {
    let (Some(wanted), Some(got)) = (wanted, got) else {
        return Some(0);
    };
    let drift = wanted.abs_diff(got);
    if drift > DURATION_LIMIT_SEC {
        if allow_video_drift && drift <= VIDEO_DURATION_LIMIT_SEC {
            return Some(0);
        }
        return None;
    }
    Some(match drift <= DURATION_TIGHT_SEC {
        true => DURATION_TIGHT,
        false => DURATION_LOOSE,
    })
}

// ── Album ────────────────────────────────────────────────────────────────────

/// Take markers carried by the release rather than the row ("KALYANI (Remix)" as the album of
/// a plain-titled "KALYANI"), read with the same parser so the same vocabulary applies.
fn album_version_markers(album: &str) -> BTreeSet<String> {
    if album.trim().is_empty() {
        BTreeSet::new()
    } else {
        parse_title(album, "").versions
    }
}

/// Exact release agreement, stronger than a tight-versus-loose runtime, so the known album
/// beats a different release whose duration happens to be one second closer. A disagreement is
/// not a veto: compilations and reissues can contain the same master.
fn album_score(wanted: Option<&str>, got: &str) -> i64 {
    let Some(want) = album_key(wanted) else {
        return 0;
    };
    let Some(have) = album_key(Some(got)) else {
        return 0;
    };
    match want == have {
        true => ALBUM_EXACT,
        false => 0,
    }
}

/// Punctuation, spacing and a trailing edition label are catalogue formatting, not release
/// identity.
fn album_key(value: Option<&str>) -> Option<String> {
    let text = normalize(value.unwrap_or_default());
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut text = text.to_owned();
    for _ in 0..BRACKET_PASSES {
        if !BRACKETED.is_match(&text) {
            break;
        }
        text = BRACKETED.replace_all(&text, " ").into_owned();
    }
    let words: String = WORD_SPLIT
        .split(&text)
        .map(|part| NON_ALNUM.replace_all(part, "").into_owned())
        .filter(|word| !word.is_empty() && !ALBUM_NOISE_WORDS.contains(word.as_str()))
        .collect();
    (!words.is_empty()).then_some(words)
}

// ── Context ──────────────────────────────────────────────────────────────────

/// A nudge when both listings mention the same film or album in their asides.
fn context_score(wanted: &TitleParts, got: &TitleParts) -> i64 {
    match wanted.context.iter().any(|word| got.context.contains(word)) {
        true => CONTEXT_SHARED,
        false => 0,
    }
}

// ── Romaji ───────────────────────────────────────────────────────────────────

/// Hepburn transliteration for kana; anything else (kanji, Latin, digits) passes through
/// untouched. Bridges コイコガレ and koikogare — but deliberately not kanji (no algorithmic
/// reading) nor English loans (ドア reads "doa", not "door").
fn romaji_of(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0usize;
    let mut geminate = false;
    let mut last_vowel: Option<char> = None;
    while i < chars.len() {
        let mut character = chars[i];
        if ('ァ'..='ヶ').contains(&character) {
            character = char::from_u32(character as u32 - 0x60).unwrap_or(character);
        }
        match character {
            'っ' => {
                geminate = true;
                i += 1;
            }
            'ー' => {
                if let Some(vowel) = last_vowel {
                    out.push(vowel);
                }
                i += 1;
            }
            'ん' => {
                let peek = chars.get(i + 1).and_then(|&next| {
                    let next = match ('ァ'..='ヶ').contains(&next) {
                        true => char::from_u32(next as u32 - 0x60).unwrap_or(next),
                        false => next,
                    };
                    kana_romaji(next).and_then(|base| base.chars().next())
                });
                let roma = match matches!(peek, Some('b') | Some('m') | Some('p')) {
                    true => "m",
                    false => "n",
                };
                emit(&mut out, roma, &mut geminate, &mut last_vowel);
                i += 1;
            }
            _ => match kana_romaji(character) {
                None => {
                    out.push(character);
                    last_vowel = None;
                    geminate = false;
                    i += 1;
                }
                Some(base) => {
                    let next = chars
                        .get(i + 1)
                        .map(|&next| match ('ァ'..='ヶ').contains(&next) {
                            true => char::from_u32(next as u32 - 0x60).unwrap_or(next),
                            false => next,
                        });
                    let small = next.and_then(|next| small_y(next).or_else(|| small_v(next)));
                    match small {
                        Some(small) => {
                            // sh/ch/j already carry the glide: しゃ is "sha", not "shya".
                            let cons = CONS_OVERRIDES
                                .iter()
                                .find(|(kana, _)| base.starts_with(kana))
                                .map(|(_, cons)| *cons)
                                .unwrap_or(&base[..base.len() - 1]);
                            let glide = match cons == "sh" || cons == "ch" || cons == "j" {
                                true => small.trim_start_matches('y'),
                                false => small,
                            };
                            emit(
                                &mut out,
                                &format!("{cons}{glide}"),
                                &mut geminate,
                                &mut last_vowel,
                            );
                            i += 2;
                        }
                        None => {
                            emit(&mut out, base, &mut geminate, &mut last_vowel);
                            i += 1;
                        }
                    }
                }
            },
        }
    }
    out
}

/// Appends one mora's romaji, doubling its leading consonant after a small tsu.
fn emit(out: &mut String, roma: &str, geminate: &mut bool, last_vowel: &mut Option<char>) {
    let mut roma = roma.to_owned();
    if *geminate {
        *geminate = false;
        let doubled = match roma.starts_with("ch") {
            true => format!("t{roma}"),
            false => match roma.starts_with("sh") {
                true => format!("s{roma}"),
                false => match roma.starts_with("ts") {
                    true => format!("t{roma}"),
                    false => format!("{}{roma}", roma.chars().next().unwrap_or_default()),
                },
            },
        };
        roma = doubled;
    }
    out.push_str(&roma);
    *last_vowel = roma
        .chars()
        .rev()
        .find(|character| "aeiou".contains(*character));
}

/// The hiragana a katakana character folds to, and the kana-to-romaji table read by
/// [`romaji_of`].
fn kana_romaji(character: char) -> Option<&'static str> {
    Some(match character {
        'あ' => "a",
        'い' => "i",
        'う' => "u",
        'え' => "e",
        'お' => "o",
        'か' => "ka",
        'き' => "ki",
        'く' => "ku",
        'け' => "ke",
        'こ' => "ko",
        'が' => "ga",
        'ぎ' => "gi",
        'ぐ' => "gu",
        'げ' => "ge",
        'ご' => "go",
        'さ' => "sa",
        'し' => "shi",
        'す' => "su",
        'せ' => "se",
        'そ' => "so",
        'ざ' => "za",
        'じ' => "ji",
        'ず' => "zu",
        'ぜ' => "ze",
        'ぞ' => "zo",
        'た' => "ta",
        'ち' => "chi",
        'つ' => "tsu",
        'て' => "te",
        'と' => "to",
        'だ' => "da",
        'ぢ' => "ji",
        'づ' => "zu",
        'で' => "de",
        'ど' => "do",
        'な' => "na",
        'に' => "ni",
        'ぬ' => "nu",
        'ね' => "ne",
        'の' => "no",
        'は' => "ha",
        'ひ' => "hi",
        'ふ' => "fu",
        'へ' => "he",
        'ほ' => "ho",
        'ば' => "ba",
        'び' => "bi",
        'ぶ' => "bu",
        'べ' => "be",
        'ぼ' => "bo",
        'ぱ' => "pa",
        'ぴ' => "pi",
        'ぷ' => "pu",
        'ぺ' => "pe",
        'ぽ' => "po",
        'ま' => "ma",
        'み' => "mi",
        'む' => "mu",
        'め' => "me",
        'も' => "mo",
        'や' => "ya",
        'ゆ' => "yu",
        'よ' => "yo",
        'ら' => "ra",
        'り' => "ri",
        'る' => "ru",
        'れ' => "re",
        'ろ' => "ro",
        'わ' => "wa",
        'を' => "wo",
        'ん' => "n",
        'ゔ' => "vu",
        'ぁ' => "a",
        'ぃ' => "i",
        'ぅ' => "u",
        'ぇ' => "e",
        'ぉ' => "o",
        'ゃ' => "ya",
        'ゅ' => "yu",
        'ょ' => "yo",
        'ゎ' => "wa",
        'ゕ' => "ka",
        'ゖ' => "ke",
        _ => return None,
    })
}

/// The small kana that mark a glide after a consonant.
fn small_y(character: char) -> Option<&'static str> {
    Some(match character {
        'ゃ' => "ya",
        'ゅ' => "yu",
        'ょ' => "yo",
        _ => return None,
    })
}

/// The small vowels that lengthen a consonant into a mora of their own.
fn small_v(character: char) -> Option<&'static str> {
    Some(match character {
        'ぁ' => "a",
        'ぃ' => "i",
        'ぅ' => "u",
        'ぇ' => "e",
        'ぉ' => "o",
        _ => return None,
    })
}

/// Consonants that do not simply take the vowel's own letter.
static CONS_OVERRIDES: LazyLock<[(&'static str, &'static str); 6]> = LazyLock::new(|| {
    [
        ("shi", "sh"),
        ("chi", "ch"),
        ("tsu", "ts"),
        ("fu", "f"),
        ("ji", "j"),
        ("zu", "z"),
    ]
});

// ── Characters ───────────────────────────────────────────────────────────────

/// Whether `character` belongs to one of the CJK blocks titles may be written in.
fn is_cjk(character: char) -> bool {
    matches!(character,
        '\u{3040}'..='\u{309f}'   // hiragana
        | '\u{30a0}'..='\u{30ff}' // katakana
        | '\u{4e00}'..='\u{9fff}' // CJK unified ideographs
        | '\u{ac00}'..='\u{d7af}' // hangul syllables
    )
}

/// The browser-comparable halves of a search query.
const LATIN_QUERY_CHARS: &str = "abcdefghijklmnopqrstuvwxyz0123456789 ";

// ── Patterns ─────────────────────────────────────────────────────────────────

static BRACKETED: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[(\[]([^()\[\]]*)[)\]]").expect("the bracket pattern"));
static DASH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s+[-–—|]+\s+").expect("the dash pattern"));
static FEATURING: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(feat|ft|featuring|with)\b.*").expect("the feat pattern"));
static PT_BR_FEATURING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(\bpart\.\s+|\bpart\s+(?:esp\.?|especial)\b)([^)\]]*[)\]]?)")
        .expect("the Portuguese feat pattern")
});
/// `~` and the wave dash join words in Japanese titles ("真夜中のドア〜stay").
static WORD_SPLIT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\s.·~〜]+").expect("the word pattern"));
/// What survives into identity: any language's letters and digits.
static NON_ALNUM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^\p{L}\p{N}]").expect("the alphanumeric pattern"));
static LATIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-z0-9]").expect("the Latin pattern"));
static CJK_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[「『【〈《〔［｛]").expect("the opening bracket pattern"));
static CJK_CLOSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[」』】〉》〕］｝]").expect("the closing bracket pattern"));
/// Whitespace and punctuation that never separate CJK words, plus the joiners.
static CJK_STRIP: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\s[:punct:]・～〜ー-]+").expect("the CJK strip pattern"));
static ARTIST_SEPARATORS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s*(?:[,&/;·|、，×]|\band\b|\bx\b|\bvs\.?\b|\bfeat\.?\b|\bft\.?\b|\bfeaturing\b|\bwith\b)\s*")
        .expect("the artist separator pattern")
});

// ── Vocabulary ───────────────────────────────────────────────────────────────

/// What makes a listing a different recording rather than a different listing of the same one.
/// A title carrying one of these on one side only is refused outright.
static VERSION_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "remix",
        "remixes",
        "rmx",
        "refix",
        "flip",
        "bootleg",
        "mashup",
        "medley",
        "live",
        "concert",
        "unplugged",
        "acoustic",
        "instrumental",
        "karaoke",
        "vocals",
        "vocal",
        "acapella",
        "acappella",
        "backing",
        "stems",
        "stem",
        "cover",
        "demo",
        "reprise",
        "remake",
        "rework",
        "extended",
        "edit",
        "version",
        "mix",
        "dub",
        "vip",
        "session",
        "sessions",
        "sped",
        "slowed",
        "reverb",
        "nightcore",
        "lofi",
        "orchestral",
        "symphonic",
        "part",
        "pt",
        "chapter",
        "カバー",
        "カラオケ",
        "リミックス",
        "ライブ",
        "アコースティック",
        "インスト",
        "バージョン",
        "歌ってみた",
        "踊ってみた",
        "弾いてみた",
        "翻唱",
        "现场",
        "混音",
        "커버",
        "라이브",
        "리믹스",
    ]
    .into_iter()
    .collect()
});

/// The version words that are a take even printed bare at the end of a title. The rest are
/// real title words often enough that reading them as a take would let a cover stand in.
static TRAILING_TAKES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "instrumental",
        "karaoke",
        "acapella",
        "acappella",
        "remix",
        "rmx",
        "nightcore",
    ]
    .into_iter()
    .collect()
});

/// Takes whose absence from the audio is the point, applied from the album in both directions.
static NOVOCAL_TAKES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "instrumental",
        "karaoke",
        "acapella",
        "acappella",
        "backing",
        "stems",
        "stem",
    ]
    .into_iter()
    .collect()
});

/// Asides that read like a version and describe the ordinary release.
static NEUTRAL_SEGMENTS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "albumversion",
        "originalversion",
        "originalmix",
        "singleversion",
        "radioversion",
        "radioedit",
        "stereoversion",
        "monoversion",
        "studioversion",
        "fullversion",
        "standardversion",
        "explicitversion",
        "deluxeversion",
        "originaltrack",
    ]
    .into_iter()
    .collect()
});

/// Where YouTube Music prints a type instead of an artist: "… — Música".
static YOUTUBE_TYPE_LABELS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "música", "musica", "vídeo", "video", "song", "album", "álbum", "artista", "artist",
    ]
    .into_iter()
    .collect()
});

/// Packaging words, worth nothing as a tie-break because everything has them.
static NOISE_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "official",
        "video",
        "audio",
        "lyrics",
        "lyric",
        "lyrical",
        "visualizer",
        "song",
        "songs",
        "full",
        "music",
        "the",
        "and",
        "from",
        "feat",
        "ft",
        "featuring",
        "with",
        "new",
        "latest",
        "free",
        "download",
        "remaster",
        "remastered",
        "explicit",
        "clean",
        "bonus",
        "track",
        "deluxe",
        "original",
        "album",
        "single",
        "hd",
        "hq",
        "4k",
        "mp3",
    ]
    .into_iter()
    .collect()
});

static ALBUM_NOISE_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "album",
        "deluxe",
        "edition",
        "expanded",
        "remaster",
        "remastered",
        "version",
        "explicit",
        "clean",
        "bonus",
        "anniversary",
    ]
    .into_iter()
    .collect()
});

/// Trailing labels an upload hangs on a title with no brackets to hold them.
static TRAILING_NOISE: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        "song",
        "songs",
        "video",
        "audio",
        "lyrics",
        "lyric",
        "lyrical",
        "official",
        "full",
        "hd",
        "hq",
        "4k",
        "mp3",
        "ost",
        "soundtrack",
    ]
    .into_iter()
    .collect()
});

/// Dropped from the core so that "Jack and Jill" and "Jack & Jill" are one title.
static JOINING_WORDS: LazyLock<HashSet<&'static str>> =
    LazyLock::new(|| ["and"].into_iter().collect());

// ── Weights ──────────────────────────────────────────────────────────────────

/// Everything that reaches scoring has already matched on title and version.
const BASE: i64 = 100;
const ARTIST_EXACT: i64 = 25;
const ARTIST_SHARED: i64 = 10;
/// A penalty, not a pass: any candidate whose credit genuinely agrees beats it by at least 25.
const CREDITS_DISAGREE: i64 = -30;
/// How exactly two runtimes must agree before that stands in for a shared credit.
const CREDIT_OVERRIDE_SEC: u64 = 2;
const DURATION_TIGHT: i64 = 40;
const DURATION_LOOSE: i64 = 15;
const ALBUM_EXACT: i64 = 35;
const CONTEXT_SHARED: i64 = 20;
/// Within this many seconds is the same master, allowing for trimmed silence.
const DURATION_TIGHT_SEC: u64 = 3;
/// Past this, two tracks sharing a title are not sharing a recording.
const DURATION_LIMIT_SEC: u64 = 30;
/// Visual intros and outros can make a video substantially longer than its audio master.
const VIDEO_DURATION_LIMIT_SEC: u64 = 90;

const BRACKET_PASSES: usize = 3;
const DASH_PASSES: usize = 3;

#[cfg(test)]
mod tests {
    use super::*;

    fn row(title: &str, artist: &str, duration: u64) -> AddonTrack {
        AddonTrack {
            id: "x".into(),
            title: title.into(),
            artist: artist.into(),
            duration: Some(duration as f64),
            ..AddonTrack::default()
        }
    }

    fn target(title: &str, artist: &str, duration: u64) -> Target {
        Target {
            title: title.into(),
            artist: artist.into(),
            duration_sec: Some(duration),
            album: None,
            is_video: false,
        }
    }

    /// The shape the user's Fujii Kaze tracks come in: a Japanese title, a dash, then the
    /// romaji the catalogues file it under. The rows are the live ones Aurix and Gobuz answer
    /// that query with.
    #[test]
    fn a_latin_tail_stands_in_for_a_japanese_head() {
        assert_eq!(
            searchable_title("罪の香り - Tsumi No Kaori", "Fujii Kaze"),
            "tsumi no kaori"
        );
        let wanted = Target {
            title: "罪の香り - Tsumi No Kaori".into(),
            artist: "Fujii Kaze".into(),
            duration_sec: Some(214),
            album: Some("HELP EVER HURT NEVER".into()),
            is_video: false,
        };
        assert!(
            queries(&wanted)
                .first()
                .is_some_and(|q| q == "tsumi no kaori fujii kaze")
        );
        let mut aurix = row("Tsumi No Kaori", "Fujii Kaze", 214);
        aurix.album = "HELP EVER HURT NEVER".into();
        assert!(score(&aurix, &wanted).is_some());
        // Gobuz files the same recording under the Japanese title
        assert!(score(&row("罪の香り", "Fujii Kaze", 214), &wanted).is_some());
    }

    /// A kana title gets the romaji query, so a row filed under the other script is still
    /// found.
    #[test]
    fn a_kana_title_is_also_asked_for_in_romaji() {
        let wanted = target("やさしさ", "Fujii Kaze", 240);
        assert!(queries(&wanted).iter().any(|q| q == "yasashisa"));
        assert!(score(&row("Yasashisa", "Fujii Kaze", 240), &wanted).is_some());
    }

    /// Packaging comes off, a take marker does not, and a different credit loses to the right
    /// one however exactly the runtime agrees.
    #[test]
    fn packaging_is_ignored_but_takes_are_not() {
        let wanted = target("Paniyon Sa (From \"Satyamev Jayate\")", "Atif Aslam", 233);
        assert_eq!(
            searchable_title("Paniyon Sa (From \"Satyamev Jayate\")", ""),
            "paniyon sa"
        );
        assert!(score(&row("Paniyon Sa", "Atif Aslam, Tulsi Kumar", 233), &wanted).is_some());
        assert!(score(&row("Paniyon Sa (Live)", "Atif Aslam", 233), &wanted).is_none());
        // an exact runtime stands in for a credit disagreement, penalised; a far one cannot
        assert!(score(&row("Paniyon Sa", "Someone Else", 233), &wanted).is_some());
        assert!(score(&row("Paniyon Sa", "Someone Else", 260), &wanted).is_none());
        let rows = [
            row("Paniyon Sa", "Someone Else", 233),
            row("Paniyon Sa", "Atif Aslam", 233),
        ];
        assert_eq!(ranked(&rows, &wanted)[0].artist, "Atif Aslam");
    }

    /// The wrong recording cannot sneak in on a shared name.
    #[test]
    fn duration_and_artist_veto_without_a_shared_credit() {
        let wanted = target("KALYANI", "Someone", 200);
        assert!(score(&row("KALYANI (Remix)", "Someone", 200), &wanted).is_none());
        assert!(score(&row("KALYANI", "Another Artist", 260), &wanted).is_none());
    }
}
