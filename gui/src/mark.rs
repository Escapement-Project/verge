//! Verge's mark in motion. A beam sweeps the plate and each sample moves from the V to the
//! project's step as the beam passes it, grey stems rising from the floor; a hold on the step; the
//! next sweep wipes back to the V. Every number in it comes from the design tokens
//! (`tokens::mark`, generated from the site repository's design/tokens.toml and design/marks), as
//! the landing page's does: this file is only how to draw it.
//!
//! It plays once at launch on the strip's mark, and loops while nothing is attached. Neither plays
//! when the desktop asks for reduced motion (`set_motion`).

use egui::{Color32, CornerRadius, Id, Painter, Rect, Stroke};

pub use crate::tokens::mark::{BACK, END, FITTED_24, Geometry, LOOP, MASTER, MOVE, SWEEP};
use crate::tokens::mark::{BEAM_OPACITY, EASE, FLOOR_RUN, GRID_X, GROW, LINE_OPACITY, RISER};

/// One instant of the sequence, whatever the size it is drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    /// Each sample's way from the V (0) to the step (1).
    pub toward_step: [f32; 7],
    /// The beam's place on the 64-unit grid.
    pub beam: Option<f32>,
    /// The step's trace, from and to, on the 64-unit grid.
    pub trace: Option<(f32, f32)>,
    /// How far the floor and the stems have grown, 0 to 1.
    pub lines: f32,
    /// How far the samples have handed over to the icon's own pixels, 0 to 1 (`paint_exact`).
    pub settled: f32,
}

impl Frame {
    /// The mark at rest.
    pub const REST: Frame = Frame {
        toward_step: [0.0; 7],
        beam: None,
        trace: None,
        lines: 0.0,
        settled: 1.0,
    };
}

/// Eases out at the tokens' power: quick off the mark, slowing into place, never past it.
fn ease_out(x: f32) -> f32 {
    1.0 - (1.0 - x.clamp(0.0, 1.0)).powi(EASE)
}

/// The sequence `t` seconds after it starts; the rest outside 0 to `END`.
#[must_use]
pub fn at(t: f32) -> Frame {
    if !(0.0..END).contains(&t) {
        return Frame::REST;
    }
    let toward_step = GRID_X.map(|x| {
        let reached = |t0: f32| ease_out((t - t0 - x / 64.0 * SWEEP) / MOVE);
        if t < BACK {
            reached(0.0)
        } else {
            1.0 - reached(BACK)
        }
    });
    let across = |t0: f32| (t - t0) / SWEEP * 64.0;
    let beam = if t < SWEEP {
        Some(across(0.0))
    } else if (BACK..BACK + SWEEP).contains(&t) {
        Some(across(BACK))
    } else {
        None
    };
    // The step's edge, drawn behind the first beam and wiped by the second.
    let (start, finish) = FLOOR_RUN;
    let trace = if t < SWEEP {
        Some((start, across(0.0).max(start)))
    } else if t < BACK {
        Some((start, finish))
    } else if t < BACK + SWEEP {
        Some((across(BACK).max(start), finish))
    } else {
        None
    }
    .filter(|(from, to)| to > from);
    let lines = (t / GROW).min((END - t) / GROW).clamp(0.0, 1.0);
    let settled = ((t - (END - GROW)) / GROW).clamp(0.0, 1.0);
    Frame {
        toward_step,
        beam,
        trace,
        lines,
        settled,
    }
}

pub struct Colours {
    pub plate: Color32,
    pub lume: Color32,
    pub line: Color32,
    /// A hairline round the plate, which keeps it apart from a night ground as dark as it is.
    pub edge: Option<Color32>,
}

impl Colours {
    /// The face's plate, lume and plate-line colours; `edged` adds the hairline.
    #[must_use]
    pub fn of(face: &crate::tokens::Face, edged: bool) -> Colours {
        Colours {
            plate: face.plate,
            lume: face.lume,
            line: face.plate_line,
            edge: edged.then_some(face.hair),
        }
    }
}

