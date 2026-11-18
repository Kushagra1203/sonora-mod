# Patches

The forks in `vendor` are not tracked by this repository, so the changes they carry are kept
here as patches.

## `vendor/gpui`

Apply from the root of `vendor/gpui`:

```sh
git apply ../../patches/gpui-vulkan-first.patch
```

### `gpui-vulkan-first.patch`

The window is drawn with Vulkan, but the instance asked for OpenGL as well, and asking for
OpenGL makes the process load Mesa's software rasteriser and its LLVM: about ten megabytes
of resident memory for a driver that is never the one that draws. The instance now offers
Vulkan alone, and OpenGL is asked for when Vulkan cannot make a surface for the window, so
a machine without a Vulkan driver still draws.

## `vendor/ytmusic`

The crate comes from <https://github.com/sonorahq/ytmusic-rs> at the revision the lock file
pins. Fetch it and apply the patch from the root of `vendor/ytmusic`:

```sh
git clone https://github.com/sonorahq/ytmusic-rs vendor/ytmusic
cd vendor/ytmusic
git checkout d47b3ae
git apply ../../patches/ytmusic-player-cache.patch
```

### `ytmusic-player-cache.patch`

YouTube names a different player build in its widget script depending on who asks and when,
and the crate asks without a visitor, so a run meets a build the last run never saw. The
cache held one build under one name, so the run after a change had to prepare the new build
again: six seconds of processor time at startup, and the parser and the generator it needs
staying in memory for as long as the app runs, which came to seventy megabytes.

Each build now gets a file of its own, so a build that has been prepared once is never
prepared again, and the solvers of builds that are long gone are thrown away.
