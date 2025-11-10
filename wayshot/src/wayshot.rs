use config::Config;
use std::{
    env,
    fs::File,
    io::{self, BufWriter, Cursor, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use eyre::{Result, bail, eyre};
use image::{ColorType, DynamicImage, GenericImageView, ImageBuffer, Rgb, Rgba};
use libwayshot::WayshotConnection;

mod cli;
mod config;
mod utils;

use dialoguer::{FuzzySelect, theme::ColorfulTheme};
use tracing::{info, warn};
use utils::{EncodingFormat, get_absolute_path, get_expanded_path, parse_geometry_str, waysip_to_region};

use wl_clipboard_rs::copy::{MimeType, Options, Source};

use rustix::{mm::{MapFlags, ProtFlags, mmap, munmap}, runtime::{self, Fork}};

fn select_output<T>(outputs: &[T]) -> Option<usize>
where
    T: ToString,
{
    let Ok(selection) = FuzzySelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Choose Screen")
        .default(0)
        .items(outputs)
        .interact()
    else {
        return None;
    };
    Some(selection)
}

fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    let config_path = cli.config.unwrap_or(Config::get_default_path());
    let config = Config::load(&config_path).unwrap_or_default();
    let base = config.base.unwrap_or_default();
    let file = config.file.unwrap_or_default();

    let log_level = cli.log_level.unwrap_or(base.get_log_level());
    tracing_subscriber::fmt()
        .with_max_level(log_level)
        .with_writer(io::stderr)
        .init();

    let cursor = match cli.cursor {
        true => cli.cursor,
        _ => base.cursor.unwrap_or_default(),
    };
    let clipboard = match cli.clipboard {
        true => cli.clipboard,
        _ => base.clipboard.unwrap_or_default(),
    };

    let input_encoding = cli
        .file
        .as_ref()
        .and_then(|pathbuf| pathbuf.try_into().ok());
    let encoding = cli
        .encoding
        .or(input_encoding)
        .unwrap_or(file.encoding.unwrap_or_default());

    if let Some(ie) = input_encoding
        && ie != encoding
    {
        tracing::warn!(
            "The encoding requested '{encoding}' does not match the output file's encoding '{ie}'. Still using the requested encoding however.",
        );
    }

    let file_name_format = cli.file_name_format.unwrap_or(
        file.name_format
            .unwrap_or("wayshot-%Y_%m_%d-%H_%M_%S".to_string()),
    );
    let mut stdout_print = base.stdout.unwrap_or_default();
    let file = cli
        .file
        .and_then(|pathbuf| {
            if pathbuf.to_string_lossy() == "-" {
                stdout_print = true;
                None
            } else {
                Some(utils::get_full_file_name(
                    &pathbuf,
                    &file_name_format,
                    encoding,
                ))
            }
        })
        .or_else(|| {
            if base.file.unwrap_or_default() {
                let dir = file
                    .path
                    .unwrap_or_else(|| env::current_dir().unwrap_or_default());
                Some(utils::get_full_file_name(&dir, &file_name_format, encoding))
            } else {
                None
            }
        });

    let output = cli.output.or(base.output);

    let tone_map_target = cli
        .tone_map_file
        .map(|pathbuf| {
            let expanded = get_expanded_path(&pathbuf);
            let absolute = get_absolute_path(&expanded);
            if absolute.is_dir() {
                return Err(eyre!(
                    "--tone-map-file must include a filename, got directory {}",
                    absolute.display()
                ));
            }
            let encoding: EncodingFormat = (&absolute)
                .try_into()
                .map_err(|e| eyre!("failed to infer encoding for {}: {e}", absolute.display()))?;
            Ok((absolute, encoding))
        })
        .transpose()?;

    let dmabuf_device = cli.dmabuf.as_ref().map(|path| get_expanded_path(path));

    let wayshot_conn = if let Some(device_path) = dmabuf_device.clone() {
        let render_node = device_path
            .to_str()
            .ok_or_else(|| eyre!("render node path must be valid UTF-8"))?;
        WayshotConnection::new_with_dmabuf(render_node)?
    } else {
        WayshotConnection::new()?
    };

    let stdout = io::stdout();
    let mut writer = BufWriter::new(stdout.lock());

    if cli.list_outputs {
        let valid_outputs = wayshot_conn.get_all_outputs();
        for output in valid_outputs {
            writeln!(writer, "{}", output.name)?;
        }

        writer.flush()?;

        return Ok(());
    }

    if dmabuf_device.is_some() {
        if stdout_print {
            bail!("--dmabuf does not support writing to stdout");
        }
        if clipboard {
            bail!("--dmabuf cannot be combined with --clipboard");
        }

        let output_name = output
            .clone()
            .ok_or_else(|| eyre!("--dmabuf requires an explicit --output"))?;
        let file_path = file
            .clone()
            .ok_or_else(|| eyre!("--dmabuf requires an explicit output file path"))?;

        let output_info = wayshot_conn
            .get_all_outputs()
            .iter()
            .find(|info| info.name == output_name)
            .cloned()
            .ok_or_else(|| eyre!("No output named '{output_name}'"))?;

        let mut frames =
            wayshot_conn.capture_frame_copies_dmabuf(&[(output_info, None)], cursor)?;
        let (frame_copy, frame_guard, _) = frames
            .pop()
            .ok_or_else(|| eyre!("Failed to capture DMA-BUF frame"))?;

        let metadata_path = persist_dmabuf(&file_path, &frame_copy)?;
        drop(frame_guard);

        info!(
            "Saved DMA-BUF capture to {} (metadata: {})",
            file_path.display(),
            metadata_path.display()
        );

        return Ok(());
    }

    let image_buffer = if let Some(geometry_str) = &cli.geometry_str {
        let region = parse_geometry_str(geometry_str)?;
        wayshot_conn
            .screenshot_region(region, cursor)
            .or_else(|err| match err {
                libwayshot::Error::NoOutputs => wayshot_conn.screenshot_all(cursor),
                _ => Err(err),
            })?
    } else if cli.geometry {
        wayshot_conn.screenshot_freeze(
            |w_conn| {
                let info = libwaysip::get_area(
                    Some(libwaysip::WaysipConnection {
                        connection: &w_conn.conn,
                        globals: &w_conn.globals,
                    }),
                    libwaysip::SelectionType::Area,
                )
                .map_err(|e| libwayshot::Error::FreezeCallbackError(e.to_string()))?
                .ok_or(libwayshot::Error::FreezeCallbackError(
                    "Failed to capture the area".to_string(),
                ))?;
                waysip_to_region(info.size(), info.left_top_point())
            },
            cursor,
        )?
    } else if let Some(output_name) = output {
        let outputs = wayshot_conn.get_all_outputs();
        if let Some(output) = outputs.iter().find(|output| output.name == output_name) {
            wayshot_conn.screenshot_single_output(output, cursor)?
        } else {
            bail!("No output found!");
        }
    } else if cli.choose_output {
        let outputs = wayshot_conn.get_all_outputs();
        let output_names: Vec<&str> = outputs
            .iter()
            .map(|display| display.name.as_str())
            .collect();
        if let Some(index) = select_output(&output_names) {
            wayshot_conn.screenshot_single_output(&outputs[index], cursor)?
        } else {
            bail!("No output found!");
        }
    } else {
        wayshot_conn.screenshot_all(cursor)?
    };

    let mut image_buf: Option<Cursor<Vec<u8>>> = None;
    if let Some(ref path) = file {
        save_image_with_options(&image_buffer, path, encoding, cli.embed_hdr_icc)?;
    }

    if stdout_print {
        let bytes = encode_image_to_vec(&image_buffer, encoding, cli.embed_hdr_icc)?;
        writer.write_all(&bytes)?;
        image_buf = Some(Cursor::new(bytes));
    }

    if clipboard {
        clipboard_daemonize(match image_buf.take() {
            Some(buf) => buf,
            None => {
                let bytes = encode_image_to_vec(&image_buffer, encoding, cli.embed_hdr_icc)?;
                Cursor::new(bytes)
            }
        })?;
    }

    if let Some((tone_path, tone_encoding)) = tone_map_target {
        if matches!(image_buffer.color(), ColorType::Rgb16 | ColorType::Rgba16) {
            let tonemapped = tonemap_hdr_to_sdr(&image_buffer)?;
            save_image_with_options(&tonemapped, &tone_path, tone_encoding, false)?;
        } else {
            warn!(
                "--tone-map-file requested but screenshot is {:?}; skipping tone-mapped export",
                image_buffer.color()
            );
        }
    }

    Ok(())
}

/// Daemonize and copy the given buffer containing the encoded image to the clipboard
fn clipboard_daemonize(buffer: Cursor<Vec<u8>>) -> Result<()> {
    let mut opts = Options::new();
    opts.foreground(false);

    match unsafe { runtime::kernel_fork() } {
        Ok(Fork::ParentOf(_)) => Ok(()),
        Ok(Fork::Child(_)) => {
            opts.foreground(true);
            opts.copy(
                Source::Bytes(buffer.into_inner().into()),
                MimeType::Autodetect,
            )?;
            Ok(())
        }
        Err(e) => {
            tracing::warn!(
                "Fork failed with error: {e}, couldn't offer image on the clipboard persistently.
                 Use a clipboard manager to record screenshot."
            );
            opts.copy(
                Source::Bytes(buffer.into_inner().into()),
                MimeType::Autodetect,
            )?;
            Ok(())
        }
    }
}

fn persist_dmabuf(path: &Path, frame: &libwayshot::DMAFrameCopy) -> Result<PathBuf> {
    let mut file = File::create(path)?;

    let fd = frame
        .buffer_object
        .fd_for_plane(0)
        .map_err(|_| eyre!("failed to export dma-buf fd"))?;
    let length = frame.buffer_object.stride() as usize * frame.frame_format.size.height as usize;

    let ptr = unsafe {
        mmap(
            std::ptr::null_mut(),
            length,
            ProtFlags::READ,
            MapFlags::SHARED,
            &fd,
            0,
        )?
    };

    let slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, length) };
    file.write_all(slice)?;
    file.flush()?;

    unsafe {
        munmap(ptr, length)?;
    }

    let base_ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("raw");
    let metadata_path = path.with_extension(format!("{base_ext}.metadata"));
    let modifier: u64 = frame.buffer_object.modifier().into();

    let metadata = format!(
        "format=0x{format:08x}\nwidth={width}\nheight={height}\nstride={stride}\nmodifier=0x{modifier:016x}\ntransform={transform:?}\nlogical_x={logical_x}\nlogical_y={logical_y}\nlogical_width={logical_width}\nlogical_height={logical_height}\n",
        format = frame.frame_format.format,
        width = frame.frame_format.size.width,
        height = frame.frame_format.size.height,
        stride = frame.buffer_object.stride(),
        modifier = modifier,
        transform = frame.transform,
        logical_x = frame.logical_region.inner.position.x,
        logical_y = frame.logical_region.inner.position.y,
        logical_width = frame.logical_region.inner.size.width,
        logical_height = frame.logical_region.inner.size.height,
    );

    std::fs::write(&metadata_path, metadata)?;

    Ok(metadata_path)
}