/// Paints the plate and the mark at `frame` into `rect`, a square, with egui's shapes.
pub fn paint(painter: &Painter, rect: Rect, geometry: &Geometry, frame: &Frame, colours: &Colours) {
    let g = geometry;
    let scale = rect.width() / g.side;
    let corner = CornerRadius::same((rect.width() * 0.21875).round() as u8);
    painter.rect_filled(rect, corner, colours.plate);
    if let Some(edge) = colours.edge {
        painter.rect_stroke(
            rect,
            corner,
            Stroke::new(1.0, edge),
            egui::StrokeKind::Inside,
        );
    }
    under(painter, rect, g, frame, colours);
    for (centre, _) in centres(g, frame) {
        painter.circle_filled(rect.min + centre * scale, g.radius * scale, colours.lume);
    }
    beam(painter, rect, g, frame, colours);
}

/// An icon's pixels as coverage, row by row: its plate's (its alpha) and its samples' (how far
/// each pixel inside the plate is from the plate's colour to lume).
pub struct Icon {
    pub side: usize,
    plate: Vec<f32>,
    samples: Vec<f32>,
}

impl Icon {
    /// From the icon's RGBA, drawn in `plate` and `lume`.
    #[must_use]
    pub fn from_rgba(side: usize, rgba: &[u8], plate: Color32, lume: Color32) -> Icon {
        let (ground, ink) = (f32::from(plate.g()), f32::from(lume.g()));
        let pixels = || rgba.as_chunks::<4>().0.iter();
        Icon {
            side,
            plate: pixels().map(|p| f32::from(p[3]) / 255.0).collect(),
            samples: pixels()
                .map(|p| {
                    if p[3] == 255 {
                        ((f32::from(p[1]) - ground) / (ink - ground)).clamp(0.0, 1.0)
                    } else {
                        0.0
                    }
                })
                .collect(),
        }
    }
}

/// As `paint`, but laid down a physical pixel at a time on the icon's own plate, the samples from
/// their coverage. As the stems retract the samples blend into the icon's own (`Frame::settled`),
/// so the last frame is the icon and the hand over to it moves nothing. `geometry` must be the one
/// the icon was drawn from, and `rect` must be `icon.side` physical pixels across.
pub fn paint_exact(
    painter: &Painter,
    rect: Rect,
    geometry: &Geometry,
    frame: &Frame,
    colours: &Colours,
    pixels_per_point: f32,
    icon: &Icon,
) {
    let side = icon.side;
    let pixel = 1.0 / pixels_per_point;
    // On the physical pixel grid, or every pixel would blur into its neighbours.
    let min = (rect.min.to_vec2() * pixels_per_point).round() / pixels_per_point;
    let rect = Rect::from_min_size(min.to_pos2(), rect.size());
    let computed = raster(geometry, frame, side);
    let samples: Vec<f32> = computed
        .iter()
        .zip(&icon.samples)
        .map(|(c, own)| c + (own - c) * frame.settled)
        .collect();
    let layer = |colour: Color32, cover: &[f32]| {
        let mut mesh = egui::Mesh::default();
        for (n, &coverage) in cover.iter().enumerate() {
            if coverage > 0.0 {
                let at = min + egui::vec2((n % side) as f32, (n / side) as f32) * pixel;
                let square = Rect::from_min_size(at.to_pos2(), egui::Vec2::splat(pixel));
                mesh.add_colored_rect(square, colour.gamma_multiply(coverage));
            }
        }
        painter.add(egui::Shape::mesh(mesh));
    };
    layer(colours.plate, &icon.plate);
    under(painter, rect, geometry, frame, colours);
    layer(colours.lume, &samples);
    beam(painter, rect, geometry, frame, colours);
}

