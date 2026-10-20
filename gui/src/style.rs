//! Colour roles, spacing, and type by role.
//!
//! Every face is OFL 1.1 and ships unmodified with its licence in `fonts/`. They are inserted in
//! front of egui's own families, so egui's fonts stay as fallbacks for any glyph they lack.

use std::sync::Arc;

use egui::epaint::text::{FontData, FontDefinitions, FontFamily};
use egui::{Context, CornerRadius, FontId, RichText, Shadow, Stroke, TextStyle, Visuals};

/// Colour roles. Words and glyphs on a fill are always [`INK`].
pub mod color {
    use egui::Color32;

    pub const BACKGROUND: Color32 = Color32::from_rgb(0x12, 0x14, 0x18);
    /// Fields, the log, and the unit list's ground.
    pub const SUNKEN: Color32 = Color32::from_rgb(0x0C, 0x0D, 0x10);
    /// The selected unit, the strip, the band under the slab.
    pub const SURFACE: Color32 = Color32::from_rgb(0x1B, 0x1E, 0x24);
    /// Structure, disabled and neutral outlines.
    pub const BORDER: Color32 = Color32::from_rgb(0x35, 0x3B, 0x46);
    pub const TEXT: Color32 = Color32::from_rgb(0xE6, 0xE8, 0xEC);
    /// Labels and metadata; stock's identity fill.
    pub const TEXT_2: Color32 = Color32::from_rgb(0x8D, 0x96, 0xA6);
    /// Selection and focus: the selected unit and the chosen image source.
    pub const ACCENT: Color32 = Color32::from_rgb(0x4F, 0x9C, 0xF5);
    /// Our firmware's identity, installing it, and an update that finished.
    pub const OURS: Color32 = Color32::from_rgb(0xC0, 0xFE, 0x04);
    /// Attention and consequential choices: the jumper, going back to stock, installing anyway.
    pub const ATTENTION: Color32 = Color32::from_rgb(0xF1, 0x71, 0x0D);
    /// A genuine fault: an update that failed. Nothing else.
    pub const FAULT: Color32 = Color32::from_rgb(0xF2, 0x47, 0x23);
    pub const INK: Color32 = Color32::from_rgb(0x0B, 0x0C, 0x0E);
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
    /// The slab's one word. A placeholder face until a display face is chosen.
    Display,
    Title,
    Instruction,
    Action,
    /// Uppercase field names and tags.
    Label,
    /// Versions and counts set large.
    Tally,
    Body,
    /// Serials, hashes, paths and log lines.
    Data,
}

impl Role {
    const ALL: [Role; 8] = [
        Role::Display,
        Role::Title,
        Role::Instruction,
        Role::Action,
        Role::Label,
        Role::Tally,
        Role::Body,
        Role::Data,
    ];

