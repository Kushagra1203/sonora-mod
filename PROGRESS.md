# Addon Provider — Working Notes

Goal: BitChord-style addon sources in Sonora — search/play FLAC through community addon HTTP
servers (the protocol BitChord documents), managed as a Sonora provider.

## State — 2026-11-02 (afternoon session)

**FEATURE WORKS END-TO-END. Verified by CLI-driven test (no clicks needed).**

Verified chain (sandbox profile `~/sonora-sandbox`, adds: Qobuz [Heisenbergg] + Lossless
Bridge seeded; 2 dead addons tested earlier and handled gracefully):

1. Provider registered — `sonora providers` lists `addons`.
2. `sonora provider addons` switches to it (guest mode; marker file written on first
   sign-in, kept in `~/.simpmusic`-equivalent `accounts/addons/active`).
3. `sonora search "Yo-Sea Someday"` returns hits from BOTH addons, priority-ordered,
   de-duplicated (Qobuz wins duplicates), each with an `addon://addon:{i}:{id}` link.
4. `sonora play addon://addon:2:ne_2648150478` → engine log:
   `playback: addon:2:ne_2648150478 started, flac 1511 kbps, 30.2 MiB`
5. `sonora current` → `state: playing`, position 1s → 18s advancing in real time.
6. Dead addons (bitchord-hires HTTP 402, eljohnzc timeout) fail per-addon with a redacted
   log line and the walk continues.

## Feedback round — 2026-11-02 (evening)

Three changes from the user's review of the UI:

1. **Addons no longer appear as a streaming service.** `MusicProvider::account_service()` was
   added (default `true`, addons `false`); `ProviderInfo.account_service` carries it, and both
   the settings accounts list and the login screen filter on it. The Addons page now owns a
   "Use addons / In use" row that switches to the provider (or starts its anonymous session).
2. **Addon order is visible and rearrangeable.** Rows carry their priority number and
   up/down buttons; the order is the search order (first addon to match a title+artist wins
   de-duplication). Track ids now carry a stable key derived from the addon URL
   (`addon:{md5(url)[..12]}:{id}`), so reordering does not break the queue/history; numeric
   ids from before this change still fall back to list position.
3. **Quality badge.** New `crates/views/src/shared/quality.rs` derives the badge from the
   engine's `TrackSignal.encoding`, not the requested level: FLAC/ALAC/PCM above 16-bit or
   48 kHz reads "Hi-Res Lossless"; otherwise "Lossless"; lossy streams read "Lossy" with the
   codec and bitrate in the fullscreen line (`AAC 256 kbps`, `Opus 160 kbps`, …). The player
   bar shows the pill next to the artist; the fullscreen player shows pill + depth/rate under
   the seek row. New icon `assets/icons/common/audio-lines.svg`.

Also fixed while here: ALAC from an addon was being labelled `Encoding::Aac`; it now reports
`Encoding::Alac { depth }` so the badge says Lossless instead of Lossy.

Verified: `cargo check -p views`, `cargo test -p music --lib addon::` (11 passed), full
`cargo build -p sonora`; sandbox restarted on the new binary with addons current and a
stable-key search id (`addon:e02e98ba179f:296344806`) playing.

## Lossless upgrade — 2026-11-02 night / 11-03

A track playing on any other provider now starts at once and becomes lossless a moment
later, BitChord-style:

- `music` side: `AddonUpgrade` (crates/music/src/addon/mod.rs) searches the enabled
  sources, resolves and verifies the stream (lossless, progressive, unencrypted) and
  caches it for the engine (`Shared::remember_stream`/`take_stream`).
- `state` side (crates/state/src/playback.rs): a shadow `addon_engine` loads the copy
  paused at the live position, seeks and plays it, then pauses the original; the queue,
  history and like state keep the original track. Commit on the addon's Seeked/Playing.
  Pause cancels a pending search or commits an in-flight swap held paused; seek, volume,
  output change and teardown are handled; a dead copy falls back to the original for the
  rest of that track. Setting `lossless_upgrade` (default on) with a toggle row on the
  Addon sources page.

Fixes from the user's testing (bracket titles stayed lossy; 40–50 s waits):

- `title_key` strips bracketed qualifiers and featuring credits before matching, so
  `Dracula (feat. JENNIE)` matches an addon's `Dracula`.
- All enabled addons are searched at once (`FuturesUnordered`) with a per-source 8 s cap;
  the first usable answer wins, so a dead or cold addon cannot hold a fast one back.
  Stream resolution capped at 12 s.
- The upgrade asks `Tier::HiRes` and tries hi-res rows before CD ones, so the badge
  reaches Hi-Res when the addon has it (`Quality::HiRes` addon playback asks HI_RES too).

Verified: `cargo check -p music`, `cargo test -p music --lib addon::` (11 passed), full
`cargo build -p sonora`. User reported upgrades working and the bracket-title songs
flipping on the new binary while changing tracks by hand.

### Round 2 — 2026-11-03

