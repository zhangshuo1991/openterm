//! Design tokens and runtime color-scheme support.
//!
//! Four built-in palettes: DarkTeal (default), DarkBlue, Dracula, Light.
//! The active palette is stored in a thread-local so every UI function
//! reads the current scheme without needing to pass state through every call.
#![allow(dead_code)]

use iced::{Color, Font};
use std::cell::Cell;

// ── Color scheme enum ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorScheme {
    #[default]
    DarkTeal,
    DarkBlue,
    Dracula,
    Light,
}

impl ColorScheme {
    pub const ALL: &'static [ColorScheme] = &[
        ColorScheme::DarkTeal,
        ColorScheme::DarkBlue,
        ColorScheme::Dracula,
        ColorScheme::Light,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ColorScheme::DarkTeal => "Dark Teal",
            ColorScheme::DarkBlue => "Dark Blue",
            ColorScheme::Dracula  => "Dracula",
            ColorScheme::Light    => "Light",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "DarkBlue" => Self::DarkBlue,
            "Dracula"  => Self::Dracula,
            "Light"    => Self::Light,
            _          => Self::DarkTeal,
        }
    }

    pub fn to_str(self) -> &'static str {
        match self {
            Self::DarkTeal => "DarkTeal",
            Self::DarkBlue => "DarkBlue",
            Self::Dracula  => "Dracula",
            Self::Light    => "Light",
        }
    }
}

// ── Palette struct ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub struct Palette {
    pub surface_0: Color,
    pub surface_1: Color,
    pub surface_2: Color,
    pub surface_3: Color,
    pub text_high: Color,
    pub text_muted: Color,
    pub text_dim: Color,
    pub accent: Color,
    pub accent_strong: Color,
    pub accent_alpha: f32,
    pub status_ok: Color,
    pub status_warn: Color,
    pub status_error: Color,
    pub status_idle: Color,
    pub border_subtle_alpha: f32,
    pub border_strong_alpha: f32,
}

const fn rgb(r: f32, g: f32, b: f32) -> Color { Color::from_rgb(r, g, b) }

const DARK_TEAL: Palette = Palette {
    surface_0: rgb(0.022, 0.026, 0.031),
    surface_1: rgb(0.050, 0.058, 0.067),
    surface_2: rgb(0.072, 0.083, 0.095),
    surface_3: rgb(0.098, 0.112, 0.128),
    text_high:  rgb(0.910, 0.925, 0.923),
    text_muted: rgb(0.600, 0.625, 0.632),
    text_dim:   rgb(0.380, 0.405, 0.412),
    accent:        rgb(0.235, 0.620, 0.560),
    accent_strong: rgb(0.420, 0.800, 0.720),
    accent_alpha: 0.140,
    status_ok:    rgb(0.360, 0.760, 0.520),
    status_warn:  rgb(0.880, 0.680, 0.280),
    status_error: rgb(0.890, 0.330, 0.360),
    status_idle:  rgb(0.430, 0.460, 0.470),
    border_subtle_alpha: 0.060,
    border_strong_alpha: 0.150,
};

const DARK_BLUE: Palette = Palette {
    surface_0: rgb(0.020, 0.025, 0.040),
    surface_1: rgb(0.045, 0.055, 0.085),
    surface_2: rgb(0.065, 0.080, 0.120),
    surface_3: rgb(0.090, 0.110, 0.160),
    text_high:  rgb(0.900, 0.920, 0.960),
    text_muted: rgb(0.580, 0.610, 0.700),
    text_dim:   rgb(0.350, 0.380, 0.460),
    accent:        rgb(0.220, 0.530, 0.940),
    accent_strong: rgb(0.420, 0.700, 1.000),
    accent_alpha: 0.150,
    status_ok:    rgb(0.300, 0.780, 0.500),
    status_warn:  rgb(0.940, 0.720, 0.260),
    status_error: rgb(0.920, 0.330, 0.360),
    status_idle:  rgb(0.420, 0.450, 0.530),
    border_subtle_alpha: 0.070,
    border_strong_alpha: 0.160,
};

const DRACULA: Palette = Palette {
    surface_0: rgb(0.157, 0.165, 0.212),
    surface_1: rgb(0.180, 0.188, 0.235),
    surface_2: rgb(0.220, 0.228, 0.280),
    surface_3: rgb(0.260, 0.270, 0.330),
    text_high:  rgb(0.973, 0.973, 0.949),
    text_muted: rgb(0.745, 0.757, 0.812),
    text_dim:   rgb(0.500, 0.510, 0.560),
    accent:        rgb(0.741, 0.576, 0.976),  // purple
    accent_strong: rgb(0.860, 0.730, 1.000),
    accent_alpha: 0.160,
    status_ok:    rgb(0.314, 0.980, 0.482),
    status_warn:  rgb(0.996, 0.965, 0.639),
    status_error: rgb(1.000, 0.333, 0.333),
    status_idle:  rgb(0.560, 0.576, 0.640),
    border_subtle_alpha: 0.080,
    border_strong_alpha: 0.180,
};

