# Performance findings

Last updated: 2026-11-19

This file records the results of the overnight optimization pass so later work does not have to
rediscover them. The hard rule for every performance change is that Sonora's appearance, motion,
and animation quality must remain unchanged. A cheaper rendering path is only acceptable when it
is visually equivalent.

## Host and rollback point

- Host: aarch64 Apple M1, Asahi Linux, 2560x1600, Hyprland 0.57.
- Graphics: Vulkan on the "Apple M1 (G13G B1)" adapter.
- Baseline release: tag `perf-baseline`, commit `843477a`.
- The checkout is 18 GB and must remain below 30 GB. Never run `cargo clean`.
- Build with `CARGO_BUILD_JOBS=2 cargo build --release --package sonora`.
- The installed binary is `~/.local/opt/sonora/usr/bin/sonora`; the original release remains at
  `~/.local/opt/sonora/usr/bin/sonora.appimage` for rollback.

## Result

A controlled comparison used the same host, account, queue, and playback path:

| Build | RSS | Heap | File-backed memory |
| --- | ---: | ---: | ---: |
| Original release (`perf-baseline`) | 383 MiB | 200 MiB | 184 MiB |
| Optimized release | 278 MiB | 102 MiB | about 176 MiB |

That is roughly 105 MiB less resident memory and 98 MiB less heap in the comparable run. The
optimized process can fall much lower after its temporary YouTube PO-token web page closes: the
latest installed-build observation settled at 91.8 MiB RSS, 74.5 MiB heap, and 16.8 MiB
file-backed memory. The 278 MiB and 91.8 MiB figures describe different phases and must not be
compared directly.

The remaining large allocation in the comparable run is the YouTube PO-token web page, not the
home feed:

- PO-token page: about 89 MiB (70 MiB file-backed and 21 MiB heap).
- New home feed: about 14 MiB end to end. In an isolated probe it peaked at 17.6 MiB and retained
  6.9 MiB after `malloc_trim`.
- QuickJS solver cache: about 7 MiB.

YouTube now binds PO tokens per video, so the web page cannot be removed without risking
playback. Reusing or pooling that page may reduce peak memory, but only if token binding and
playback remain exactly as reliable.

## CPU hotspot: the ambient field

The ambient drift is the only large sustained CPU cost found. In the fullscreen or Home view it
used about 37% of one CPU core. The visualizer was not responsible: disabling it left the same
37% measurement.

The cost is in the per-frame ambient drawing:

- Five drifting blobs, each built from 24 concentric discs, means 120 discs rebuilt and painted
  on every frame.
- The discs and their blurred layer are drawn at one-quarter resolution, but they are still
  rebuilt for the whole window each frame.
- The ambient already requests 30 frames per second rather than the display's 60, in commit
  `3a52aa3`. Do not reduce this further without explicit approval, because it changes the motion.

Control measurements:

| Configuration | CPU |
| --- | ---: |
| Ambient drift enabled | about 37% of one core |
| `ambient_motion` disabled | about 1.25% of one core |
| Diagnostic build with the discs removed | about 1.42% of one core |

The diagnostic removal was not shipped: it removed the discs' visible radial falloff and was
reverted. It was only used to prove that the discs, not the visualizer, account for the cost.

### Changes rejected because they alter the look or feel

- Removing or reducing the 24 discs per blob.
- Replacing the stacked discs with a different shadow construction.
- Slowing the drift or reducing the frame rate below 30 fps.
- Changing the blur radius, downscale factor, opacity ramps, or edge fades.
- Removing the blurred ambient layer.

These may reduce CPU, but each changes the picture or the character of the animation and is
therefore out of bounds without the user's explicit approval.

### Look-neutral opportunities still worth trying

1. **Suspend the ambient timer while the window is hidden.** GPUI exposes
   `Window::is_visible()` and `observe_window_visibility`. The drift can stop scheduling frames
   while hidden and resume when visible. The elapsed-time-based drift already catches up after a
   gap, so the visible animation need not change. This is the safest remaining CPU improvement.
2. **Avoid repainting an entirely hidden or fully covered window.** `is_visible()` covers the
   hidden case; compositor-level occlusion detection would be needed for full coverage and is more
   invasive.
3. **Reduce the PO-token page's peak cost.** Investigate reusing a webview session or page while
   preserving per-video token binding. This must be tested against actual YouTube playback before
   being considered safe.
4. **Only as a last resort, move the ambient field to a shader.** It could preserve the same
   mathematical falloff while avoiding 120 DOM elements, but small GPU rounding differences can
   change the picture. It requires pixel-level comparison and explicit visual approval.

## Changes already shipped

The optimization branch includes:

- Vulkan-first GPU initialization, excluding software ICDs from the process.
- glibc arena tuning, `mallinfo2`/`malloc_trim` heap trimming, and an earlier trim threshold.
- An idle audio engine that stops polling and a spectrum analyzer that sleeps while unused.
- Mixer and engine-refill pacing, less frequent resolver writes, and lazy mix resolution.
- Cached page themes, favorite-track indexing, genre artwork caching, and cheaper soft artwork
  decoding.
- Loading-animation frame caps, off-screen queue prefetch pacing, and visualizer rendering only
  when its levels move.
- A prepared QuickJS solver per player build.
- The approved Home feed change: listening-history recents followed by YouTube's own picks.
- The approved R&B genre fix: the "Music videos" shelf is left out of audio-only genre pages.

The full commit sequence is available with:

```sh
git log --oneline --reverse perf-baseline..HEAD
```

## Measurement notes

- The app logs its RSS, heap, GPU, and file-backed footprint every ten seconds when debug logging
  is enabled. Look for `memory:` lines in `~/.local/state/sonora/sonora.log`.
- CPU was sampled over a 15-second interval with the helper at `/tmp/opencode/cpu.py`.
- `perf`, `strace`, `heaptrack`, and `valgrind` were unavailable on the host.
- Attaching `gdb` detached the app from PulseAudio, so it was not used for interactive checks.
- Hyprland 0.57's Lua configuration broke `hyprctl dispatch`, preventing reliable workspace and
  focus changes during automated checks.

## Current release state

As of 2026-11-19, the installed release binary is built from the restored source with
`DISCS = 24`. Its SHA-256 matches `target/release/sonora`. The checkout is 18 GB, the working
tree is clean, and the original `sonora.appimage` is untouched.
