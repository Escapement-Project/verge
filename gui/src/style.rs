//! Colour roles, spacing, and type by role, from the Escapement Project's design tokens.
//!
//! The colours come from [`crate::tokens`], a copy of the generated `tokens.rs`. The window shows
//! the day or the night face as the system prefers, and every colour is read from the face on
//! show when it is drawn. Every font is OFL 1.1, a fixed instance the tokens' build cuts, with its
//! licence in `fonts/`. They are inserted in front of egui's own families, so egui's fonts stay as
//! fallbacks for any glyph they lack.

use std::sync::Arc;

use egui::epaint::text::{FontData, FontDefinitions, FontFamily};
use egui::{
    Color32, Context, CornerRadius, FontId, RichText, Shadow, Stroke, TextStyle, Theme, Visuals,
};

pub use crate::tokens::Face;
use crate::tokens::{DAY, FOCUS_WIDTH, NIGHT, WASH};

/// The colour roles of the face on show: day or night, as the system prefers.
#[must_use]
pub fn face(ctx: &Context) -> &'static Face {
    match ctx.theme() {
        Theme::Light => &DAY,
        Theme::Dark => &NIGHT,
    }
}

impl Face {
    /// The ink wash over what the pointer is on, to lay over any ground.
    #[must_use]
    pub fn wash(&self) -> Color32 {
        self.ink.gamma_multiply(WASH)
    }

    /// The wash laid over `ground`, as one opaque colour.
    #[must_use]
    pub fn washed(&self, ground: Color32) -> Color32 {
        mix(ground, self.ink, WASH)
    }
}

/// `over` at `amount` on `ground`, mixed in sRGB as the pages' `color-mix` does.
fn mix(ground: Color32, over: Color32, amount: f32) -> Color32 {
    let channel =
        |g: u8, o: u8| (f32::from(g) + (f32::from(o) - f32::from(g)) * amount).round() as u8;
    Color32::from_rgb(
        channel(ground.r(), over.r()),
        channel(ground.g(), over.g()),
        channel(ground.b(), over.b()),
    )
}

/// A status and its colours. Fault and attention are hard to tell apart by hue alone, so wherever
/// one is drawn, words or a glyph say which.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Something needs a look: the jumper, going back to stock, installing anyway.
    Attention,
    /// Something failed.
    Fault,
}

impl Status {
    /// For text and lines.
    #[must_use]
    pub fn ink(self, face: &Face) -> Color32 {
        match self {
            Status::Attention => face.attention_ink,
            Status::Fault => face.fault_ink,
        }
    }

    /// For areas, with [`Face::on_status`] on them.
    #[must_use]
    pub fn fill(self, face: &Face) -> Color32 {
        match self {
            Status::Attention => face.attention_fill,
            Status::Fault => face.fault_fill,
        }
    }
}

/// Spacing steps.
pub mod space {
    pub const XS: f32 = 4.0;
    pub const S: f32 = 8.0;
    pub const M: f32 = 16.0;
    pub const L: f32 = 24.0;
}

pub const RADIUS: u8 = 2;
/// An action slab's height.
pub const SLOT_HEIGHT: f32 = 48.0;

/// Text by what it does. Each is a named [`TextStyle`], so a face or size changes here alone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The slab's one word.
    Display,
    /// The window's name in the strip.
    Title,
    Instruction,
    Action,
    /// Field names and tags.
    Label,
    /// Versions and counts set large.
    Tally,
    /// Figures that change in place: readings, timings, progress.
    Figures,
    Body,
    /// Serials, hashes, paths and log lines.
    Code,
}

impl Role {
    const ALL: [Role; 9] = [
        Role::Display,
        Role::Title,
        Role::Instruction,
        Role::Action,
        Role::Label,
        Role::Tally,
        Role::Figures,
        Role::Body,
        Role::Code,
    ];

