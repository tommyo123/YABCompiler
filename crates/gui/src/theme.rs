//! Color themes for egui.
//!
//! Latte is the light variant and the default. Frappé, Macchiato and
//! Mocha are progressively darker variants the user can pick from the
//! View menu.

use eframe::egui;
use egui::{Color32, CornerRadius, Stroke, Visuals, style};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flavor {
    #[default]
    Latte,
    Frappe,
    Macchiato,
    Mocha,
}

impl Flavor {
    pub fn label(self) -> &'static str {
        match self {
            Flavor::Latte => "Latte (light)",
            Flavor::Frappe => "Frappé",
            Flavor::Macchiato => "Macchiato",
            Flavor::Mocha => "Mocha",
        }
    }

    pub fn palette(self) -> Palette {
        match self {
            Flavor::Latte => Palette::LATTE,
            Flavor::Frappe => Palette::FRAPPE,
            Flavor::Macchiato => Palette::MACCHIATO,
            Flavor::Mocha => Palette::MOCHA,
        }
    }

    pub fn is_dark(self) -> bool {
        !matches!(self, Flavor::Latte)
    }

    /// Stable identifier for persistence — never changes even if
    /// `label()` is reworded, so a saved preference keeps loading.
    pub fn key(self) -> &'static str {
        match self {
            Flavor::Latte => "latte",
            Flavor::Frappe => "frappe",
            Flavor::Macchiato => "macchiato",
            Flavor::Mocha => "mocha",
        }
    }

    /// Parse a stored [`Flavor::key`] back to a flavor.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "latte" => Some(Flavor::Latte),
            "frappe" => Some(Flavor::Frappe),
            "macchiato" => Some(Flavor::Macchiato),
            "mocha" => Some(Flavor::Mocha),
            _ => None,
        }
    }
}

#[allow(dead_code)] // full palette exposed for future syntax/log highlighting
pub struct Palette {
    pub rosewater: Color32,
    pub flamingo: Color32,
    pub pink: Color32,
    pub mauve: Color32,
    pub red: Color32,
    pub maroon: Color32,
    pub peach: Color32,
    pub yellow: Color32,
    pub green: Color32,
    pub teal: Color32,
    pub sky: Color32,
    pub sapphire: Color32,
    pub blue: Color32,
    pub lavender: Color32,
    pub text: Color32,
    pub subtext1: Color32,
    pub subtext0: Color32,
    pub overlay2: Color32,
    pub overlay1: Color32,
    pub overlay0: Color32,
    pub surface2: Color32,
    pub surface1: Color32,
    pub surface0: Color32,
    pub base: Color32,
    pub mantle: Color32,
    pub crust: Color32,
}

const fn rgb(r: u8, g: u8, b: u8) -> Color32 {
    Color32::from_rgb(r, g, b)
}

impl Palette {
    pub const LATTE: Palette = Palette {
        rosewater: rgb(0xdc, 0x8a, 0x78),
        flamingo: rgb(0xdd, 0x78, 0x78),
        pink: rgb(0xea, 0x76, 0xcb),
        mauve: rgb(0x88, 0x39, 0xef),
        red: rgb(0xd2, 0x0f, 0x39),
        maroon: rgb(0xe6, 0x45, 0x53),
        peach: rgb(0xfe, 0x64, 0x0b),
        yellow: rgb(0xdf, 0x8e, 0x1d),
        green: rgb(0x40, 0xa0, 0x2b),
        teal: rgb(0x17, 0x92, 0x99),
        sky: rgb(0x04, 0xa5, 0xe5),
        sapphire: rgb(0x20, 0x9f, 0xb5),
        blue: rgb(0x1e, 0x66, 0xf5),
        lavender: rgb(0x72, 0x87, 0xfd),
        text: rgb(0x4c, 0x4f, 0x69),
        subtext1: rgb(0x5c, 0x5f, 0x77),
        subtext0: rgb(0x6c, 0x6f, 0x85),
        overlay2: rgb(0x7c, 0x7f, 0x93),
        overlay1: rgb(0x8c, 0x8f, 0xa1),
        overlay0: rgb(0x9c, 0xa0, 0xb0),
        surface2: rgb(0xac, 0xb0, 0xbe),
        surface1: rgb(0xbc, 0xc0, 0xcc),
        surface0: rgb(0xcc, 0xd0, 0xda),
        base: rgb(0xef, 0xf1, 0xf5),
        mantle: rgb(0xe6, 0xe9, 0xef),
        crust: rgb(0xdc, 0xe0, 0xe8),
    };

