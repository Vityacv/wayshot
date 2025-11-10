use std::{
    ffi::CString,
    os::fd::OwnedFd,
    time::{SystemTime, UNIX_EPOCH},
};

use gbm::BufferObject;
use image::{ColorType, DynamicImage, ImageBuffer, Pixel, Rgb, Rgba};
use memmap2::MmapMut;
use rustix::{
    fs::{self, SealFlags},
    io, shm,
};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_output,
    wl_shm::{self, Format},
    wl_shm_pool::WlShmPool,
};

use crate::{Error, Result, region::{LogicalRegion, Size}};

pub struct FrameGuard {
    pub buffer: WlBuffer,
    pub shm_pool: WlShmPool,
}

impl Drop for FrameGuard {
    fn drop(&mut self) {
        self.buffer.destroy();
        self.shm_pool.destroy();
    }
}

pub struct DMAFrameGuard {
    pub buffer: WlBuffer,
}
impl Drop for DMAFrameGuard {
    fn drop(&mut self) {
        self.buffer.destroy();
    }
}

pub struct EGLImageGuard<'a, T: khronos_egl::api::EGL1_5> {
    pub image: khronos_egl::Image,
    pub(crate) egl_instance: &'a khronos_egl::Instance<T>,
    pub(crate) egl_display: khronos_egl::Display,
}

impl<T: khronos_egl::api::EGL1_5> Drop for EGLImageGuard<'_, T> {
    fn drop(&mut self) {
        self.egl_instance
            .destroy_image(self.egl_display, self.image)
            .unwrap_or_else(|e| {
                tracing::error!("EGLimage destruction had error: {e}");
            });
    }
}

/// Type of frame supported by the compositor. For now we only support Argb8888, Xrgb8888, and
/// Xbgr8888.
///
/// See `zwlr_screencopy_frame_v1::Event::Buffer` as it's retrieved from there.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct FrameFormat {
    pub format: Format,
    /// Size of the frame in pixels. This will always be in "landscape" so a
    /// portrait 1080x1920 frame will be 1920x1080 and will need to be rotated!
    pub size: Size,
    /// Stride is the number of bytes between the start of a row and the start of the next row.
    pub stride: u32,
}

/// Type of DMABUF frame supported by the compositor
///
/// See `zwlr_screencopy_frame_v1::Event::linux_dmabuf` as it's retrieved from there.
#[derive(Debug, Copy, Clone, PartialEq)]
pub struct DMAFrameFormat {
    pub format: u32,
    /// Size of the frame in pixels. This will always be in "landscape" so a
    /// portrait 1080x1920 frame will be 1920x1080 and will need to be rotated!
    pub size: Size,
}

impl FrameFormat {
    /// Returns the size of the frame in bytes, which is the stride * height.
    pub fn byte_size(&self) -> u64 {
        self.stride as u64 * self.size.height as u64
    }
}

#[tracing::instrument(skip(frame_data))]
fn create_image_buffer<P>(
    frame_format: &FrameFormat,
    frame_data: &FrameData,
) -> Result<ImageBuffer<P, Vec<P::Subpixel>>>
where
    P: Pixel<Subpixel = u8>,
{
    tracing::debug!("Creating image buffer");
    match frame_data {
        FrameData::Mmap(frame_mmap) => ImageBuffer::from_vec(
            frame_format.size.width,
            frame_format.size.height,
            frame_mmap.to_vec(),
        )
        .ok_or(Error::BufferTooSmall),
        FrameData::GBMBo(_) => todo!(),
    }
}

#[derive(Debug)]
pub enum FrameData {
    Mmap(MmapMut),
    GBMBo(BufferObject<()>),
}
/// The copied frame comprising of the FrameFormat, ColorType (Rgba8), and a memory backed shm
/// file that holds the image data in it.
#[derive(Debug)]
pub struct FrameCopy {
    pub frame_format: FrameFormat,
    pub frame_color_type: ColorType,
    pub frame_data: FrameData,
    pub transform: wl_output::Transform,
    /// Logical region with the transform already applied.
    pub logical_region: LogicalRegion,
    pub physical_size: Size,
}

