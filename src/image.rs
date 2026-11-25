//! Image → ESC/POS raster (GS v 0), centered on 80 mm paper (576 dots).

use image::{imageops::FilterType, DynamicImage, GenericImageView, Rgba, RgbaImage};
use tracing::error;

pub struct ImageOptions {
    pub paper_width: u32,
    pub exact_width: Option<u32>,
    pub exact_height: Option<u32>,
    /// Crop uniform borders (same color as the top-left pixel) first.
    pub autocrop: bool,
}

impl Default for ImageOptions {
    fn default() -> Self {
        ImageOptions { paper_width: 576, exact_width: None, exact_height: None, autocrop: true }
    }
}

fn same(a: &Rgba<u8>, b: &Rgba<u8>) -> bool {
    a.0.iter().zip(b.0.iter()).all(|(x, y)| x.abs_diff(*y) <= 1)
}

/// Crop the frame of the top-left pixel's color, only when it surrounds the
/// image on all four sides (like Jimp's default autocrop).
fn autocrop(img: RgbaImage) -> RgbaImage {
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return img;
    }
    let bg = *img.get_pixel(0, 0);
    let row_is_bg = |y: u32| (0..w).all(|x| same(img.get_pixel(x, y), &bg));
    let col_is_bg = |x: u32, y0: u32, y1: u32| (y0..y1).all(|y| same(img.get_pixel(x, y), &bg));

    let mut top = 0;
    while top < h - 1 && row_is_bg(top) {
        top += 1;
    }
    let mut bottom = h;
    while bottom > top + 1 && row_is_bg(bottom - 1) {
        bottom -= 1;
    }
    let mut left = 0;
    while left < w - 1 && col_is_bg(left, top, bottom) {
        left += 1;
    }
    let mut right = w;
    while right > left + 1 && col_is_bg(right - 1, top, bottom) {
        right -= 1;
    }
    if top == 0 || left == 0 || bottom == h || right == w {
        return img;
    }
    image::imageops::crop_imm(&img, left, top, right - left, bottom - top).to_image()
}

/// Convert an encoded image (PNG/JPEG) to an ESC/POS raster command.
/// Returns an empty buffer when the image cannot be decoded.
pub fn to_escpos(bytes: &[u8], opt: &ImageOptions) -> Vec<u8> {
    let img = match image::load_from_memory(bytes) {
        Ok(i) => i,
        Err(e) => {
            error!(target: "image", error = %e, bytes = bytes.len(), "Failed to process image");
            return Vec::new();
        }
    };
    let mut rgba = img.to_rgba8();
    if opt.autocrop {
        rgba = autocrop(rgba);
    }
    let mut img = DynamicImage::ImageRgba8(rgba);

    if opt.exact_width.is_some() || opt.exact_height.is_some() {
        let (w0, h0) = img.dimensions();
        let (w, h) = match (opt.exact_width, opt.exact_height) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) => (w, ((h0 as f64) * (w as f64) / (w0 as f64)).round().max(1.0) as u32),
            (None, Some(h)) => (((w0 as f64) * (h as f64) / (h0 as f64)).round().max(1.0) as u32, h),
            (None, None) => (w0, h0),
        };
        img = img.resize_exact(w, h, FilterType::Triangle);
    }
    let img = img.to_rgba8();
    let (iw, ih) = img.dimensions();

    let width_bytes = opt.paper_width.div_ceil(8) as usize;
    let padded = (width_bytes * 8) as i64;
    let start_x = (padded - iw as i64) / 2;

    let mut data = vec![0u8; width_bytes * ih as usize];
    for y in 0..ih {
        for x in 0..iw {
            let px = (start_x + x as i64) as usize;
            if (px as i64) < 0 || px >= padded as usize {
                continue;
            }
            let Rgba([r, g, b, a]) = *img.get_pixel(x, y);
            // Composite over white paper, then threshold the luminance.
            let a = a as f64 / 255.0;
            let blend = |c: u8| c as f64 * a + 255.0 * (1.0 - a);
            let grey = 0.2126 * blend(r) + 0.7152 * blend(g) + 0.0722 * blend(b);
            if grey < 127.0 {
                data[y as usize * width_bytes + px / 8] |= 1 << (7 - (px % 8));
            }
        }
    }

    let mut out = vec![
        0x1d,
        0x76,
        0x30,
        0x00,
        (width_bytes % 256) as u8,
        (width_bytes / 256) as u8,
        (ih % 256) as u8,
        (ih / 256) as u8,
    ];
    out.extend_from_slice(&data);
    out
}
