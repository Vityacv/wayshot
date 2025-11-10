# HDR Metadata Support for wlroots/sway Screencopy

## Summary

The current `zwlr_screencopy_v1` workflow only transfers raw pixels. Clients
receiving 10-bit HDR buffers have no way to discover the colour primaries,
transfer function, or mastering information that the compositor applied. As a
result, third-party tools must guess metadata (e.g. BT.2020 + PQ) before
encoding or displaying the capture, which is fragile and inaccurate on other
setups.

## Proposed Enhancement

Expose colour characteristics alongside screencopy frames when an output is
running in HDR mode. Clients should be able to retrieve at least:

1. Colour primaries (BT.2020, P3, etc.)
2. Transfer function (PQ / HLG / gamma)
3. Matrix coefficients (BT.2020 non-constant, etc.)
4. Optional mastering display information
5. Optional content light level information

## Potential Approaches

1. **Extend `zwlr_screencopy_v1`:**
   - Introduce new events (e.g. `color_description`) emitted before `buffer_done`.
   - Emit data based on the compositor’s output state (`wlr_output_image_description`,
     HDR metadata negotiated with DRM/Vulkan).

2. **Adopt `ext_image_capture_source_v1`:**
   - wlroots already has partial support for this protocol. Completing it would
     allow clients to bind a capture source and receive HDR metadata as defined
     by the protocol.

3. **Expose compositor colour state via a companion interface:**
   - Similar to `wp_viewporter` or `xdg_output`, provide a global that mirrors the
     current `wlr_output_state.image_description` and associated HDR metadata for
     each output.

## Implementation Notes

- Sway should populate `wlr_output_state.image_description` whenever HDR is enabled.
- The capture code in `types/wlr_screencopy_v1.c` needs access to that metadata.
- Consider fallbacks for SDR outputs to maintain compatibility.
- Coordinate with the Wayland community if extending the existing protocol.

## Benefits

- Clients can export captures with accurate HDR metadata (PNG `cICP`, AVIF/HEIF
  colour boxes, JPEG XL ICC profiles, etc.).
- Reduces guesswork and ensures consistent results across compositors.
- Opens the door for more sophisticated HDR workflows (recording, streaming,
  calibration utilities) on wlroots-based compositors.
