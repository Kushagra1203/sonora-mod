# Patches

The GPUI fork in `vendor/gpui` is not tracked by this repository, so the changes this fork
carries are kept here as patches. Apply one from the root of `vendor/gpui`:

```sh
git apply ../../patches/gpui-vulkan-first.patch
```

## `gpui-vulkan-first.patch`

The window is drawn with Vulkan, but the instance asked for OpenGL as well, and asking for
OpenGL makes the process load Mesa's software rasteriser and its LLVM: about ten megabytes
of resident memory for a driver that is never the one that draws. The instance now offers
Vulkan alone, and OpenGL is asked for when Vulkan cannot make a surface for the window, so
a machine without a Vulkan driver still draws.
