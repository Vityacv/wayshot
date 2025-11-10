<p align=center>
  <img src="https://git.sr.ht/~shinyzenith/wayshot/blob/main/docs/assets/wayshot.png" alt=wayshot width=60%>
  <p align=center>A native, blazing-fast 🚀🚀🚀 screenshot crate for wlroots based compositors such as sway and river.</p>

  <p align="center">
  <a href="./LICENSE.md"><img src="https://img.shields.io/github/license/waycrate/wayshot?style=flat-square&logo=appveyor"></a>
  <img src="https://img.shields.io/badge/cargo-v0.1.0-green?style=flat-square&logo=appveyor">
  </p>
</p>

# `libwayshot`

`libwayshot` is a convenient wrapper over the wlroots screenshot protocol that provides a simple API to take screenshots with.

# Example usage

```rust
use libwayshot::WayshotConnection;

let wayshot_connection = WayshotConnection::new()?;
let image_buffer = wayshot_connection.screenshot_all()?;
```

## Capturing DMA-BUF frames for HDR

If you need to capture the buffer exactly as advertised by the compositor (for example when HDR is
enabled and the output is using 10-bit formats), initialise the connection with DMA-BUF support and
use the new DMA capture helpers:

```rust,no_run
use libwayshot::{WayshotConnection, DMAFrameCopy};

let connection = WayshotConnection::from_connection_with_dmabuf(
    wayland_client::Connection::connect_to_env()?,
    "/dev/dri/renderD128",
)?;

let output = connection.get_all_outputs()[0].clone();
let mut frames = connection.capture_frame_copies_dmabuf(&[(output, None)], true)?;
let (frame, _guard, _info) = frames.pop().expect("no frame captured");

frame.map(|mapped| {
    // mapped.buffer() now contains the raw compositor-provided pixels.
    println!("Stride: {}", mapped.stride());
    Ok::<_, libwayshot::Error>(())
})?;
```

`DMAFrameCopy::map` keeps the data in GPU memory until the closure returns, ensuring there is no
implicit truncation to 8-bit.
