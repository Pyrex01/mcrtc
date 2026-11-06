//! Procedural textures: seeded noise, drawn here, so the binary ships no artwork.

use eframe::egui::{Color32, ColorImage};

/// Deterministic, so every run and every machine draws the same pixels.
struct Rng(u32);

impl Rng {
    fn next(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(1664525).wrapping_add(1013904223);
        f64::from(self.0) / 4294967296.0
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() * limit as f64) as usize
    }
}

fn rgb(hex: u32) -> Color32 {
    let [_, r, g, b] = hex.to_be_bytes();
    Color32::from_rgb(r, g, b)
}

/// Saturation and lightness run from 0 to 1, the hue from 0 to 360.
fn hsl(hue: f32, saturation: f32, lightness: f32) -> Color32 {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let channel = |offset: f32| {
        let k = (offset + hue / 30.0) % 12.0;
        let value = lightness - chroma / 2.0 * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0);
        (value * 255.0).round() as u8
    };
    Color32::from_rgb(channel(0.0), channel(8.0), channel(4.0))
}

struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<Color32>,
}

impl Canvas {
    fn new(width: usize, height: usize, fill: Color32) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill; width * height],
        }
    }

    fn fill(&mut self, x: usize, y: usize, width: usize, height: usize, color: Color32) {
        for row in y..y + height {
            for column in x..x + width {
                let pixel = &mut self.pixels[row * self.width + column];
                *pixel = color.blend_over(*pixel);
            }
        }
    }

    fn image(self) -> ColorImage {
        ColorImage::new([self.width, self.height], self.pixels)
    }
}

trait Blend {
    fn blend_over(self, under: Color32) -> Color32;
}

impl Blend for Color32 {
    fn blend_over(self, under: Color32) -> Color32 {
        let alpha = u32::from(self.a());
        let mix = |top: u8, bottom: u8| {
            // The channels of a Color32 are premultiplied by its alpha already.
            (u32::from(top) + u32::from(bottom) * (255 - alpha) / 255).min(255) as u8
        };
        Color32::from_rgb(
            mix(self.r(), under.r()),
            mix(self.g(), under.g()),
            mix(self.b(), under.b()),
        )
    }
}

fn noise_tile(palette: &[u32], seed: u32) -> ColorImage {
    let mut random = Rng(seed);
    let mut canvas = Canvas::new(16, 16, Color32::BLACK);
    for pixel in &mut canvas.pixels {
        *pixel = rgb(palette[random.below(palette.len())]);
    }
    canvas.image()
}

pub fn earth() -> ColorImage {
    let palette = [
        0x2e2118, 0x2a1d15, 0x352619, 0x241912, 0x3a2a1c, 0x2e2118, 0x1f1610,
    ];
    noise_tile(&palette, 7)
}

pub fn stone() -> ColorImage {
    let palette = [0x6c6c6c, 0x666666, 0x616161, 0x707070, 0x5d5d5d, 0x686868];
    noise_tile(&palette, 21)
}

/// The wordmark: an original block font, one bevelled stone cube per glyph pixel.
const WORD: [&[&str]; 5] = [GLYPH_M, GLYPH_C, GLYPH_R, GLYPH_T, GLYPH_C];
const GLYPH_M: &[&str] = &[
    "11000011", "11100111", "11111111", "11011011", "11000011", "11000011", "11000011",
];
const GLYPH_C: &[&str] = &[
    "0111111", "1111111", "1100000", "1100000", "1100000", "1111111", "0111111",
];
const GLYPH_R: &[&str] = &[
    "1111110", "1111111", "1100011", "1111110", "1101100", "1100110", "1100011",
];
const GLYPH_T: &[&str] = &[
    "11111111", "11111111", "00011000", "00011000", "00011000", "00011000", "00011000",
];