    pub const FRAPPE: Palette = Palette {
        rosewater: rgb(0xf2, 0xd5, 0xcf),
        flamingo: rgb(0xee, 0xbe, 0xbe),
        pink: rgb(0xf4, 0xb8, 0xe4),
        mauve: rgb(0xca, 0x9e, 0xe6),
        red: rgb(0xe7, 0x82, 0x84),
        maroon: rgb(0xea, 0x99, 0x9c),
        peach: rgb(0xef, 0x9f, 0x76),
        yellow: rgb(0xe5, 0xc8, 0x90),
        green: rgb(0xa6, 0xd1, 0x89),
        teal: rgb(0x81, 0xc8, 0xbe),
        sky: rgb(0x99, 0xd1, 0xdb),
        sapphire: rgb(0x85, 0xc1, 0xdc),
        blue: rgb(0x8c, 0xaa, 0xee),
        lavender: rgb(0xba, 0xbb, 0xf1),
        text: rgb(0xc6, 0xd0, 0xf5),
        subtext1: rgb(0xb5, 0xbf, 0xe2),
        subtext0: rgb(0xa5, 0xad, 0xce),
        overlay2: rgb(0x94, 0x9c, 0xbb),
        overlay1: rgb(0x83, 0x8b, 0xa7),
        overlay0: rgb(0x73, 0x7a, 0x94),
        surface2: rgb(0x62, 0x68, 0x80),
        surface1: rgb(0x51, 0x57, 0x6d),
        surface0: rgb(0x41, 0x45, 0x59),
        base: rgb(0x30, 0x34, 0x46),
        mantle: rgb(0x29, 0x2c, 0x3c),
        crust: rgb(0x23, 0x26, 0x34),
    };

    pub const MACCHIATO: Palette = Palette {
        rosewater: rgb(0xf4, 0xdb, 0xd6),
        flamingo: rgb(0xf0, 0xc6, 0xc6),
        pink: rgb(0xf5, 0xbd, 0xe6),
        mauve: rgb(0xc6, 0xa0, 0xf6),
        red: rgb(0xed, 0x87, 0x96),
        maroon: rgb(0xee, 0x99, 0xa0),
        peach: rgb(0xf5, 0xa9, 0x7f),
        yellow: rgb(0xee, 0xd4, 0x9f),
        green: rgb(0xa6, 0xda, 0x95),
        teal: rgb(0x8b, 0xd5, 0xca),
        sky: rgb(0x91, 0xd7, 0xe3),
        sapphire: rgb(0x7d, 0xc4, 0xe4),
        blue: rgb(0x8a, 0xad, 0xf4),
        lavender: rgb(0xb7, 0xbd, 0xf4),
        text: rgb(0xca, 0xd3, 0xf5),
        subtext1: rgb(0xb8, 0xc0, 0xe0),
        subtext0: rgb(0xa5, 0xad, 0xcb),
        overlay2: rgb(0x93, 0x9a, 0xb7),
        overlay1: rgb(0x80, 0x87, 0xa2),
        overlay0: rgb(0x6e, 0x73, 0x8d),
        surface2: rgb(0x5b, 0x60, 0x78),
        surface1: rgb(0x49, 0x4d, 0x64),
        surface0: rgb(0x36, 0x3a, 0x4f),
        base: rgb(0x24, 0x27, 0x3a),
        mantle: rgb(0x1e, 0x20, 0x30),
        crust: rgb(0x18, 0x19, 0x26),
    };

