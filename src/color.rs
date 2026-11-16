use ash::vk::Format;
use log::error;
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
pub(crate) struct ImageInfo {
    pub(crate) width: u32,
    pub(crate) height: u32,

    pub(crate) pixel_format: Format,
}

#[derive(Default)]
pub(crate) struct Histogram<T> {
    pub(crate) data: Vec<T>,
    pub(crate) maxima: T,
}

#[derive(Default)]
pub(crate) struct Parade<T> {
    pub(crate) width: usize,
    pub(crate) data: Vec<T>,
    pub(crate) maxima: T,
}

#[derive(Default)]
pub(crate) struct ColorAnalysisResult {
    pub(crate) duration: Duration,
    pub(crate) sample_size: [u32; 2],
    pub(crate) luminance_is_pq: bool,

    pub(crate) pixel_format: Format,

    pub(crate) luminance_histogram: Histogram<u32>,
    pub(crate) color_histogram: Histogram<[u32; 3]>,
    pub(crate) luminance_parade: Parade<u32>,
    pub(crate) color_parade: Parade<[u32; 3]>,
}

pub(crate) struct PreparedAnalysis {
    pub(crate) analysis: ColorAnalysisResult,
    pub(crate) luminance: egui::ColorImage,
    pub(crate) color: egui::ColorImage,
}

#[derive(Clone, Copy)]
enum PixelLayout {
    Rgba8,
    Bgra8,
    A2Rgb10,
    A2Bgr10,
}

impl PixelLayout {
    fn from_format(format: Format) -> Option<Self> {
        match format {
            Format::R8G8B8A8_UNORM | Format::R8G8B8A8_SRGB => Some(Self::Rgba8),
            Format::B8G8R8A8_UNORM | Format::B8G8R8A8_SRGB => Some(Self::Bgra8),
            Format::A2R10G10B10_UNORM_PACK32 => Some(Self::A2Rgb10),
            Format::A2B10G10R10_UNORM_PACK32 => Some(Self::A2Bgr10),
            _ => None,
        }
    }
}

pub(crate) fn analyze_pixels(pixels: &[u8], image_info: ImageInfo) -> Option<ColorAnalysisResult> {
    let Some(layout) = PixelLayout::from_format(image_info.pixel_format) else {
        error!("unsupported pixel format {:?}", image_info.pixel_format);
        return None;
    };

    let start = Instant::now();

    let ten_bit = matches!(layout, PixelLayout::A2Rgb10 | PixelLayout::A2Bgr10);
    let value_bucket_count = if ten_bit { 1024 } else { 256 };
    const MAX_PARADE_WIDTH: usize = 4096;
    let image_width = image_info.width as usize;
    let parade_width = image_width.min(MAX_PARADE_WIDTH);
    let parade_cell_count = parade_width * value_bucket_count;

    let mut result = ColorAnalysisResult {
        sample_size: [image_info.width, image_info.height],
        luminance_is_pq: ten_bit,
        pixel_format: image_info.pixel_format,
        ..Default::default()
    };
    result
        .luminance_histogram
        .data
        .resize(value_bucket_count, 0);
    result
        .color_histogram
        .data
        .resize(value_bucket_count, [0; 3]);
    result.luminance_parade.width = parade_width;
    result.luminance_parade.data.resize(parade_cell_count, 0);
    result.color_parade.width = parade_width;
    result.color_parade.data.resize(parade_cell_count, [0; 3]);

    for (pixel_index, pixel) in pixels.chunks_exact(4).enumerate() {
        let bytes: u32 = u32::from_ne_bytes(pixel[..4].try_into().unwrap());

        let (red, green, blue) = match layout {
            PixelLayout::Rgba8 => (pixel[0] as u16, pixel[1] as u16, pixel[2] as u16),
            PixelLayout::Bgra8 => (pixel[2] as u16, pixel[1] as u16, pixel[0] as u16),
            PixelLayout::A2Rgb10 => (
                ((bytes >> 20) & 0x3FF) as u16,
                ((bytes >> 10) & 0x3FF) as u16,
                (bytes & 0x3FF) as u16,
            ),
            PixelLayout::A2Bgr10 => (
                (bytes & 0x3FF) as u16,
                ((bytes >> 10) & 0x3FF) as u16,
                ((bytes >> 20) & 0x3FF) as u16,
            ),
        };

        result.color_histogram.data[red as usize][0] += 1;
        result.color_histogram.data[green as usize][1] += 1;
        result.color_histogram.data[blue as usize][2] += 1;
        result.color_histogram.maxima[0] =
            result.color_histogram.maxima[0].max(result.color_histogram.data[red as usize][0]);
        result.color_histogram.maxima[1] =
            result.color_histogram.maxima[1].max(result.color_histogram.data[green as usize][1]);
        result.color_histogram.maxima[2] =
            result.color_histogram.maxima[2].max(result.color_histogram.data[blue as usize][2]);

        let luminance = if ten_bit {
            (nits_to_pq(hdr10_luminance_nits(red, green, blue)) * (value_bucket_count - 1) as f64)
                as f32
        } else {
            0.2126 * red as f32 + 0.7152 * green as f32 + 0.0722 * blue as f32
        };
        let luminance_bucket = (luminance.round() as usize).min(value_bucket_count - 1);
        result.luminance_histogram.data[luminance_bucket] += 1;
        result.luminance_histogram.maxima = result
            .luminance_histogram
            .maxima
            .max(result.luminance_histogram.data[luminance_bucket]);

        let x = pixel_index % image_width;
        let x_bucket = x * parade_width / image_width;
        let parade_row_offset = x_bucket * value_bucket_count;
        let red_cell = parade_row_offset + red as usize;
        let green_cell = parade_row_offset + green as usize;
        let blue_cell = parade_row_offset + blue as usize;
        let luminance_cell = parade_row_offset + luminance_bucket;

        result.color_parade.data[red_cell][0] += 1;
        result.color_parade.data[green_cell][1] += 1;
        result.color_parade.data[blue_cell][2] += 1;
        result.color_parade.maxima[0] =
            result.color_parade.maxima[0].max(result.color_parade.data[red_cell][0]);
        result.color_parade.maxima[1] =
            result.color_parade.maxima[1].max(result.color_parade.data[green_cell][1]);
        result.color_parade.maxima[2] =
            result.color_parade.maxima[2].max(result.color_parade.data[blue_cell][2]);

        result.luminance_parade.data[luminance_cell] += 1;
        result.luminance_parade.maxima = result
            .luminance_parade
            .maxima
            .max(result.luminance_parade.data[luminance_cell]);
    }

    let end = Instant::now();
    result.duration = end - start;

    Some(result)
}