    fn name(self) -> &'static str {
        match self {
            Role::Display => "display",
            Role::Title => "title",
            Role::Instruction => "instruction",
            Role::Action => "action",
            Role::Label => "label",
            Role::Tally => "tally",
            Role::Body => "body",
            Role::Data => "data",
        }
    }

    fn font(self) -> FontId {
        let (size, family) = match self {
            Role::Display => (64.0, Face::Display),
            Role::Title => (22.0, Face::Condensed),
            Role::Instruction => (20.0, Face::Condensed),
            Role::Action => (17.0, Face::Condensed),
            Role::Label => (11.0, Face::Label),
            Role::Tally => (40.0, Face::Label),
            Role::Body => (14.0, Face::Body),
            Role::Data => (12.5, Face::Data),
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

#[derive(Clone, Copy)]
enum Face {
    /// The display role's own family, so swapping its face touches nothing else.
    Display,
    Condensed,
    Label,
    Body,
    Data,
}

impl Face {
    fn family(self) -> FontFamily {
        match self {
            Face::Display => FontFamily::Name("display".into()),
            Face::Condensed => FontFamily::Name("condensed".into()),
            Face::Label => FontFamily::Name("label".into()),
            Face::Body => FontFamily::Proportional,
            Face::Data => FontFamily::Monospace,
        }
    }
}

const CONDENSED: &[u8] = include_bytes!("../fonts/IBMPlexSansCondensed-Medium.ttf");
const BODY: &[u8] = include_bytes!("../fonts/IBMPlexSans-Light.ttf");
const DATA: &[u8] = include_bytes!("../fonts/IBMPlexMono-Regular.ttf");
const LABEL: &[u8] = include_bytes!("../fonts/KodeMono-Bold.ttf");

/// The licences of the faces above, for the About view and the distribution.
pub const FONT_LICENCES: &[(&str, &str)] = &[
    (
        "IBM Plex Sans Condensed Medium, IBM Plex Sans Light, IBM Plex Mono Regular",
        include_str!("../fonts/IBMPlex-LICENSE.txt"),
    ),
    ("Kode Mono Bold", include_str!("../fonts/KodeMono-OFL.txt")),
];

fn fonts() -> FontDefinitions {
    let mut fonts = FontDefinitions::default();
    for (name, bytes) in [
        ("plex-sans-condensed-medium", CONDENSED),
        ("plex-sans-light", BODY),
        ("plex-mono-regular", DATA),
        ("kode-mono-bold", LABEL),
    ] {
        fonts
            .font_data
            .insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
    }
    let fallbacks = fonts
        .families
        .get(&FontFamily::Proportional)
        .cloned()
        .unwrap_or_default();
    let with_fallbacks = |first: &str| {
        let mut list = vec![first.to_owned()];
        list.extend(fallbacks.iter().cloned());
        list
    };
    fonts.families.insert(
        Face::Display.family(),
        with_fallbacks("plex-sans-condensed-medium"),
    );
    fonts.families.insert(
        Face::Condensed.family(),
        with_fallbacks("plex-sans-condensed-medium"),
    );
    fonts
        .families
        .insert(Face::Label.family(), with_fallbacks("kode-mono-bold"));
    for (family, first) in [
        (FontFamily::Proportional, "plex-sans-light"),
        (FontFamily::Monospace, "plex-mono-regular"),
    ] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, first.to_owned());
    }
    fonts
}

/// Fonts, text styles and visuals for the window.
pub fn install(ctx: &Context) {
    ctx.set_fonts(fonts());
    ctx.set_visuals(visuals());
    ctx.all_styles_mut(|style| {
        for role in Role::ALL {
            style.text_styles.insert(role.style(), role.font());
        }
        style.text_styles.insert(TextStyle::Body, Role::Body.font());
        style
            .text_styles
            .insert(TextStyle::Button, Role::Action.font());
        style
            .text_styles
            .insert(TextStyle::Monospace, Role::Data.font());
        style.spacing.item_spacing = egui::vec2(space::S, space::S);
        style.spacing.button_padding = egui::vec2(space::M, space::S);
        style.visuals = visuals();
    });
}

fn visuals() -> Visuals {
    let mut visuals = Visuals::dark();
    let radius = CornerRadius::same(RADIUS);
    visuals.panel_fill = color::BACKGROUND;
    visuals.window_fill = color::SURFACE;
    visuals.extreme_bg_color = color::SUNKEN;
    visuals.faint_bg_color = color::SURFACE;
    visuals.window_corner_radius = radius;
    visuals.menu_corner_radius = radius;
    visuals.window_shadow = Shadow::NONE;
    visuals.popup_shadow = Shadow::NONE;
    visuals.window_stroke = Stroke::new(1.0, color::BORDER);
    visuals.selection.bg_fill = color::ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, color::INK);
    visuals.hyperlink_color = color::ACCENT;
    visuals.override_text_color = Some(color::TEXT);
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = radius;
        widget.expansion = 0.0;
    }
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, color::BORDER);
    visuals.widgets.inactive.bg_fill = color::SURFACE;
    visuals.widgets.inactive.weak_bg_fill = color::SURFACE;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, color::BORDER);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, color::ACCENT);
    visuals.widgets.active.bg_stroke = Stroke::new(2.0, color::ACCENT);
    visuals
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