/// The samples' centres in the geometry's units, with their index.
fn centres(g: &Geometry, frame: &Frame) -> [(egui::Vec2, usize); 7] {
    std::array::from_fn(|i| {
        let y = g.v[i] + (g.step[i] - g.v[i]) * frame.toward_step[i];
        (egui::vec2(g.x[i], y), i)
    })
}

/// The samples' coverage of each pixel of a plate `side` pixels across, row by row, from 8 × 8
/// samples in each pixel.
#[must_use]
pub fn raster(geometry: &Geometry, frame: &Frame, side: usize) -> Vec<f32> {
    const N: usize = 8;
    let scale = side as f32 / geometry.side;
    let r2 = (geometry.radius * scale).powi(2);
    let dots = centres(geometry, frame).map(|(c, _)| c * scale);
    let in_dot = |x: f32, y: f32| {
        dots.iter()
            .any(|d| (x - d.x).powi(2) + (y - d.y).powi(2) <= r2)
    };
    (0..side * side)
        .map(|n| {
            let mut hits = 0;
            for a in 0..N {
                for b in 0..N {
                    let x = (n % side) as f32 + (a as f32 + 0.5) / N as f32;
                    let y = (n / side) as f32 + (b as f32 + 0.5) / N as f32;
                    hits += usize::from(in_dot(x, y));
                }
            }
            hits as f32 / (N * N) as f32
        })
        .collect()
}

/// The step's trace, the floor and the stems, drawn under the samples.
fn under(painter: &Painter, rect: Rect, g: &Geometry, frame: &Frame, colours: &Colours) {
    let scale = rect.width() / g.side;
    let at = |x: f32, y: f32| rect.min + egui::vec2(x, y) * scale;
    let x = |grid_x: f32| g.x0 + g.k * grid_x;
    let line =
        |alpha: f32, width: f32| Stroke::new(width * scale, colours.line.gamma_multiply(alpha));
    if let Some((from, to)) = frame.trace {
        // Level along a run, up the riser where it passes it, then level to its end.
        let edge = RISER;
        let level = if from < edge { g.floor } else { g.top };
        let mut points = vec![at(x(from), level)];
        let rises = from < edge && to > edge;
        if rises {
            points.push(at(x(edge), g.floor));
            points.push(at(x(edge), g.top));
        }
        points.push(at(x(to), if rises { g.top } else { level }));
        painter.add(egui::Shape::line(points, line(LINE_OPACITY, g.line)));
    }
    if frame.lines > 0.0 {
        let floor = [
            at(x(FLOOR_RUN.0), g.floor),
            at(
                x(FLOOR_RUN.0 + (FLOOR_RUN.1 - FLOOR_RUN.0) * frame.lines),
                g.floor,
            ),
        ];
        painter.line_segment(floor, line(LINE_OPACITY, g.line));
        for (centre, _) in centres(g, frame) {
            let top = g.floor + (centre.y - g.floor) * frame.lines;
            painter.line_segment(
                [at(centre.x, g.floor), at(centre.x, top)],
                line(LINE_OPACITY, g.stem),
            );
        }
    }
}

/// The beam, drawn over the samples.
fn beam(painter: &Painter, rect: Rect, g: &Geometry, frame: &Frame, colours: &Colours) {
    let Some(beam) = frame.beam else {
        return;
    };
    let scale = rect.width() / g.side;
    let ((from, to), width) = (g.beam, g.beam_width);
    let x = rect.min.x + (g.x0 + g.k * beam) * scale;
    painter.line_segment(
        [
            egui::pos2(x, rect.min.y + from * scale),
            egui::pos2(x, rect.min.y + to * scale),
        ],
        Stroke::new(width * scale, colours.line.gamma_multiply(BEAM_OPACITY)),
    );
}

fn motion_id() -> Id {
    Id::unique("verge-mark-motion")
}