pub(crate) fn luminance_parade_image(analysis: &ColorAnalysisResult) -> egui::ColorImage {
    let height = analysis.luminance_histogram.data.len();
    let parade = &analysis.luminance_parade;
    let mut image = egui::ColorImage::new([parade.width, height], egui::Color32::BLACK);

    for x in 0..parade.width {
        for value in 0..height {
            let count = parade.data[x * height + value];
            let intensity = density_intensity(count, parade.maxima);
            image.pixels[(height - 1 - value) * parade.width + x] =
                egui::Color32::from_gray(intensity);
        }
    }

    image
}

pub(crate) fn color_parade_image(analysis: &ColorAnalysisResult) -> egui::ColorImage {
    let height = analysis.color_histogram.data.len();
    let parade = &analysis.color_parade;
    let maximum = parade.maxima.into_iter().max().unwrap_or(0);
    let mut image = egui::ColorImage::new([parade.width, height], egui::Color32::BLACK);

    for x in 0..parade.width {
        for value in 0..height {
            let counts = parade.data[x * height + value];
            let red = density_intensity(counts[0], maximum);
            let green = density_intensity(counts[1], maximum);
            let blue = density_intensity(counts[2], maximum);
            image.pixels[(height - 1 - value) * parade.width + x] =
                egui::Color32::from_rgb(red, green, blue);
        }
    }

    image
}

fn density_intensity(count: u32, maximum: u32) -> u8 {
    if count == 0 || maximum == 0 {
        return 0;
    }

    ((count as f32 / maximum as f32).sqrt() * 255.0).round() as u8
}
fn pq10_to_nits(code: u16) -> f64 {
    let pq = f64::from(code) / 1023.0;

    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 32.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 128.0;
    const C3: f64 = 2392.0 / 128.0;

    let p = pq.powf(1.0 / M2);
    let linear = ((p - C1).max(0.0) / (C2 - C3 * p)).powf(1.0 / M1);

    10_000.0 * linear
}

fn hdr10_luminance_nits(red: u16, green: u16, blue: u16) -> f64 {
    0.2627 * pq10_to_nits(red) + 0.6780 * pq10_to_nits(green) + 0.0593 * pq10_to_nits(blue)
}

pub(crate) fn nits_to_pq(nits: f64) -> f64 {
    const M1: f64 = 2610.0 / 16384.0;
    const M2: f64 = 2523.0 / 32.0;
    const C1: f64 = 3424.0 / 4096.0;
    const C2: f64 = 2413.0 / 128.0;
    const C3: f64 = 2392.0 / 128.0;

    let p = (nits.clamp(0.0, 10_000.0) / 10_000.0).powf(M1);
    ((C1 + C2 * p) / (1.0 + C3 * p)).powf(M2)
}
