//! Deterministic album-art resizing, palette extraction, and 1-bit rendering.

use std::collections::BTreeMap;

use ::image::{DynamicImage, GenericImageView, ImageError, Rgb, RgbImage, imageops::FilterType};
use base64::{Engine as _, engine::general_purpose};

use crate::render::{Art, DitherMode};

/// Errors returned when source artwork cannot be converted into a valid frame.
#[derive(Debug, thiserror::Error)]
pub enum ImagePipelineError {
    #[error("image width and height must both be greater than zero")]
    InvalidDimensions,
    #[error("failed to decode image: {0}")]
    Decode(#[from] ImageError),
}

/// Decode JPEG/PNG bytes, center-crop to the target aspect ratio, and resize exactly.
pub fn resize(bytes: &[u8], width: u32, height: u32) -> Result<RgbImage, ImagePipelineError> {
    if width == 0 || height == 0 {
        return Err(ImagePipelineError::InvalidDimensions);
    }

    let source = ::image::load_from_memory(bytes)?;
    let (source_width, source_height) = source.dimensions();
    if source_width == 0 || source_height == 0 {
        return Err(ImagePipelineError::InvalidDimensions);
    }

    let (crop_x, crop_y, crop_width, crop_height) =
        center_crop(source_width, source_height, width, height);
    let cropped = source.crop_imm(crop_x, crop_y, crop_width, crop_height);
    let resized = cropped.resize_exact(width, height, FilterType::Lanczos3);
    Ok(composite_over_white(&resized))
}

/// Return deterministic dominant and HSL-clamped accent colors.
pub fn extract_palette(image: &RgbImage) -> [String; 2] {
    let histogram = color_histogram(image);
    if histogram.is_empty() {
        return ["#000000".to_string(), format_hex(clamp_accent([0, 0, 0]))];
    }

    let first = histogram[0].color;
    let second = histogram
        .iter()
        .max_by(|left, right| {
            weighted_distance(left, first)
                .total_cmp(&weighted_distance(right, first))
                .then_with(|| right.color.cmp(&left.color))
        })
        .map_or(first, |entry| entry.color);
    let mut centroids = [rgb_to_vector(first), rgb_to_vector(second)];

    for _ in 0..16 {
        let mut sums = [[0.0_f64; 3]; 2];
        let mut weights = [0_u64; 2];
        for entry in &histogram {
            let vector = rgb_to_vector(entry.color);
            let cluster = usize::from(
                squared_distance(vector, centroids[1]) < squared_distance(vector, centroids[0]),
            );
            weights[cluster] += entry.count;
            for channel in 0..3 {
                sums[cluster][channel] += vector[channel] * entry.count as f64;
            }
        }
        for cluster in 0..2 {
            if weights[cluster] > 0 {
                for channel in 0..3 {
                    centroids[cluster][channel] = sums[cluster][channel] / weights[cluster] as f64;
                }
            }
        }
    }

    let weights = cluster_weights(&histogram, centroids);
    let (dominant, accent) = if weights[1] > weights[0] {
        (centroids[1], centroids[0])
    } else {
        (centroids[0], centroids[1])
    };
    let dominant = vector_to_rgb(dominant);
    let accent = clamp_accent(vector_to_rgb(accent));
    [format_hex(dominant), format_hex(accent)]
}

/// Convert a resized RGB image into a packed, base64-encoded 1-bit frame.
pub fn dither(image: &RgbImage, mode: DitherMode) -> Art {
    let ink = match mode {
        DitherMode::Bayer => bayer_ink(image),
        DitherMode::Atkinson => atkinson_ink(image),
    };
    let packed = pack_1bit(image.width(), image.height(), &ink);
    Art {
        w: image.width(),
        h: image.height(),
        dither: mode,
        bits: general_purpose::STANDARD.encode(packed),
    }
}

/// Run decode, resize, palette extraction, dithering, and packed Art construction.
pub fn process(
    bytes: &[u8],
    width: u32,
    height: u32,
    mode: DitherMode,
) -> Result<(Art, [String; 2]), ImagePipelineError> {
    let image = resize(bytes, width, height)?;
    let palette = extract_palette(&image);
    Ok((dither(&image, mode), palette))
}

#[derive(Clone, Copy)]
struct HistogramEntry {
    color: [u8; 3],
    count: u64,
}

fn center_crop(
    source_width: u32,
    source_height: u32,
    target_width: u32,
    target_height: u32,
) -> (u32, u32, u32, u32) {
    if u64::from(source_width) * u64::from(target_height)
        > u64::from(source_height) * u64::from(target_width)
    {
        let crop_width = ((u64::from(source_height) * u64::from(target_width))
            / u64::from(target_height))
        .max(1)
        .min(u64::from(source_width)) as u32;
        (
            (source_width - crop_width) / 2,
            0,
            crop_width,
            source_height,
        )
    } else {
        let crop_height = ((u64::from(source_width) * u64::from(target_height))
            / u64::from(target_width))
        .max(1)
        .min(u64::from(source_height)) as u32;
        (
            0,
            (source_height - crop_height) / 2,
            source_width,
            crop_height,
        )
    }
}

fn composite_over_white(image: &DynamicImage) -> RgbImage {
    let rgba = image.to_rgba8();
    let mut rgb = RgbImage::new(rgba.width(), rgba.height());
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let alpha = u32::from(pixel[3]);
        let blend =
            |channel: u8| ((u32::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        rgb.put_pixel(
            x,
            y,
            Rgb([blend(pixel[0]), blend(pixel[1]), blend(pixel[2])]),
        );
    }
    rgb
}

fn color_histogram(image: &RgbImage) -> Vec<HistogramEntry> {
    let mut counts = BTreeMap::<[u8; 3], u64>::new();
    for pixel in image.pixels() {
        *counts.entry(pixel.0).or_default() += 1;
    }
    let mut histogram: Vec<_> = counts
        .into_iter()
        .map(|(color, count)| HistogramEntry { color, count })
        .collect();
    histogram.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.color.cmp(&right.color))
    });
    histogram
}

