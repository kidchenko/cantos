# Assets

`icon.svg` is the source of truth for the app mark. The rasters beside it are
generated from it, but they are **committed, not built**: nothing in the build
renders SVG, and `build.rs`/`ui/mod.rs` embed the raster files directly.

| file | used by | notes |
|---|---|---|
| `icon.svg` | nothing at runtime | the source; edit this one |
| `icon.ico` | `build.rs`, for the exe's Windows resource, and the Inno installer | 8 sizes, 16 to 256, PNG-compressed |
| `icon-256.png` | the README, and the Chocolatey `iconUrl` | |
| `tray-64.rgba` | `src/ui/mod.rs`, via `include_bytes!` | raw RGBA, **must be exactly 16384 bytes** (64 × 64 × 4) |
| `app.manifest` | `build.rs` | PerMonitorV2 DPI awareness; not generated |

## Regenerating

With [ImageMagick](https://imagemagick.org) on PATH, from the repository root:

```powershell
# 256px PNG
magick -background none assets\icon.svg -resize 256x256 assets\icon-256.png

# Tray icon: raw RGBA, no header. The byte count is load-bearing:
# ui/mod.rs hands these bytes straight to Icon::from_rgba as 64x64.
magick -background none assets\icon.svg -resize 64x64 -depth 8 RGBA:assets\tray-64.rgba
```

Check the tray file before committing it; a wrong size is a startup panic, not
a compile error:

```powershell
(Get-Item assets\tray-64.rgba).Length   # must be 16384
```

## Do not regenerate `icon.ico` with ImageMagick

`-define icon:auto-resize=...` writes BMP-encoded entries rather than
PNG-compressed ones, and the result is far larger than what is committed:

| command | size |
|---|---|
| committed `icon.ico` | **16 KB** |
| `auto-resize=64,48,32,16` | 32 KB |
| `auto-resize=128,64,48,32,16` | 100 KB |
| `auto-resize=256,128,64,48,32,16` | 370 KB |

The icon is embedded in the executable as a Windows resource, so that lands
directly on the shipped binary, and CI fails the build above 3 MB. If the mark
ever changes, produce the `.ico` with a tool that keeps PNG compression at
every size, then confirm it still reports eight entries:

```powershell
magick identify assets\icon.ico     # expect 16,20,24,32,48,64,128,256, all PNG
```

## The mark

A dark rounded tile with one corner lit: the app's whole idea in one shape.
The quarter-disc radius and the gradient radius are both `40` in the 64-unit
viewBox, so the colour sweep ends exactly where the lit corner does.

The settings windows draw the same mark in code (`draw_mark` in
`src/ui/window_*.rs`) rather than loading this file, so that they have no
external asset to fetch. It is a deliberately simplified copy: flat `#6ea8fe` instead
of the gradient. If you restyle the icon, those copies need the same edit.