    fn name(self) -> &'static str {
        match self {
            Role::Display => "display",
            Role::Title => "title",
            Role::Instruction => "instruction",
            Role::Action => "action",
            Role::Label => "label",
            Role::Tally => "tally",
            Role::Figures => "figures",
            Role::Body => "body",
            Role::Code => "code",
        }
    }

    fn font(self) -> FontId {
        let (size, family) = match self {
            Role::Display => (52.0, Family::Display),
            Role::Title => (18.0, Family::Display),
            Role::Instruction => (18.0, Family::TextStrong),
            Role::Action => (15.0, Family::TextMedium),
            Role::Label => (12.5, Family::TextMedium),
            Role::Tally => (32.0, Family::Figures),
            Role::Figures => (12.0, Family::Figures),
            Role::Body => (14.0, Family::Text),
            Role::Code => (12.0, Family::Code),
        };
        FontId::new(size, family.family())
    }

    #[must_use]
    pub fn style(self) -> TextStyle {
        TextStyle::Name(self.name().into())
    }
}

/// `text` set in `role`.
pub fn text(role: Role, text: impl Into<String>) -> RichText {
    RichText::new(text).text_style(role.style())
}

/// The families the roles draw from, one per font file.
#[derive(Clone, Copy)]
enum Family {
    /// Saira, expanded, 500.
    Display,
    /// Hanken Grotesk 400.
    Text,
    /// Hanken Grotesk 500.
    TextMedium,
    /// Hanken Grotesk 600.
    TextStrong,
    /// Martian Mono, semi-expanded, 500.
    Figures,
    /// Martian Mono 500.
    Code,
}

impl Family {
    const ALL: [Family; 6] = [
        Family::Display,
        Family::Text,
        Family::TextMedium,
        Family::TextStrong,
        Family::Figures,
        Family::Code,
    ];

    /// The file under `fonts/`, by the name `tokens::FONTS` gives it, and its bytes.
    fn source(self) -> (&'static str, &'static [u8]) {
        macro_rules! font {
            ($file:literal) => {
                ($file, include_bytes!(concat!("../fonts/", $file)))
            };
        }
        match self {
            Family::Display => font!("Saira-Expanded-Medium.ttf"),
            Family::Text => font!("HankenGrotesk-Regular.ttf"),
            Family::TextMedium => font!("HankenGrotesk-Medium.ttf"),
            Family::TextStrong => font!("HankenGrotesk-SemiBold.ttf"),
            Family::Figures => font!("MartianMono-SemiExpanded-Medium.ttf"),
            Family::Code => font!("MartianMono-Medium.ttf"),
        }
    }

    fn file(self) -> &'static str {
        self.source().0
    }

    fn family(self) -> FontFamily {
        match self {
            Family::Text => FontFamily::Proportional,
            Family::Code => FontFamily::Monospace,
            _ => FontFamily::Name(self.file().into()),
        }
    }
}

/// The licences of the faces above, for the About view and the distribution.
pub const FONT_LICENCES: &[(&str, &str)] = &[
    (
        "Saira Expanded Medium",
        include_str!("../fonts/saira-OFL.txt"),
    ),
    (
        "Hanken Grotesk Regular, Medium and SemiBold",
        include_str!("../fonts/hanken-grotesk-OFL.txt"),
    ),
    (
        "Martian Mono Medium and SemiExpanded Medium",
        include_str!("../fonts/martian-mono-OFL.txt"),
    ),
];

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    for family in Family::ALL {
        fonts.font_data.insert(
            family.file().to_owned(),
            Arc::new(FontData::from_static(family.source().1)),
        );
        match family.family() {
            named @ FontFamily::Name(_) => {
                let mut list = vec![family.file().to_owned()];
                list.extend(fallbacks.iter().cloned());
                fonts.families.insert(named, list);
            }
            builtin => fonts
                .families
                .entry(builtin)
                .or_default()
                .insert(0, family.file().to_owned()),
        }
    }
    fonts
}