fn weighted_distance(entry: &HistogramEntry, from: [u8; 3]) -> f64 {
    squared_distance(rgb_to_vector(entry.color), rgb_to_vector(from)) * entry.count as f64
}

fn cluster_weights(histogram: &[HistogramEntry], centroids: [[f64; 3]; 2]) -> [u64; 2] {
    let mut weights = [0_u64; 2];
    for entry in histogram {
        let vector = rgb_to_vector(entry.color);
        let cluster = usize::from(
            squared_distance(vector, centroids[1]) < squared_distance(vector, centroids[0]),
        );
        weights[cluster] += entry.count;
    }
    weights
}

fn rgb_to_vector(color: [u8; 3]) -> [f64; 3] {
    [
        f64::from(color[0]),
        f64::from(color[1]),
        f64::from(color[2]),
    ]
}

fn vector_to_rgb(vector: [f64; 3]) -> [u8; 3] {
    vector.map(|channel| channel.round().clamp(0.0, 255.0) as u8)
}

fn squared_distance(left: [f64; 3], right: [f64; 3]) -> f64 {
    left.into_iter()
        .zip(right)
        .map(|(left, right)| (left - right).powi(2))
        .sum()
}

fn clamp_accent(color: [u8; 3]) -> [u8; 3] {
    let (hue, saturation, lightness) = rgb_to_hsl(color);
    hsl_to_rgb(hue, saturation.max(0.4), lightness.clamp(0.35, 0.75))
}

fn rgb_to_hsl(color: [u8; 3]) -> (f64, f64, f64) {
    let red = f64::from(color[0]) / 255.0;
    let green = f64::from(color[1]) / 255.0;
    let blue = f64::from(color[2]) / 255.0;
    let maximum = red.max(green).max(blue);
    let minimum = red.min(green).min(blue);
    let delta = maximum - minimum;
    let lightness = (maximum + minimum) / 2.0;
    if delta <= f64::EPSILON {
        return (0.0, 0.0, lightness);
    }

    let saturation = delta / (1.0 - (2.0 * lightness - 1.0).abs());
    let hue_sector = if (maximum - red).abs() <= f64::EPSILON {
        ((green - blue) / delta).rem_euclid(6.0)
    } else if (maximum - green).abs() <= f64::EPSILON {
        (blue - red) / delta + 2.0
    } else {
        (red - green) / delta + 4.0
    };
    (hue_sector * 60.0, saturation, lightness)
}

fn hsl_to_rgb(hue: f64, saturation: f64, lightness: f64) -> [u8; 3] {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let hue_sector = hue.rem_euclid(360.0) / 60.0;
    let secondary = chroma * (1.0 - (hue_sector.rem_euclid(2.0) - 1.0).abs());
    let (red, green, blue) = match hue_sector as u8 {
        0 => (chroma, secondary, 0.0),
        1 => (secondary, chroma, 0.0),
        2 => (0.0, chroma, secondary),
        3 => (0.0, secondary, chroma),
        4 => (secondary, 0.0, chroma),
        _ => (chroma, 0.0, secondary),
    };
    let match_value = lightness - chroma / 2.0;
    [red, green, blue].map(|channel| ((channel + match_value) * 255.0).round() as u8)
}

fn format_hex(color: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", color[0], color[1], color[2])
}

