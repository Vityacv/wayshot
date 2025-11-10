# HDR Enhancements in This Branch

This document summarises the HDR-related additions made to `wayshot` and the recommended workflow for capturing HDR-ready screenshots that also produce SDR-friendly tonemapped copies.

## CLI Additions

### `--embed-hdr-icc`

Adds a minimal HDR metadata chunk (PNG `cICP`) when the compositor advertises a 10-bit wl_shm format. The saved image retains 16-bit pixels so HDR viewers (eg. mpv) display it correctly.

### `--tone-map-file <PATH>`

Optionally writes a tonemapped SDR copy alongside the HDR capture. The extra file uses the extension to choose the encoder (PNG/JPEG/WebP). The tone mapping is applied inside wayshot using libplacebo-like settings; upstream still assumes PQ + BT.2020.

### `--geometry-str <GEOMETRY>`

New flag that accepts a geometry string similar to `slurp` output (`"x,y widthxheight"`). This bypasses the interactive selection and feeds the specified rectangle into `wayshot`.

## Library Changes

### 10-bit SHM Preservation

The SHM helper now recognises wl_shm `Xrgb2101010`/`Abgr2101010` formats and promotes them to 16-bit RGB(A) without truncation. That allows the PNG path to keep HDR precision instead of collapsing to 8-bit.

### DMA-BUF Export Updates

When DMA-BUF is available, tone-mapped data stays on the GPU, preserving the compositor's format until the client (or script) wants to read it back.

### Region Capture Hook

A convenience wrapper in `WayshotConnection` allows screenshotting a `LogicalRegion` directly (used by the `--geometry-str` support).

## Config Recommendation

To avoid accidental auto-generated PNGs when piping to ffmpeg, the base config can be customised:

```
[base]
file   = false
stdout = true
```

## Cropped vs. Full-Frame Tone-Mapping

Tone mapping behaves differently depending on how much of the scene the algorithm sees. When you run the compositor overlay (`--geometry`) the SHM path truncates the pixels to 8-bit before we ever reach HDR processing, which produces washed-out results. Two alternate workflows solve this:

1. **Full frame → tone map → crop**  
   Capture the entire HDR output (`--output DP-3 --embed-hdr-icc -`), tone-map the full frame via libplacebo, and only then crop. This mimics mpv’s HDR → SDR pipeline. The helper script `~/bin/wayshot-region-webp` does exactly that, saving `/tmp/screen.webp` with no intermediate HDR file.

2. **Full frame → tone map → crop (manual one-liner)**  
   Similar to the script but inline, useful for quick tests or different formats.

Tonemapping a full frame first provides the same “context” (peak brightness, average luminance, etc.) the player sees. Cropping after tone mapping avoids the clamped results that come from the older SHM region selection.

Even if wlroots/Sway exposes HDR metadata (primaries, PQ/HLG flags, mastering data), region captures almost always go through a compositor overlay that down-converts to 8-bit. Full-frame capture → tone-map → crop remains the only way to preserve highlight detail until direct HDR region captures exist.

Other platforms already do this: Windows’s HDR screenshot APIs grab the full swapchain before tone mapping, and macOS still emits SDR when using the built-in selector. HDR capture utilities on both platforms therefore capture the whole frame first, then convert or crop. The helper script mirrors that approach on wlroots.

## Example Workflow

* One-liner for HDR → SDR WebP (full capture → tone-map → crop):

  ```
  cd /tmp && read out x y w h <<<"$(slurp -f '%o %x %y %w %h')" &&
  read ox oy <<<"$(swaymsg -t get_outputs | jq -r --arg out "$out" '.[] | select(.name==$out) | "\(.rect.x) \(.rect.y)"')" &&
  cx=$((x - ox)); cy=$((y - oy)) &&
  [[ $cx -lt 0 ]] && { w=$((w + cx)); cx=0; }
  [[ $cy -lt 0 ]] && { h=$((h + cy)); cy=0; } &&
  ~/proj/wayshot/target/release/wayshot --log-level error --output "$out" --embed-hdr-icc - |
    ffmpeg -y -f png_pipe -i - \
      -vf "libplacebo=tonemapping=bt.2390:color_primaries=bt709:color_trc=bt709:colorspace=bt709,crop=${w}:${h}:${cx}:${cy}" \
      -c:v libwebp -lossless 1 /tmp/screen.webp
  ```

* The helper script `~/bin/wayshot-region-webp` bundles the same logic, keeping `/tmp/screen.webp` up-to-date and showing a notification.

## Future Upstream Work (optional)

`HDR_METADATA.md` outlines how wlroots/Sway could expose compositor HDR metadata via screencopy. Once upstream surfaces primaries/transfer function/mastering data, wayshot can embed accurate metadata without guessing BT.2020 + PQ.