pub fn logo() -> ColorImage {
    const CELL: usize = 5;
    const DEPTH: usize = 3;
    const SPACING: usize = 1;

    let mut cells = Vec::new();
    let mut offset = 0;
    for glyph in WORD {
        for (y, line) in glyph.iter().enumerate() {
            for (x, pixel) in line.bytes().enumerate() {
                if pixel == b'1' {
                    cells.push(((offset + x) * CELL, y * CELL));
                }
            }
        }
        offset += glyph[0].len() + SPACING;
    }
    let columns = offset - SPACING;

    let mut canvas = Canvas::new(
        columns * CELL + DEPTH,
        7 * CELL + DEPTH,
        Color32::TRANSPARENT,
    );
    // The shadow goes down first, so it can never cover a neighbouring block.
    for &(left, top) in &cells {
        for row in top + DEPTH..top + DEPTH + CELL {
            for column in left + DEPTH..left + DEPTH + CELL {
                canvas.pixels[row * canvas.width + column] = Color32::BLACK;
            }
        }
    }
    let mut random = Rng(99);
    for &(left, top) in &cells {
        for row in top..top + CELL {
            for column in left..left + CELL {
                let shade = 176 - (top * 2 / 3) as i32 + random.below(34) as i32;
                let shade = shade.clamp(0, 255) as u8;
                canvas.pixels[row * canvas.width + column] = Color32::from_gray(shade);
            }
        }
        canvas.fill(
            left,
            top,
            CELL,
            1,
            Color32::from_white_alpha((0.35 * 255.0) as u8),
        );
        canvas.fill(
            left,
            top + CELL - 1,
            CELL,
            1,
            Color32::from_black_alpha((0.3 * 255.0) as u8),
        );
    }
    canvas.image()
}

/// A small pixel portrait derived from the name, so friends are easy to tell apart.
pub fn portrait(name: &str) -> ColorImage {
    let seed = name.chars().fold(2166136261_u32, |seed, letter| {
        (seed ^ letter as u32).wrapping_mul(16777619)
    });
    let mut random = Rng(seed);
    let hue = random.below(360) as f32;
    let skin = [0xc69c6d, 0xe0b48a, 0x8d5a3a, 0xf0c8a0][random.below(4)];

    let mut canvas = Canvas::new(8, 8, rgb(skin));
    let hair = hsl(hue, 0.45, 0.28);
    canvas.fill(0, 0, 8, 2, hair);
    for x in 0..8 {
        if random.next() > 0.45 {
            canvas.fill(x, 2, 1, 1, hair);
        }
    }
    canvas.fill(0, 2, 1, 3, hair);
    canvas.fill(7, 2, 1, 3, hair);
    canvas.fill(1, 4, 2, 1, Color32::WHITE);
    canvas.fill(5, 4, 2, 1, Color32::WHITE);
    let eyes = hsl((hue + 150.0) % 360.0, 0.6, 0.35);
    canvas.fill(2, 4, 1, 1, eyes);
    canvas.fill(5, 4, 1, 1, eyes);
    canvas.fill(3, 6, 2, 1, Color32::from_black_alpha((0.45 * 255.0) as u8));
    canvas.image()
}

/// A plain stone block for the window icon, as 32x32 RGBA.
pub fn icon() -> (Vec<u8>, u32, u32) {
    const SIZE: usize = 32;
    let tile = stone();
    let mut rgba = Vec::with_capacity(SIZE * SIZE * 4);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let color = tile.pixels[(y / 2) * 16 + x / 2];
            rgba.extend_from_slice(&color.to_array());
        }
    }
    (rgba, SIZE as u32, SIZE as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_noise_matches_the_seeded_generator() {
        let mut random = Rng(7);
        assert_eq!(random.0, 7);
        random.next();
        assert_eq!(random.0, 7_u32 * 1664525 + 1013904223);
    }

    #[test]
    fn colors_convert_from_hsl() {
        assert_eq!(hsl(0.0, 1.0, 0.5), Color32::from_rgb(255, 0, 0));
        assert_eq!(hsl(120.0, 1.0, 0.5), Color32::from_rgb(0, 255, 0));
        assert_eq!(hsl(240.0, 1.0, 0.5), Color32::from_rgb(0, 0, 255));
    }

    #[test]
    fn a_name_always_gets_the_same_face_and_friends_differ() {
        assert_eq!(portrait("alex").pixels, portrait("alex").pixels);
        assert_ne!(portrait("alex").pixels, portrait("sam").pixels);
        assert_eq!(portrait("").size, [8, 8]);
    }

    #[test]
    fn the_logo_fits_the_title_screen() {
        let logo = logo();
        assert_eq!(logo.size, [208, 38]);
        assert!(logo.pixels.iter().any(|pixel| pixel.a() == 0));
    }
}
