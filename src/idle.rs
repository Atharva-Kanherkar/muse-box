//! Deterministic idle-mode clock rendering.

use ::image::{Rgb, RgbImage};
use chrono::{DateTime, Timelike, Utc};

use crate::{image, render::Art, state::RenderParams};

const BAYER_8X8: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

const DIGIT_SEGMENTS: [u8; 10] = [
    0b111_1110, 0b011_0000, 0b110_1101, 0b111_1001, 0b011_0011, 0b101_1011, 0b101_1111, 0b111_0000,
    0b111_1111, 0b111_1011,
];

#[derive(Clone, Copy, Debug)]
struct ClockLayout {
    x: u32,
    y: u32,
    digit_width: u32,
    digit_height: u32,
    thickness: u32,
    gap: u32,
}

/// Render a packed, device-sized idle clock for the UTC minute containing `at`.
pub fn render(at: DateTime<Utc>, params: RenderParams) -> Art {
    let mut frame = pattern_field(at, params.width, params.height);
    draw_clock(&mut frame, at);
    image::dither(&frame, params.dither)
}

fn pattern_field(at: DateTime<Utc>, width: u32, height: u32) -> RgbImage {
    let minute = at.timestamp().div_euclid(60) as u64;
    let phase_x = (minute % 8) as usize;
    let phase_y = ((minute / 3) % 8) as usize;
    RgbImage::from_fn(width, height, |x, y| {
        let ordered = u16::from(BAYER_8X8[(y as usize + phase_y) % 8][(x as usize + phase_x) % 8]);
        let drift = ((u64::from(x) * 3 + u64::from(y) * 5 + minute) % 23) as u16;
        let value = 178 + ((ordered * 51) / 63) + drift;
        let value = value.min(238) as u8;
        Rgb([value, value, value])
    })
}

fn draw_clock(frame: &mut RgbImage, at: DateTime<Utc>) {
    let layout = clock_layout(frame.width(), frame.height());
    let digits = [
        at.hour() / 10,
        at.hour() % 10,
        at.minute() / 10,
        at.minute() % 10,
    ];
    let mut x = layout.x;
    for (index, digit) in digits.into_iter().enumerate() {
        if index == 2 {
            draw_colon(frame, x, layout);
            x += layout.thickness + layout.gap * 2;
        }
        draw_digit(frame, x, layout.y, digit as usize, layout);
        x += layout.digit_width + layout.gap;
    }
}

fn clock_layout(width: u32, height: u32) -> ClockLayout {
    let margin = (width.min(height) / 32).max(2);
    let max_height = height.saturating_sub(margin * 2).max(1);
    let mut digit_height = (max_height * 3 / 4).max(1);
    loop {
        let thickness = (digit_height / 10).max(2).min(digit_height);
        let digit_width = (digit_height * 2 / 5).max(thickness * 2);
        let gap = (thickness / 2).max(2);
        let total_width = digit_width * 4 + thickness + gap * 6;
        if total_width <= width.saturating_sub(margin * 2) || digit_height == 1 {
            return ClockLayout {
                x: width.saturating_sub(total_width) / 2,
                y: height.saturating_sub(digit_height) / 2,
                digit_width,
                digit_height,
                thickness,
                gap,
            };
        }
        digit_height -= 1;
    }
}

fn draw_digit(frame: &mut RgbImage, x: u32, y: u32, digit: usize, layout: ClockLayout) {
    let mask = DIGIT_SEGMENTS[digit];
    let half = layout.digit_height / 2;
    let horizontal_width = layout.digit_width.saturating_sub(layout.thickness * 2);
    let vertical_height = half.saturating_sub(layout.thickness);
    let segments = [
        (x + layout.thickness, y, horizontal_width, layout.thickness),
        (
            x + layout.digit_width - layout.thickness,
            y + layout.thickness,
            layout.thickness,
            vertical_height,
        ),
        (
            x + layout.digit_width - layout.thickness,
            y + half,
            layout.thickness,
            vertical_height,
        ),
        (
            x + layout.thickness,
            y + layout.digit_height - layout.thickness,
            horizontal_width,
            layout.thickness,
        ),
        (x, y + half, layout.thickness, vertical_height),
        (x, y + layout.thickness, layout.thickness, vertical_height),
        (
            x + layout.thickness,
            y + half - layout.thickness / 2,
            horizontal_width,
            layout.thickness,
        ),
    ];
    for (index, rect) in segments.into_iter().enumerate() {
        if mask & (1 << (6 - index)) != 0 {
            fill_rect(frame, rect);
        }
    }
}

fn draw_colon(frame: &mut RgbImage, x: u32, layout: ClockLayout) {
    let upper_y = layout.y + layout.digit_height / 3 - layout.thickness / 2;
    let lower_y = layout.y + layout.digit_height * 2 / 3 - layout.thickness / 2;
    fill_rect(frame, (x, upper_y, layout.thickness, layout.thickness));
    fill_rect(frame, (x, lower_y, layout.thickness, layout.thickness));
}

fn fill_rect(frame: &mut RgbImage, (x, y, width, height): (u32, u32, u32, u32)) {
    for pixel_y in y..y.saturating_add(height).min(frame.height()) {
        for pixel_x in x..x.saturating_add(width).min(frame.width()) {
            frame.put_pixel(pixel_x, pixel_y, Rgb([20, 20, 20]));
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::{Engine as _, engine::general_purpose};
    use chrono::TimeZone;

    use super::*;
    use crate::render::DitherMode;

    #[test]
    fn golden_bayer_clock_at_400_square() {
        let at = Utc.with_ymd_and_hms(2026, 8, 21, 9, 41, 0).unwrap();
        let params = RenderParams {
            width: 400,
            height: 400,
            dither: DitherMode::Bayer,
        };
        let art = render(at, params);
        assert_eq!(packed_hash(&art), 8_132_122_953_464_961_965);
        assert!(clock_layout(params.width, params.height).digit_height * 5 >= params.height * 2);
    }

    #[test]
    fn golden_atkinson_clock_at_296_wide() {
        let at = Utc.with_ymd_and_hms(2026, 12, 31, 23, 58, 0).unwrap();
        let params = RenderParams {
            width: 296,
            height: 128,
            dither: DitherMode::Atkinson,
        };
        let art = render(at, params);
        assert_eq!(packed_hash(&art), 18_235_879_371_226_139_755);
        assert!(clock_layout(params.width, params.height).digit_height * 5 >= params.height * 2);
    }

    #[test]
    fn fixed_inputs_are_byte_deterministic() {
        let at = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 0).unwrap();
        let params = RenderParams {
            width: 296,
            height: 128,
            dither: DitherMode::Bayer,
        };
        assert_eq!(render(at, params).bits, render(at, params).bits);
    }

    #[test]
    fn minute_changes_pattern_and_clock() {
        let first = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 0).unwrap();
        let params = RenderParams::default();
        assert_ne!(
            render(first, params).bits,
            render(first + chrono::Duration::minutes(1), params).bits
        );
    }

    fn packed_hash(art: &Art) -> u64 {
        let bytes = general_purpose::STANDARD.decode(&art.bits).unwrap();
        bytes.into_iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }
}