fn save_image_with_options(
    image: &DynamicImage,
    path: &Path,
    encoding: EncodingFormat,
    embed_hdr: bool,
) -> Result<()> {
    if embed_hdr
        && matches!(encoding, EncodingFormat::Png)
        && matches!(image.color(), ColorType::Rgb16 | ColorType::Rgba16)
    {
        let bytes = encode_png_with_hdr(image)?;
        std::fs::write(path, bytes)?;
        Ok(())
    } else {
        if embed_hdr && !matches!(image.color(), ColorType::Rgb16 | ColorType::Rgba16) {
            warn!(
                "--embed-hdr-icc requested but screenshot is {:?}; HDR metadata not applied",
                image.color()
            );
        }
        let mut file = BufWriter::new(File::create(path)?);
        image.write_to(&mut file, encoding.into())?;
        Ok(())
    }
}

fn encode_image_to_vec(
    image: &DynamicImage,
    encoding: EncodingFormat,
    embed_hdr: bool,
) -> Result<Vec<u8>> {
    if embed_hdr
        && matches!(encoding, EncodingFormat::Png)
        && matches!(image.color(), ColorType::Rgb16 | ColorType::Rgba16)
    {
        encode_png_with_hdr(image)
    } else {
        let mut cursor = Cursor::new(Vec::new());
        image.write_to(&mut cursor, encoding.into())?;
        Ok(cursor.into_inner())
    }
}