fn bayer_ink(image: &RgbImage) -> Vec<bool> {
    const MATRIX: [[u16; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    image
        .enumerate_pixels()
        .map(|(x, y, pixel)| {
            let threshold = MATRIX[y as usize % 4][x as usize % 4] * 16 + 8;
            luminance(pixel) < threshold
        })
        .collect()
}

fn atkinson_ink(image: &RgbImage) -> Vec<bool> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let mut luminances: Vec<f32> = image
        .pixels()
        .map(|pixel| f32::from(luminance(pixel)))
        .collect();
    let mut ink = vec![false; luminances.len()];
    const NEIGHBORS: [(isize, isize); 6] = [(1, 0), (2, 0), (-1, 1), (0, 1), (1, 1), (0, 2)];

    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let Some(value) = luminances.get(index).copied() else {
                continue;
            };
            let is_ink = value < 128.0;
            if let Some(output) = ink.get_mut(index) {
                *output = is_ink;
            }
            let quantized = if is_ink { 0.0 } else { 255.0 };
            let distributed_error = (value - quantized) / 8.0;

            for (offset_x, offset_y) in NEIGHBORS {
                let Some(neighbor_x) = x.checked_add_signed(offset_x) else {
                    continue;
                };
                let Some(neighbor_y) = y.checked_add_signed(offset_y) else {
                    continue;
                };
                if neighbor_x >= width || neighbor_y >= height {
                    continue;
                }
                let neighbor_index = neighbor_y * width + neighbor_x;
                if let Some(neighbor) = luminances.get_mut(neighbor_index) {
                    *neighbor = (*neighbor + distributed_error).clamp(0.0, 255.0);
                }
            }
        }
    }
    ink
}

fn luminance(pixel: &Rgb<u8>) -> u16 {
    ((299 * u32::from(pixel[0]) + 587 * u32::from(pixel[1]) + 114 * u32::from(pixel[2]) + 500)
        / 1000) as u16
}

