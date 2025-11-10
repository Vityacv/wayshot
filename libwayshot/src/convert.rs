use image::ColorType;
use wayland_client::protocol::wl_shm;

pub trait Convert {
    /// Convert raw image data into output type, return said type
    fn convert_inplace(&self, data: &mut [u8]) -> ColorType;
}

#[derive(Default)]
struct ConvertNone {}

#[derive(Default)]
struct ConvertRGB8 {}

#[derive(Default)]
struct ConvertBGR888 {}

/// Creates format converter based of input format, return None if conversion
/// isn't possible. Conversion is happening inplace.
pub fn create_converter(format: wl_shm::Format) -> Option<Box<dyn Convert>> {
    match format {
        wl_shm::Format::Xbgr8888 | wl_shm::Format::Abgr8888 => Some(Box::<ConvertNone>::default()),
        wl_shm::Format::Xrgb8888 | wl_shm::Format::Argb8888 => Some(Box::<ConvertRGB8>::default()),
        wl_shm::Format::Bgr888 => Some(Box::<ConvertBGR888>::default()),
        _ => None,
    }
}

impl Convert for ConvertNone {
    fn convert_inplace(&self, _data: &mut [u8]) -> ColorType {
        ColorType::Rgba8
    }
}

impl Convert for ConvertRGB8 {
    fn convert_inplace(&self, data: &mut [u8]) -> ColorType {
        for chunk in data.chunks_exact_mut(4) {
            chunk.swap(0, 2);
        }
        ColorType::Rgba8
    }
}

impl Convert for ConvertBGR888 {
    fn convert_inplace(&self, _data: &mut [u8]) -> ColorType {
        ColorType::Rgb8
    }
}