const LIGHT: Palette = Palette {
    surface_0: rgb(0.965, 0.968, 0.972),
    surface_1: rgb(0.940, 0.943, 0.950),
    surface_2: rgb(0.900, 0.905, 0.915),
    surface_3: rgb(0.855, 0.862, 0.875),
    text_high:  rgb(0.090, 0.100, 0.115),
    text_muted: rgb(0.380, 0.400, 0.430),
    text_dim:   rgb(0.580, 0.600, 0.630),
    accent:        rgb(0.040, 0.480, 0.430),
    accent_strong: rgb(0.020, 0.350, 0.320),
    accent_alpha: 0.120,
    status_ok:    rgb(0.120, 0.580, 0.280),
    status_warn:  rgb(0.700, 0.460, 0.040),
    status_error: rgb(0.780, 0.140, 0.140),
    status_idle:  rgb(0.520, 0.540, 0.560),
    border_subtle_alpha: 0.120,
    border_strong_alpha: 0.250,
};

impl ColorScheme {
    fn palette(self) -> Palette {
        match self {
            Self::DarkTeal => DARK_TEAL,
            Self::DarkBlue => DARK_BLUE,
            Self::Dracula  => DRACULA,
            Self::Light    => LIGHT,
        }
    }
    /// Accent color with alpha for soft fills.
    pub fn accent_soft(self) -> Color {
        let p = self.palette();
        Color { a: p.accent_alpha, ..p.accent }
    }
    /// Subtle border.
    pub fn border_subtle(self) -> Color {
        let p = self.palette();
        Color { a: p.border_subtle_alpha, ..p.text_high }
    }
    /// Strong border.
    pub fn border_strong(self) -> Color {
        let p = self.palette();
        Color { a: p.border_strong_alpha, ..p.accent_strong }
    }
}

// ── Thread-local active scheme ────────────────────────────────────────────────

thread_local! {
    static ACTIVE: Cell<ColorScheme> = const { Cell::new(ColorScheme::DarkTeal) };
    /// Optional runtime accent override (set from a user-chosen hex color).
    static ACCENT_OVERRIDE: Cell<Option<Color>> = const { Cell::new(None) };
}

pub fn set_scheme(s: ColorScheme) {
    ACTIVE.with(|c| c.set(s));
}

/// Parse a "#rrggbb" string and install it as the active accent override.
/// An empty / invalid string clears the override (back to the scheme accent).
pub fn set_accent_override(hex: &str) {
    ACCENT_OVERRIDE.with(|c| c.set(parse_hex(hex)));
}

/// Parse "#rrggbb" (or "rrggbb") into a Color. Returns None when malformed.
pub fn parse_hex(hex: &str) -> Option<Color> {
    let h = hex.trim().trim_start_matches('#');
    if h.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&h[0..2], 16).ok()?;
    let g = u8::from_str_radix(&h[2..4], 16).ok()?;
    let b = u8::from_str_radix(&h[4..6], 16).ok()?;
    Some(Color::from_rgb8(r, g, b))
}

/// The six built-in accent presets offered in the Appearance panel.
pub const ACCENT_PRESETS: [(&str, &str); 6] = [
    ("Teal", "#3c9e8f"),
    ("Blue", "#3887f0"),
    ("Purple", "#bd93f6"),
    ("Orange", "#e07b3d"),
    ("Green", "#5cb863"),
    ("Rose", "#e0518a"),
];

/// Terminal cursor shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
}

impl CursorShape {
    pub const ALL: [CursorShape; 3] = [CursorShape::Block, CursorShape::Underline, CursorShape::Beam];

    pub fn label(self) -> &'static str {
        match self {
            CursorShape::Block => "Block",
            CursorShape::Underline => "Underline",
            CursorShape::Beam => "Beam",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "underline" => Self::Underline,
            "beam" => Self::Beam,
            _ => Self::Block,
        }
    }

    pub fn to_str(self) -> &'static str {
        match self {
            Self::Block => "block",
            Self::Underline => "underline",
            Self::Beam => "beam",
        }
    }
}

pub fn current_scheme() -> ColorScheme {
    ACTIVE.with(|c| c.get())
}

fn p() -> Palette {
    current_scheme().palette()
}

// ── Accessor functions (replace the old pub const) ────────────────────────────

