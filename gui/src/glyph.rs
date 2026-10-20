//! Status glyphs, each a 5 × 5 grid of square cells, painted as rects.
//!
//! One grid, one cell size and one gap for all of them, so they read as one family at any size.

use egui::{Color32, Painter, Rect, Vec2, pos2, vec2};

/// A glyph's rows, top first; bit 4 is the leftmost cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Glyph(pub [u8; 5]);

/// Our firmware: an aperture with a step cut into its foot.
pub const OURS: Glyph = Glyph([0b11111, 0b10001, 0b10101, 0b10001, 0b11011]);
/// Stock: a solid block with one cell missing.
pub const STOCK: Glyph = Glyph([0b11111, 0b11111, 0b11011, 0b11111, 0b11111]);
/// ISP mode, a bootloader waiting for an image: four corner brackets.
pub const WAITING: Glyph = Glyph([0b11011, 0b10001, 0b00000, 0b10001, 0b11011]);
/// An update under way: an alternating lattice.
pub const WORKING: Glyph = Glyph([0b10101, 0b01010, 0b10101, 0b01010, 0b10101]);
pub const DONE: Glyph = Glyph([0b00001, 0b00011, 0b10110, 0b11100, 0b01000]);
pub const FAILED: Glyph = Glyph([0b10001, 0b01010, 0b00100, 0b01010, 0b10001]);
/// Needs a person: the jumper, a unit that could not be asked.
pub const ATTENTION: Glyph = Glyph([0b00100, 0b00100, 0b00100, 0b00000, 0b00100]);

impl Glyph {
    #[must_use]
    pub fn is_set(self, row: usize, column: usize) -> bool {
        self.0[row] & (1 << (4 - column)) != 0
    }

    /// Paints the glyph filling `size` × `size` with its top left at `origin`.
    pub fn paint(self, painter: &Painter, origin: egui::Pos2, size: f32, color: Color32) {
        let pitch = size / 5.0;
        // A gap of a sixth of the pitch keeps cells distinct at small sizes and solid at large.
        let cell = pitch * 5.0 / 6.0;
        for row in 0..5 {
            for column in 0..5 {
                if self.is_set(row, column) {
                    let min = origin + vec2(column as f32 * pitch, row as f32 * pitch);
                    painter.rect_filled(Rect::from_min_size(min, Vec2::splat(cell)), 0.0, color);
                }
            }
        }
    }

    /// Allocates a square of `size` in `ui` and paints the glyph in it.
    pub fn show(self, ui: &mut egui::Ui, size: f32, color: Color32) -> egui::Response {
        let (rect, response) = ui.allocate_exact_size(Vec2::splat(size), egui::Sense::hover());
        self.paint(ui.painter(), pos2(rect.min.x, rect.min.y), size, color);
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_fits_its_grid_and_differs() {
        let all = [OURS, STOCK, WAITING, WORKING, DONE, FAILED, ATTENTION];
        for glyph in all {
            assert!(glyph.0.iter().all(|row| *row < 32), "{glyph:?}");
        }
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(a, b);
            }
        }
        assert!(OURS.is_set(0, 0) && !OURS.is_set(4, 2));
    }
}