impl FrameCopy {
    pub(crate) fn get_image(&mut self) -> Result<DynamicImage, Error> {
        let image: DynamicImage = (self as &FrameCopy).try_into()?;
        Ok(image)
    }
}

/// Representation of a frame copied via DMA-BUF.
///
/// The buffer contents remain in GPU memory and can be accessed by mapping the underlying GBM
/// buffer object. This is the recommended path when capturing HDR or otherwise high bit-depth
/// outputs because no implicit down-conversion to 8-bit occurs.
#[derive(Debug)]
pub struct DMAFrameCopy {
    pub frame_format: DMAFrameFormat,
    pub buffer_object: BufferObject<()>,
    pub transform: wl_output::Transform,
    /// Logical region with the transform already applied.
    pub logical_region: LogicalRegion,
    pub physical_size: Size,
}

impl TryFrom<&FrameCopy> for DynamicImage {
    type Error = Error;

    fn try_from(value: &FrameCopy) -> Result<Self> {
        Ok(match value.frame_color_type {
            ColorType::Rgb8 => {
                Self::ImageRgb8(create_image_buffer(&value.frame_format, &value.frame_data)?)
            }
            ColorType::Rgba8 => {
                Self::ImageRgba8(create_image_buffer(&value.frame_format, &value.frame_data)?)
            }
            ColorType::Rgb16 => {
                let (width, height) = (
                    value.frame_format.size.width,
                    value.frame_format.size.height,
                );
                let buffer = value.to_rgb16_vec()?;
                let image = ImageBuffer::<Rgb<u16>, _>::from_vec(width, height, buffer)
                    .ok_or(Error::BufferTooSmall)?;
                Self::ImageRgb16(image)
            }
            ColorType::Rgba16 => {
                let (width, height) = (
                    value.frame_format.size.width,
                    value.frame_format.size.height,
                );
                let buffer = value.to_rgba16_vec()?;
                let image = ImageBuffer::<Rgba<u16>, _>::from_vec(width, height, buffer)
                    .ok_or(Error::BufferTooSmall)?;
                Self::ImageRgba16(image)
            }
            _ => return Err(Error::InvalidColor),
        })
    }
}

impl FrameCopy {
    fn mmap_bytes(&self) -> Result<&[u8]> {
        match &self.frame_data {
            FrameData::Mmap(mmap) => Ok(&mmap[..]),
            FrameData::GBMBo(_) => Err(Error::InvalidColor),
        }
    }

    fn to_rgb16_vec(&self) -> Result<Vec<u16>> {
        let order = match self.frame_format.format {
            wl_shm::Format::Xrgb2101010 | wl_shm::Format::Argb2101010 => ChannelOrder::Rgb,
            wl_shm::Format::Xbgr2101010 | wl_shm::Format::Abgr2101010 => ChannelOrder::Bgr,
            _ => return Err(Error::InvalidColor),
        };
        convert_10bit_to_u16(self.mmap_bytes()?, order, false)
    }

    fn to_rgba16_vec(&self) -> Result<Vec<u16>> {
        let order = match self.frame_format.format {
            wl_shm::Format::Argb2101010 => ChannelOrder::Rgb,
            wl_shm::Format::Abgr2101010 => ChannelOrder::Bgr,
            _ => return Err(Error::InvalidColor),
        };
        convert_10bit_to_u16(self.mmap_bytes()?, order, true)
    }
}

#[derive(Copy, Clone)]
enum ChannelOrder {
    Rgb,
    Bgr,
}