pub fn surface_0()      -> Color { p().surface_0 }
pub fn surface_1()      -> Color { p().surface_1 }
pub fn surface_2()      -> Color { p().surface_2 }
pub fn surface_3()      -> Color { p().surface_3 }
pub fn text_high()      -> Color { p().text_high }
pub fn text_muted()     -> Color { p().text_muted }
pub fn text_dim()       -> Color { p().text_dim }
pub fn accent()         -> Color { ACCENT_OVERRIDE.with(|c| c.get()).unwrap_or(p().accent) }
pub fn accent_strong()  -> Color {
    ACCENT_OVERRIDE
        .with(|c| c.get())
        .map(|c| lighten(c, 0.25))
        .unwrap_or(p().accent_strong)
}
pub fn accent_soft()    -> Color { Color { a: p().accent_alpha, ..accent() } }
pub fn status_ok()      -> Color { p().status_ok }
pub fn status_warn()    -> Color { p().status_warn }
pub fn status_error()   -> Color { p().status_error }
pub fn status_idle()    -> Color { p().status_idle }
pub fn border_subtle()  -> Color { Color { a: p().border_subtle_alpha, ..p().text_high } }
pub fn border_strong()  -> Color { Color { a: p().border_strong_alpha, ..p().accent_strong } }

// ── Layout & typography (unchanged) ──────────────────────────────────────────

pub const SIDEBAR_WIDTH: f32 = 260.0;
pub const SIDEBAR_MIN_WIDTH: f32 = 180.0;
pub const SIDEBAR_MAX_WIDTH: f32 = 420.0;
pub const SIDEBAR_DIVIDER_WIDTH: f32 = 5.0;
pub const TAB_BAR_HEIGHT: f32 = 38.0;
pub const TRAFFIC_LIGHT_INSET: f32 = 78.0;

pub const TERMINAL_FONT: Font = Font::with_name("Menlo");
pub const UI_FONT: Font = Font::DEFAULT;

pub const DEFAULT_FONT_SIZE: u16 = 14;
pub const MIN_FONT_SIZE: u16 = 10;
pub const MAX_FONT_SIZE: u16 = 24;

pub fn lighten(color: Color, amount: f32) -> Color {
    Color {
        r: color.r + (1.0 - color.r) * amount,
        g: color.g + (1.0 - color.g) * amount,
        b: color.b + (1.0 - color.b) * amount,
        a: color.a,
    }
}
pub fn darken(color: Color, amount: f32) -> Color {
    Color {
        r: color.r * (1.0 - amount),
        g: color.g * (1.0 - amount),
        b: color.b * (1.0 - amount),
        a: color.a,
    }
}
pub fn with_alpha(color: Color, alpha: f32) -> Color {
    Color { a: alpha, ..color }
}

/// Deterministic accent color for a tab, derived from its host/title string so
/// each saved host keeps a stable hue across sessions. Six well-spaced hues.
pub fn tab_accent(host: &str) -> Color {
    const PALETTE: [(f32, f32, f32); 6] = [
        (0.235, 0.620, 0.560), // teal
        (0.220, 0.530, 0.940), // blue
        (0.741, 0.576, 0.976), // purple
        (0.880, 0.480, 0.240), // orange
        (0.360, 0.720, 0.380), // green
        (0.860, 0.660, 0.260), // amber
    ];
    let h = host
        .bytes()
        .fold(0u32, |a, b| a.wrapping_mul(31).wrapping_add(u32::from(b)));
    let (r, g, b) = PALETTE[h as usize % PALETTE.len()];
    Color::from_rgb(r, g, b)
}

// Keep old uppercase constants as aliases for any remaining direct references.
pub const SURFACE_0: Color = Color::from_rgb(0.022, 0.026, 0.031);
pub const SURFACE_1: Color = Color::from_rgb(0.050, 0.058, 0.067);
pub const SURFACE_2: Color = Color::from_rgb(0.072, 0.083, 0.095);
pub const SURFACE_3: Color = Color::from_rgb(0.098, 0.112, 0.128);
pub const TEXT_HIGH: Color  = Color::from_rgb(0.910, 0.925, 0.923);
pub const TEXT_MUTED: Color = Color::from_rgb(0.600, 0.625, 0.632);
pub const TEXT_DIM: Color   = Color::from_rgb(0.380, 0.405, 0.412);
pub const ACCENT: Color        = Color::from_rgb(0.235, 0.620, 0.560);
pub const ACCENT_STRONG: Color = Color::from_rgb(0.420, 0.800, 0.720);
pub const ACCENT_SOFT: Color   = Color::from_rgba(0.300, 0.700, 0.620, 0.140);
pub const STATUS_OK: Color    = Color::from_rgb(0.360, 0.760, 0.520);
pub const STATUS_WARN: Color  = Color::from_rgb(0.880, 0.680, 0.280);
pub const STATUS_ERROR: Color = Color::from_rgb(0.890, 0.330, 0.360);
pub const STATUS_IDLE: Color  = Color::from_rgb(0.430, 0.460, 0.470);
pub const BORDER_SUBTLE: Color = Color::from_rgba(0.780, 0.850, 0.840, 0.060);
pub const BORDER_STRONG: Color = Color::from_rgba(0.780, 0.900, 0.870, 0.150);