/// Fonts, text styles and both faces' visuals for the window.
pub fn install(ctx: &Context) {
    ctx.set_fonts(fonts());
    for (theme, face) in [(Theme::Light, &DAY), (Theme::Dark, &NIGHT)] {
        ctx.style_mut_of(theme, |style| {
            for role in Role::ALL {
                style.text_styles.insert(role.style(), role.font());
            }
            style.text_styles.insert(TextStyle::Body, Role::Body.font());
            style
                .text_styles
                .insert(TextStyle::Button, Role::Action.font());
            style
                .text_styles
                .insert(TextStyle::Monospace, Role::Code.font());
            style.text_styles.insert(
                TextStyle::Heading,
                FontId::new(16.0, Family::Display.family()),
            );
            style.drag_value_text_style = Role::Figures.style();
            style.spacing.item_spacing = egui::vec2(space::S, space::S);
            style.spacing.button_padding = egui::vec2(space::M, space::S);
            style.visuals = visuals(theme, face);
        });
    }
}

fn visuals(theme: Theme, face: &Face) -> Visuals {
    let mut visuals = match theme {
        Theme::Light => Visuals::light(),
        Theme::Dark => Visuals::dark(),
    };
    let radius = CornerRadius::same(RADIUS);
    visuals.panel_fill = face.page;
    visuals.window_fill = face.raised;
    visuals.extreme_bg_color = face.sunken;
    visuals.text_edit_bg_color = Some(face.sunken);
    visuals.code_bg_color = face.sunken;
    visuals.faint_bg_color = face.surface;
    visuals.window_corner_radius = radius;
    visuals.menu_corner_radius = radius;
    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow::NONE;
    visuals.window_stroke = Stroke::new(1.0, face.rule);
    // Selected: an ink fill with page text, for chosen buttons and selected text alike.
    visuals.selection.bg_fill = face.ink;
    visuals.selection.stroke = Stroke::new(FOCUS_WIDTH, face.page);
    visuals.text_cursor.stroke = Stroke::new(FOCUS_WIDTH, face.ink);
    visuals.hyperlink_color = face.ink;
    visuals.override_text_color = None;
    visuals.weak_text_color = Some(face.ink_2);
    visuals.warn_fg_color = face.attention_ink;
    visuals.error_fg_color = face.fault_ink;
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = radius;
        widget.expansion = 0.0;
        widget.fg_stroke = Stroke::new(1.0, face.ink);
        widget.bg_stroke = Stroke::new(1.0, face.rule);
    }
    // Disabled widgets fade towards this.
    visuals.widgets.noninteractive.weak_bg_fill = face.page;
    visuals.widgets.noninteractive.bg_fill = face.surface;
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, face.hair);
    // Buttons are an outline over their ground, and the wash over it under the pointer. The
    // focus ring is drawn over everything by `focus_ring`.
    visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_fill = face.raised;
    for widget in [
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.weak_bg_fill = face.wash();
        widget.bg_fill = face.washed(face.raised);
    }
    visuals
}

/// The focus ring: [`FOCUS_WIDTH`] of ink just outside the widget with keyboard focus, drawn
/// after everything on its layer so that no fill covers it. Call it once a frame, last.
pub fn focus_ring(ctx: &Context) {
    let Some(id) = ctx.memory(egui::Memory::focused) else {
        return;
    };
    let Some(response) = ctx.read_response(id) else {
        return;
    };
    let ring = response.rect.expand(FOCUS_WIDTH);
    // A widget part scrolled out of sight keeps the ring to the part on show.
    let clip = if response.interact_rect == response.rect {
        ring.expand(FOCUS_WIDTH)
    } else {
        response.interact_rect
    };
    ctx.layer_painter(response.layer_id)
        .with_clip_rect(clip)
        .rect_stroke(
            ring,
            CornerRadius::same(RADIUS),
            Stroke::new(FOCUS_WIDTH, face(ctx).ink),
            egui::StrokeKind::Outside,
        );
}