const CICP_BT2020_PQ: [u8; 4] = [9, 16, 9, 1];

fn encode_png_with_hdr(image: &DynamicImage) -> Result<Vec<u8>> {
    use png::{BitDepth, ColorType as PngColorType, Encoder as PngEncoder};

    let (png_color, raw): (PngColorType, Vec<u16>) = match image {
        DynamicImage::ImageRgb16(img) => (PngColorType::Rgb, img.as_raw().clone()),
        DynamicImage::ImageRgba16(img) => (PngColorType::Rgba, img.as_raw().clone()),
        _ => {
            return Err(eyre!(
                "HDR metadata requires a 16-bit RGB/RGBA screenshot, got {:?}",
                image.color()
            ));
        }
    };

    let (width, height) = image.dimensions();
    let mut channel_bytes = Vec::with_capacity(raw.len() * 2);
    for value in raw {
        channel_bytes.extend_from_slice(&value.to_be_bytes());
    }

    let mut output = Vec::new();
    {
        let mut encoder = PngEncoder::new(&mut output, width, height);
        encoder.set_color(png_color);
        encoder.set_depth(BitDepth::Sixteen);
        let mut writer = encoder.write_header()?;
        writer.write_chunk(png::chunk::cICP, &CICP_BT2020_PQ)?;
        let mut buffer = Vec::new();
        add_fake_exif(&mut buffer)?;
        writer.write_chunk(png::chunk::eXIf, &buffer)?;
        writer.write_image_data(&channel_bytes)?;
    }

    Ok(output)
}