/// Whether the mark may move: false when the desktop asks for reduced motion, and in the tests,
/// whose pictures would otherwise depend on when they were taken.
pub fn set_motion(ctx: &egui::Context, on: bool) {
    ctx.data_mut(|d| d.insert_temp(motion_id(), on));
}

#[must_use]
pub fn motion(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<bool>(motion_id()))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sequence_starts_and_ends_on_the_mark() {
        assert_eq!(at(-0.1), Frame::REST);
        assert_eq!(at(END), Frame::REST);
        // At the start only the beam shows, at the plate's left edge.
        let start = at(0.0);
        assert_eq!(start.toward_step, [0.0; 7]);
        assert_eq!(start.lines, 0.0);
        assert_eq!(start.beam, Some(0.0));
        // Just before the end every sample is back on the V and the lines nearly gone.
        let late = at(END - 0.01);
        assert!(late.toward_step.iter().all(|&p| p < 1e-3), "{late:?}");
        assert!(late.lines < 0.1);
    }

    #[test]
    fn the_step_holds_with_its_stems_and_trace() {
        let hold = at((SWEEP + MOVE + BACK) / 2.0);
        assert_eq!(hold.toward_step, [1.0; 7]);
        assert_eq!(hold.lines, 1.0);
        assert_eq!(hold.trace, Some((4.0, 60.0)));
        assert_eq!(hold.beam, None);
    }

    #[test]
    fn the_beam_moves_each_sample_in_turn() {
        // A third of the way through the first sweep, the left samples have started and the right
        // ones have not.
        let f = at(SWEEP / 3.0);
        assert!(f.toward_step[0] > 0.0);
        assert_eq!(f.toward_step[6], 0.0);
        assert!(f.toward_step.windows(2).all(|w| w[0] >= w[1]));
    }

    #[test]
    fn the_grid_maps_onto_each_drawings_samples() {
        // The beam, trace and floor are placed on the 64-unit grid and mapped across with x0 and
        // k; each drawing's samples must sit where that mapping puts the grid's, or the beam and
        // the trace's riser run beside the samples they belong to.
        for g in [&MASTER, &FITTED_24] {
            for (i, &grid) in GRID_X.iter().enumerate() {
                assert!((g.x0 + g.k * grid - g.x[i]).abs() < 1e-3, "{g:?}");
            }
        }
    }

    #[test]
    fn the_launch_play_ends_on_the_icon() {
        // The strip plays on the icon's own plate and, as the stems retract, blends its samples
        // into the icon's own, so the last frame is the icon. Computed, the samples must already
        // sit where the icon's do. The icon's renderer fills a circle's edge pixels a little less
        // than 8 x 8 sampling does, by up to 24 levels at 48 px; a sample in the wrong place is
        // off by the whole plate-to-lume step, 217.
        let face = &crate::tokens::DAY;
        let step = f32::from(face.lume.g() - face.plate.g());
        let cases: [(&[u8], &Geometry, f32); 2] = [
            (include_bytes!("../icons/verge-24.png"), &FITTED_24, 8.0),
            (include_bytes!("../icons/verge-48.png"), &MASTER, 24.0),
        ];
        for (png, geometry, limit) in cases {
            let decoded = eframe::icon_data::from_png_bytes(png).expect("the icon is a PNG");
            let icon =
                Icon::from_rgba(decoded.width as usize, &decoded.rgba, face.plate, face.lume);
            let computed = raster(geometry, &Frame::REST, icon.side);
            let worst = computed
                .iter()
                .zip(&icon.samples)
                .map(|(a, b)| (a - b).abs() * step)
                .fold(0.0, f32::max);
            assert!(
                worst <= limit,
                "{} px: a pixel is {worst} levels off the icon",
                icon.side
            );
        }
        assert_eq!(at((SWEEP + MOVE + BACK) / 2.0).settled, 0.0);
        assert!(at(END - 0.01).settled > 0.9);
        assert_eq!(Frame::REST.settled, 1.0);
    }
}