/// Verge's app icon, beside its name in the strip. At launch it plays the mark's sequence once,
/// on the icon's own pixels, and rests on the icon.
#[derive(Default)]
pub struct Mark {
    /// The texture, its pixels, and whether they are the ones for a fine display.
    icon: Option<(bool, egui::TextureHandle, crate::mark::Icon)>,
    /// When the strip first drew it.
    shown: Option<f64>,
}

impl Mark {
    pub fn show(&mut self, ui: &mut egui::Ui) {
        const SIZE: f32 = 24.0;
        let ppp = ui.ctx().pixels_per_point();
        // The 24 px icon is fitted to whole pixels; a fine display takes the 48.
        let fine = ppp > 1.25;
        if self.icon.as_ref().is_none_or(|(was, _, _)| *was != fine) {
            let png: &[u8] = if fine {
                include_bytes!("../icons/verge-48.png")
            } else {
                include_bytes!("../icons/verge-24.png")
            };
            let icon = eframe::icon_data::from_png_bytes(png).expect("the mark is a PNG");
            let image = egui::ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            let texture = ui
                .ctx()
                .load_texture("verge-mark", image, egui::TextureOptions::LINEAR);
            // The icons are drawn in the day face's plate and lume, the same in both faces.
            let pixels =
                crate::mark::Icon::from_rgba(icon.width as usize, &icon.rgba, DAY.plate, DAY.lume);
            self.icon = Some((fine, texture, pixels));
        }
        let (rect, _) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::hover());
        // On the physical pixel grid, for the play and the icon alike, so the hand over between them
        // does not shift the mark by part of a pixel.
        let rect = egui::Rect::from_min_size(
            ((rect.min.to_vec2() * ppp).round() / ppp).to_pos2(),
            rect.size(),
        );
        let Some((_, texture, pixels)) = &self.icon else {
            return;
        };
        let now = ui.input(|i| i.time);
        let t = (now - *self.shown.get_or_insert(now)) as f32;
        if crate::mark::motion(ui.ctx()) && t < crate::mark::END {
            let colours = crate::mark::Colours::of(face(ui.ctx()), false);
            let frame = crate::mark::at(t);
            let geometry = if fine {
                &crate::mark::MASTER
            } else {
                &crate::mark::FITTED_24
            };
            // On a display whose pixels do not land on the icon's (a scale of 1.5, say) there is
            // no pixel-exact hand over to make: draw it with shapes.
            if (SIZE * ppp).round() as usize == pixels.side {
                crate::mark::paint_exact(
                    ui.painter(),
                    rect,
                    geometry,
                    &frame,
                    &colours,
                    ppp,
                    pixels,
                );
            } else {
                crate::mark::paint(ui.painter(), rect, geometry, &frame, &colours);
            }
            ui.ctx().request_repaint();
            return;
        }
        egui::Image::new(egui::load::SizedTexture::new(
            texture.id(),
            egui::vec2(SIZE, SIZE),
        ))
        .paint_at(ui, rect);
    }
}

/// WCAG contrast ratio between two colours.
#[cfg(test)]
#[must_use]
pub fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    fn luminance(c: egui::Color32) -> f32 {
        let channel = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(c.r()) + 0.7152 * channel(c.g()) + 0.0722 * channel(c.b())
    }
    let (la, lb) = (luminance(a), luminance(b));
    let (light, dark) = if la > lb { (la, lb) } else { (lb, la) };
    (light + 0.05) / (dark + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_embedded_font_is_one_the_tokens_name() {
        for family in Family::ALL {
            assert!(
                crate::tokens::FONTS
                    .iter()
                    .any(|(_, file)| *file == family.file()),
                "{} is not in tokens::FONTS",
                family.file()
            );
        }
    }

    #[test]
    fn the_wash_mixes_ink_into_its_ground() {
        // 12 % of the way from the day's raised white to its ink, channel by channel.
        assert_eq!(DAY.washed(DAY.raised), Color32::from_rgb(0xe2, 0xe3, 0xe4));
        assert_eq!(DAY.washed(DAY.ink), DAY.ink);
    }
}