- **Version-aware matching.** A bracketed qualifier is dropped only when it is a credit or a
  note (feat., soundtrack, video tag). A qualifier naming a version — remix, extended,
  instrumental, live, radio edit, … — stays in the key, so a remix only matches the same
  remix, never the original. `title_key` in `crates/music/src/addon/mod.rs`; verified with a
  throwaway harness for the five user titles plus remix/instrumental/radio-edit cases.
- **Edit markers.** `- Explicit Ver.` / `- Clean` outside brackets are dropped from the key
  (unless the whole title is just that word).
- **Queue advance after a shadowed track ends.** Root cause: while the shadow played, the
  main engine had the next queue track queued as a *gapless join* behind the paused original.
  When the addon engine reported the end, the next load asked for it `seamless`, the engine
  took its "joined already" shortcut (no Stop, no Playing event), and resumed the paused
  original from the middle while the UI had moved to the next song. Fix: an end reported by
  the shadow engine advances with `Start::Skip` (plain load, Stop + fresh Play), and while the
  shadow plays the next track is only cached (`preload_next(at, false)`), never joined.
  `advance`/`segue_queue`/`extend_radio` now thread the `Start` through.

### Updating from upstream

The tree is a git checkout of `sonorahq/sonora` with all of this work **uncommitted** on
`main` (24 paths). Before any `git pull`/merge, commit it. The addon provider is mostly new
files (`crates/music/src/addon/`), but the lossless upgrade touches core
`crates/state/src/playback.rs`, `crates/music/src/lib.rs` and the settings views, so merges
that rework playback or settings may conflict. Nothing updates by itself: a source build
takes upstream only when the user merges it.

### What is left (user, ~2 min)

- **Addon sources look-test** (only thing needs eyes):
  Settings → Providers → **Addon sources** → the new **Upgrade to lossless** switch sits
  above the list; toggle it and confirm playback follows. The list itself: toggle a
  source, delete one, then **Add an addon** → paste a URL → it probes and adds it.
- **Queue advance check**: let a losslessly upgraded track play to its end with more songs
  queued — the next song should start from 0 with the clock moving, no next/previous dance.
  Repeat with `Seven - Explicit Ver. (feat. Latto)` for the edit-marker match.
- **Badge check** on the five tough titles: Dracula (feat. JENNIE), All The Stars (From
  "Black Panther: The Album"), like JENNIE (Extended Remix), Sunflower (Spider-Man: Into
  the Spider-Verse), Lose My Mind [From F1® The Movie] (feat. Doja Cat) — do they reach
  Lossless/Hi-Res, and roughly how many seconds in?
- Fresh addon links as old ones expire (paste them into that same screen).

### Files

- `crates/music/src/addon/` — wire.rs (protocol models + label readers), client.rs
  (30s connect / 60s search / 90s stream for cold-addon hosts), store.rs (addons.json),
  mod.rs (provider, api, admin API re-exported from `music` root), playback.rs
  (progressive FLAC; DASH refused with a message).
- `crates/music/src/live_tests/addons.rs` — live check vs the real servers:
  `SONORA_ADDON_URLS="a,b,c" cargo test -p music --lib live_tests::addons -- --ignored --nocapture`
- `crates/state/src/addons.rs` — settings entity (list, probe, toggle, delete).
- `crates/views/src/screens/settings/addons.rs` — the page + add dialog.
- Sandbox: `~/sonora-sandbox` (XDG-isolated profile), seeded with 3 addons, currently
  RUNNING as `~/sonora-mod/target/debug/sonora` with addons active (paused mid-play).

### Dev environment notes

- Build: `cd ~/sonora-mod && cargo build --package sonora -j 4`. Needs
  `LIBRARY_PATH=~/.local/lib` — handled by `.cargo/config.toml [env]` (dev symlink stands
  in for missing libxkbcommon-x11-devel until `sudo dnf install libxkbcommon-x11-devel`).
- Build time: incremental ~5-9 min when views/state change, ~30-50s for music-only.
- **Hard budget: the checkout must stay under 15 GB, ideally 12–13 GB (nearer 10 GB is fine).**
  Never `cargo clean`; keep the binary, `target/debug/deps` and the newest incremental
  generation per crate. On 2026-11-03 a cleanup took 17 GB → 9.9 GB by deleting the orphaned
  `.dwo` split-debug files (2.6 GB unique) and 45 stale incremental generations (5.2 GB);
  the build cache and `target/debug/sonora` were kept. `profile.dev` uses
  `split-debuginfo = "unpacked"`, so `.dwo` files accumulate again across full rebuilds and
  are the first thing to prune when the folder grows.
- `target/debug/sonora` talks IPC over `/run/user/1000/sonora-dev.sock`; the release
  AppImage uses `sonora.sock` — no collision.
- CLI drives everything: `sonora providers|provider|search|play|current|pause|queue…`

### Known v1 limits

- DASH (Tidal hi-res streams) refused with a readable error — additive next.
- `track(id)` only answers for tracks this session searched (in-memory rows); a restart
  forgets them (clear message: "search for this track again").
- No downloads/offline from addons yet (`allowDownloads` read but unused).
- Addon badge trusts the addon's depth/rate labels; the decoder reports the real sample rate
  but not bit depth, so a mislabelled "24-bit" would read Hi-Res.