    pub const MOCHA: Palette = Palette {
        rosewater: rgb(0xf5, 0xe0, 0xdc),
        flamingo: rgb(0xf2, 0xcd, 0xcd),
        pink: rgb(0xf5, 0xc2, 0xe7),
        mauve: rgb(0xcb, 0xa6, 0xf7),
        red: rgb(0xf3, 0x8b, 0xa8),
        maroon: rgb(0xeb, 0xa0, 0xac),
        peach: rgb(0xfa, 0xb3, 0x87),
        yellow: rgb(0xf9, 0xe2, 0xaf),
        green: rgb(0xa6, 0xe3, 0xa1),
        teal: rgb(0x94, 0xe2, 0xd5),
        sky: rgb(0x89, 0xdc, 0xeb),
        sapphire: rgb(0x74, 0xc7, 0xec),
        blue: rgb(0x89, 0xb4, 0xfa),
        lavender: rgb(0xb4, 0xbe, 0xfe),
        text: rgb(0xcd, 0xd6, 0xf4),
        subtext1: rgb(0xba, 0xc2, 0xde),
        subtext0: rgb(0xa6, 0xad, 0xc8),
        overlay2: rgb(0x93, 0x99, 0xb2),
        overlay1: rgb(0x7f, 0x84, 0x9c),
        overlay0: rgb(0x6c, 0x70, 0x86),
        surface2: rgb(0x58, 0x5b, 0x70),
        surface1: rgb(0x45, 0x47, 0x5a),
        surface0: rgb(0x31, 0x32, 0x44),
        base: rgb(0x1e, 0x1e, 0x2e),
        mantle: rgb(0x18, 0x18, 0x25),
        crust: rgb(0x11, 0x11, 0x1b),
    };
}

/// Build an `egui::Visuals` from a theme flavor.
pub fn visuals(flavor: Flavor) -> Visuals {
    let p = flavor.palette();
    let mut v = if flavor.is_dark() {
        Visuals::dark()
    } else {
        Visuals::light()
    };

    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.blue;
    v.faint_bg_color = p.mantle;
    v.extreme_bg_color = p.crust;
    v.code_bg_color = p.mantle;
    v.warn_fg_color = p.peach;
    v.error_fg_color = p.red;
    v.window_fill = p.base;
    v.window_stroke = Stroke::new(1.0, p.surface1);
    v.panel_fill = p.base;
    v.window_corner_radius = CornerRadius::same(8);
    v.menu_corner_radius = CornerRadius::same(6);
    v.selection.bg_fill = with_alpha(p.blue, 90);
    v.selection.stroke = Stroke::new(1.0, p.blue);

    let widgets = &mut v.widgets;
    widgets.noninteractive = make_widget(p.mantle, p.surface1, p.subtext1, 1.0, 4);
    widgets.inactive = make_widget(p.surface0, p.surface1, p.text, 1.0, 6);
    widgets.hovered = make_widget(p.surface1, p.overlay0, p.text, 1.5, 6);
    widgets.active = make_widget(p.surface2, p.blue, p.text, 1.5, 6);
    widgets.open = make_widget(p.surface1, p.overlay1, p.text, 1.0, 6);

    v
}

fn make_widget(
    bg_fill: Color32,
    border: Color32,
    text: Color32,
    border_width: f32,
    radius: u8,
) -> style::WidgetVisuals {
    style::WidgetVisuals {
        bg_fill,
        weak_bg_fill: bg_fill,
        bg_stroke: Stroke::new(border_width, border),
        fg_stroke: Stroke::new(1.0, text),
        corner_radius: CornerRadius::same(radius),
        expansion: 0.0,
    }
}

fn with_alpha(c: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}