fn add_fake_exif(buffer: &mut Vec<u8>) -> Result<()> {
    const TIFF_HEADER: [u8; 8] = [
        0x4D, 0x4D, // big endian
        0x00, 0x2A, // magic
        0x00, 0x00, 0x00, 0x08, // offset to first IFD
    ];
    buffer.extend_from_slice(&TIFF_HEADER);
    // No actual tags; just indicate zero entries.
    buffer.extend_from_slice(&[0x00, 0x00]);
    Ok(())
}

fn tonemap_hdr_to_sdr(image: &DynamicImage) -> Result<DynamicImage> {
    match image {
        DynamicImage::ImageRgb16(img) => {
            let (width, height) = img.dimensions();
            let mut out: ImageBuffer<Rgb<u8>, Vec<u8>> = ImageBuffer::new(width, height);
            for (x, y, pixel) in img.enumerate_pixels() {
                let [r, g, b] = tonemap_pixel(pixel.0);
                out.put_pixel(x, y, Rgb([r, g, b]));
            }
            Ok(DynamicImage::ImageRgb8(out))
        }
        DynamicImage::ImageRgba16(img) => {
            let (width, height) = img.dimensions();
            let mut out: ImageBuffer<Rgba<u8>, Vec<u8>> = ImageBuffer::new(width, height);
            for (x, y, pixel) in img.enumerate_pixels() {
                let [r, g, b] = tonemap_pixel([pixel.0[0], pixel.0[1], pixel.0[2]]);
                let alpha = (pixel.0[3] >> 8) as u8;
                out.put_pixel(x, y, Rgba([r, g, b, alpha]));
            }
            Ok(DynamicImage::ImageRgba8(out))
        }
        _ => Err(eyre!(
            "Tone mapping requires a 16-bit RGB/RGBA screenshot, got {:?}",
            image.color()
        )),
    }
}

fn tonemap_pixel(pixel: [u16; 3]) -> [u8; 3] {
    let bt2020_linear = pixel.map(|value| {
        let normalized = value as f32 / 65535.0;
        pq_eotf(normalized) / 10000.0
    });

    let (r2020, g2020, b2020) = (bt2020_linear[0], bt2020_linear[1], bt2020_linear[2]);
    let (sr, sg, sb) = bt2020_to_srgb_linear(r2020, g2020, b2020);

    let exposure = 1.2;
    [sr, sg, sb].map(|channel| {
        let mapped = filmic_tonemap(exposure * channel.max(0.0));
        let srgb = linear_to_srgb(mapped);
        (srgb.clamp(0.0, 1.0) * 255.0).round() as u8
    })
}

fn pq_eotf(x: f32) -> f32 {
    const M1: f32 = 2610.0 / 16384.0;
    const M2: f32 = 2523.0 / 32.0;
    const C1: f32 = 3424.0 / 4096.0;
    const C2: f32 = 2413.0 / 128.0;
    const C3: f32 = 2392.0 / 128.0;

    let x_pow = x.powf(M1);
    ((x_pow - C1) / (C2 - C3 * x_pow)).max(0.0).powf(M2)
}

fn bt2020_to_srgb_linear(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let sr = 1.6605 * r - 0.5876 * g - 0.0728 * b;
    let sg = -0.1246 * r + 1.1329 * g - 0.0083 * b;
    let sb = -0.0182 * r - 0.1006 * g + 1.1187 * b;
    (sr, sg, sb)
}

fn filmic_tonemap(x: f32) -> f32 {
    fn hable(x: f32) -> f32 {
        const A: f32 = 0.15;
        const B: f32 = 0.50;
        const C: f32 = 0.10;
        const D: f32 = 0.20;
        const E: f32 = 0.02;
        const F: f32 = 0.30;
        ((x * (A * x + C * B) + D * E) / (x * (A * x + B) + D * F)) - E / F
    }

    const WHITE: f32 = 11.2;
    let numerator = hable(x);
    let denominator = hable(WHITE);
    if denominator == 0.0 {
        0.0
    } else {
        (numerator / denominator).clamp(0.0, 1.0)
    }
}

fn linear_to_srgb(v: f32) -> f32 {
    if v <= 0.0031308 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}