fn convert_10bit_to_u16(data: &[u8], order: ChannelOrder, include_alpha: bool) -> Result<Vec<u16>> {
    if data.len() % 4 != 0 {
        return Err(Error::BufferTooSmall);
    }
    let mut out = Vec::with_capacity((data.len() / 4) * if include_alpha { 4 } else { 3 });
    for chunk in data.chunks_exact(4) {
        let pixel = u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        let r = expand_10_bit(((pixel >> 20) & 0x3ff) as u16);
        let g = expand_10_bit(((pixel >> 10) & 0x3ff) as u16);
        let b = expand_10_bit((pixel & 0x3ff) as u16);
        let (c0, c1, c2) = match order {
            ChannelOrder::Rgb => (r, g, b),
            ChannelOrder::Bgr => (b, g, r),
        };
        out.push(c0);
        out.push(c1);
        out.push(c2);
        if include_alpha {
            let alpha = ((pixel >> 30) & 0x3) as u16;
            out.push(expand_alpha_2bit(alpha));
        }
    }
    Ok(out)
}

fn expand_10_bit(value: u16) -> u16 {
    // Scale 10-bit to 16-bit by repeating the high bits.
    (value << 6) | (value >> 4)
}

fn expand_alpha_2bit(value: u16) -> u16 {
    match value {
        0 => 0x0000,
        1 => 0x5555,
        2 => 0xAAAA,
        _ => 0xFFFF,
    }
}

impl DMAFrameCopy {
    /// Map the DMA-BUF backed frame for CPU access.
    ///
    /// This helper will map the entire buffer region and pass the resulting [`gbm::MappedBufferObject`]
    /// to the provided closure. The mapping is released as soon as the closure returns.
    pub fn map<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&gbm::MappedBufferObject<'_, ()>) -> R,
    {
        self.buffer_object
            .map(
                0,
                0,
                self.frame_format.size.width,
                self.frame_format.size.height,
                f,
            )
            .map_err(Error::from)
    }

    /// Consume the frame and return the owned GBM [`BufferObject`].
    pub fn into_buffer_object(self) -> BufferObject<()> {
        self.buffer_object
    }
}

fn get_mem_file_handle() -> String {
    format!(
        "/libwayshot-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|time| time.subsec_nanos().to_string())
            .unwrap_or("unknown".into())
    )
}

/// Return a RawFd to a shm file. We use memfd create on linux and shm_open for BSD support.
/// You don't need to mess around with this function, it is only used by
/// capture_output_frame.
pub fn create_shm_fd() -> std::io::Result<OwnedFd> {
    // Only try memfd on linux and freebsd.
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    loop {
        // Create a file that closes on successful execution and seal it's operations.
        match fs::memfd_create(
            CString::new("libwayshot")?.as_c_str(),
            fs::MemfdFlags::CLOEXEC | fs::MemfdFlags::ALLOW_SEALING,
        ) {
            Ok(fd) => {
                // This is only an optimization, so ignore errors.
                // F_SEAL_SRHINK = File cannot be reduced in size.
                // F_SEAL_SEAL = Prevent further calls to fcntl().
                let _ = fs::fcntl_add_seals(&fd, fs::SealFlags::SHRINK | SealFlags::SEAL);
                return Ok(fd);
            }
            Err(io::Errno::INTR) => continue,
            Err(io::Errno::NOSYS) => break,
            Err(errno) => return Err(std::io::Error::from(errno)),
        }
    }

    // Fallback to using shm_open.
    let mut mem_file_handle = get_mem_file_handle();
    loop {
        let open_result = shm::open(
            mem_file_handle.as_str(),
            shm::OFlags::CREATE | shm::OFlags::EXCL | shm::OFlags::RDWR,
            fs::Mode::RUSR | fs::Mode::WUSR,
        );
        // O_CREAT = Create file if does not exist.
        // O_EXCL = Error if create and file exists.
        // O_RDWR = Open for reading and writing.
        // O_CLOEXEC = Close on successful execution.
        // S_IRUSR = Set user read permission bit .
        // S_IWUSR = Set user write permission bit.
        match open_result {
            Ok(fd) => match shm::unlink(mem_file_handle.as_str()) {
                Ok(_) => return Ok(fd),
                Err(errno) => return Err(std::io::Error::from(errno)),
            },
            Err(io::Errno::EXIST) => {
                // If a file with that handle exists then change the handle
                mem_file_handle = get_mem_file_handle();
                continue;
            }
            Err(io::Errno::INTR) => continue,
            Err(errno) => return Err(std::io::Error::from(errno)),
        }
    }
}