fn pack_1bit(width: u32, height: u32, ink: &[bool]) -> Vec<u8> {
    let width = width as usize;
    let height = height as usize;
    let row_bytes = width.div_ceil(8);
    let mut packed = vec![0_u8; row_bytes.saturating_mul(height)];
    for y in 0..height {
        for x in 0..width {
            let source_index = y * width + x;
            if !ink.get(source_index).copied().unwrap_or(false) {
                continue;
            }
            let target_index = y * row_bytes + x / 8;
            if let Some(byte) = packed.get_mut(target_index) {
                *byte |= 1 << (7 - x % 8);
            }
        }
    }
    packed
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use ::image::{GrayImage, ImageFormat, Luma, Rgba, RgbaImage};

    use super::*;

    fn encode(image: DynamicImage, format: ImageFormat) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image.write_to(&mut bytes, format).unwrap();
        bytes.into_inner()
    }

    #[test]
    fn resize_decodes_png_and_jpeg_to_exact_dimensions() {
        let source = DynamicImage::ImageRgb8(RgbImage::from_fn(12, 6, |x, _| {
            if x < 6 {
                Rgb([255, 0, 0])
            } else {
                Rgb([0, 0, 255])
            }
        }));
        for format in [ImageFormat::Png, ImageFormat::Jpeg] {
            let output = resize(&encode(source.clone(), format), 4, 4).unwrap();
            assert_eq!(output.dimensions(), (4, 4));
        }
    }

    #[test]
    fn resize_rejects_zero_dimensions_and_invalid_bytes() {
        assert!(matches!(
            resize(b"not an image", 1, 1),
            Err(ImagePipelineError::Decode(_))
        ));
        let png = encode(
            DynamicImage::ImageRgb8(RgbImage::new(1, 1)),
            ImageFormat::Png,
        );
        assert!(matches!(
            resize(&png, 0, 1),
            Err(ImagePipelineError::InvalidDimensions)
        ));
    }

    #[test]
    fn resize_handles_degenerate_valid_inputs() {
        let one_pixel = encode(
            DynamicImage::ImageRgb8(RgbImage::from_pixel(1, 1, Rgb([12, 34, 56]))),
            ImageFormat::Png,
        );
        assert_eq!(resize(&one_pixel, 1, 1).unwrap().dimensions(), (1, 1));

        let extreme = encode(
            DynamicImage::ImageRgb8(RgbImage::from_pixel(1000, 1, Rgb([1, 2, 3]))),
            ImageFormat::Png,
        );
        assert_eq!(resize(&extreme, 1, 100).unwrap().dimensions(), (1, 100));

        let grayscale = encode(
            DynamicImage::ImageLuma8(GrayImage::from_pixel(2, 2, Luma([90]))),
            ImageFormat::Png,
        );
        assert_eq!(resize(&grayscale, 3, 3).unwrap().dimensions(), (3, 3));

        let transparent = encode(
            DynamicImage::ImageRgba8(RgbaImage::from_pixel(2, 2, Rgba([0, 0, 0, 0]))),
            ImageFormat::Png,
        );
        let output = resize(&transparent, 2, 2).unwrap();
        assert!(output.pixels().all(|pixel| pixel.0 == [255, 255, 255]));
    }

    #[test]
    fn palette_clamps_black_and_white_accents() {
        for image in [
            RgbImage::from_pixel(4, 4, Rgb([0, 0, 0])),
            RgbImage::from_pixel(4, 4, Rgb([255, 255, 255])),
            RgbImage::new(0, 0),
        ] {
            let palette = extract_palette(&image);
            assert_eq!(palette.len(), 2);
            let accent = parse_hex(&palette[1]);
            let (_, saturation, lightness) = rgb_to_hsl(accent);
            assert!(saturation + 0.01 >= 0.4, "{palette:?}");
            assert!((0.35 - 0.01..=0.75 + 0.01).contains(&lightness));
        }
    }

    #[test]
    fn palette_is_deterministic_and_dominant_first() {
        let image = RgbImage::from_fn(10, 10, |x, _| {
            if x < 8 {
                Rgb([20, 40, 60])
            } else {
                Rgb([200, 100, 50])
            }
        });
        let first = extract_palette(&image);
        let second = extract_palette(&image);
        assert_eq!(first, second);
        assert_eq!(first[0], "#14283c");
    }

    #[test]
    fn packed_length_matches_padded_rows() {
        for width in [1, 7, 8, 9, 400] {
            for height in [1, 3, 400] {
                let image = RgbImage::from_fn(width, height, |x, y| {
                    let value = ((x + y) % 256) as u8;
                    Rgb([value, value, value])
                });
                for mode in [DitherMode::Bayer, DitherMode::Atkinson] {
                    let art = dither(&image, mode);
                    let packed = general_purpose::STANDARD.decode(art.bits).unwrap();
                    assert_eq!(packed.len(), width.div_ceil(8) as usize * height as usize);
                }
            }
        }
    }

    #[test]
    fn packing_is_row_major_msb_first() {
        let bits = [
            true, false, true, false, false, false, false, true, false, true, false, true, true,
            true, true, false,
        ];
        assert_eq!(pack_1bit(8, 2, &bits), [0b1010_0001, 0b0101_1110]);
    }

    #[test]
    fn dither_modes_have_stable_golden_snapshots() {
        let image = gradient_fixture();
        let bayer = dither(&image, DitherMode::Bayer).bits;
        let atkinson = dither(&image, DitherMode::Atkinson).bits;
        assert_ne!(bayer, atkinson);
        assert_eq!(
            (bayer.as_str(), atkinson.as_str()),
            (
                "9VD/qtVU/qpVUPqq1UT+qlUQ+qrVQP6qVQD6qlVA6qg=",
                "/6T9sP1K90j80Psy7yD5jPbA3FD3EOmE7KDaINsI5MA="
            )
        );
    }

    #[test]
    fn process_builds_complete_art_and_palette() {
        let source = DynamicImage::ImageRgb8(RgbImage::from_fn(20, 10, |x, _| {
            if x < 15 {
                Rgb([10, 20, 30])
            } else {
                Rgb([220, 160, 80])
            }
        }));
        let bytes = encode(source, ImageFormat::Png);
        let (art, palette) = process(&bytes, 9, 7, DitherMode::Atkinson).unwrap();

        assert_eq!((art.w, art.h, art.dither), (9, 7, DitherMode::Atkinson));
        assert_eq!(palette.len(), 2);
        assert_eq!(
            general_purpose::STANDARD.decode(art.bits).unwrap().len(),
            14
        );
    }

    #[test]
    fn pipeline_is_deterministic() {
        let bytes = encode(
            DynamicImage::ImageRgb8(gradient_fixture()),
            ImageFormat::Png,
        );
        let first = process(&bytes, 16, 16, DitherMode::Atkinson).unwrap();
        let second = process(&bytes, 16, 16, DitherMode::Atkinson).unwrap();
        assert_eq!(first, second);
    }

    fn gradient_fixture() -> RgbImage {
        RgbImage::from_fn(16, 16, |x, y| {
            let value = (x * 12 + y * 4) as u8;
            Rgb([value, value, value])
        })
    }

    fn parse_hex(value: &str) -> [u8; 3] {
        [
            u8::from_str_radix(&value[1..3], 16).unwrap(),
            u8::from_str_radix(&value[3..5], 16).unwrap(),
            u8::from_str_radix(&value[5..7], 16).unwrap(),
        ]
    }
}
