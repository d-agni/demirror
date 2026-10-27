//! Animated test pattern. Lets the whole pipeline run on any OS without a screen.

use std::time::Instant;

use anyhow::Result;

use super::Source;

pub struct Synthetic {
    id: u32,
    width: u32,
    height: u32,
    start: Instant,
}

impl Synthetic {
    pub fn new(id: u32, width: u32, height: u32) -> Self {
        Self {
            id,
            width,
            height,
            start: Instant::now(),
        }
    }
}

/// 3x5 bitmap glyphs, one row per `u8` (low 3 bits), for `0-9`, `#` and `:`.
fn glyph(c: char) -> [u8; 5] {
    match c {
        '0' => [7, 5, 5, 5, 7],
        '1' => [2, 6, 2, 2, 7],
        '2' => [7, 1, 7, 4, 7],
        '3' => [7, 1, 7, 1, 7],
        '4' => [5, 5, 7, 1, 1],
        '5' => [7, 4, 7, 1, 7],
        '6' => [7, 4, 7, 5, 7],
        '7' => [7, 1, 1, 1, 1],
        '8' => [7, 5, 7, 5, 7],
        '9' => [7, 5, 7, 1, 7],
        '#' => [5, 7, 5, 7, 5],
        ':' => [0, 2, 0, 2, 0],
        _ => [0; 5],
    }
}

impl Source for Synthetic {
    fn capture(&mut self, out: &mut Vec<u8>) -> Result<(u32, u32)> {
        let (w, h) = (self.width as usize, self.height as usize);
        let t = self.start.elapsed().as_secs_f32();
        out.resize(w * h * 4, 0);

        let shift = (t * 40.0) as usize;
        for y in 0..h {
            let row = &mut out[y * w * 4..(y + 1) * w * 4];
            for x in 0..w {
                let grid = (x + shift).is_multiple_of(64) || y % 64 == 0;
                let px = &mut row[x * 4..x * 4 + 4];
                if grid {
                    px.copy_from_slice(&[90, 90, 90, 255]);
                } else {
                    let b = (x * 255 / w) as u8;
                    let g = (y * 255 / h) as u8;
                    px.copy_from_slice(&[b / 3 + 40, g / 3 + 20, 60, 255]);
                }
            }
        }

        // Colour bars along the bottom.
        const BARS: [[u8; 3]; 7] = [
            [255, 255, 255],
            [0, 255, 255],
            [255, 255, 0],
            [0, 255, 0],
            [255, 0, 255],
            [0, 0, 255],
            [255, 0, 0],
        ];
        let bar_top = h * 7 / 8;
        for y in bar_top..h {
            for x in 0..w {
                let [b, g, r] = BARS[x * BARS.len() / w];
                out[(y * w + x) * 4..][..3].copy_from_slice(&[b, g, r]);
            }
        }

        // A square bouncing around, so motion and latency are visible.
        let size = (h / 8).max(8);
        let span_x = w.saturating_sub(size).max(1) as f32;
        let span_y = bar_top.saturating_sub(size).max(1) as f32;
        let bx = ((t * 0.37).sin() * 0.5 + 0.5) * span_x;
        let by = ((t * 0.53).cos() * 0.5 + 0.5) * span_y;
        fill(out, w, bx as usize, by as usize, size, size, [40, 200, 255]);

        // "#<id>" and the elapsed time "m:ss" in large blocky digits.
        let secs = t as u32;
        let label = format!("#{}  {}:{:02}", self.id, secs / 60, secs % 60);
        let scale = (h / 40).max(2);
        let mut cx = scale * 4;
        for c in label.chars() {
            for (row, bits) in glyph(c).iter().enumerate() {
                for col in 0..3 {
                    if bits & (4 >> col) != 0 {
                        fill(
                            out,
                            w,
                            cx + col * scale,
                            scale * 4 + row * scale,
                            scale,
                            scale,
                            [255, 255, 255],
                        );
                    }
                }
            }
            cx += scale * 4;
        }

        Ok((self.width, self.height))
    }

    fn resize(&mut self, width: u32, height: u32) -> Result<()> {
        (self.width, self.height) = (width, height);
        Ok(())
    }
}

fn fill(out: &mut [u8], w: usize, x0: usize, y0: usize, fw: usize, fh: usize, bgr: [u8; 3]) {
    let h = out.len() / (w * 4);
    for y in y0..(y0 + fh).min(h) {
        for x in x0..(x0 + fw).min(w) {
            out[(y * w + x) * 4..][..3].copy_from_slice(&bgr);
        }
    }
}
