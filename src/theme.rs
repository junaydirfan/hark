//! Accent colours, as in trill: a theme is two RGB colours (accent → gradient end). Drawing
//! reads them from a thread-local set once per frame, so no draw function needs an argument.

use std::cell::Cell;

use ratatui::style::Color;

type Rgb = [u8; 3];

/// (name, accent, gradient end). Index 0 is the default.
pub const THEMES: [(&str, Rgb, Rgb); 6] = [
    ("amber", [255, 176, 59], [255, 95, 109]),
    ("ocean", [0, 180, 255], [130, 100, 255]),
    ("violet", [185, 120, 255], [255, 95, 175]),
    ("mint", [30, 215, 140], [0, 175, 235]),
    ("rose", [255, 115, 165], [255, 180, 95]),
    ("mono", [225, 225, 225], [130, 130, 140]),
];

pub fn name(i: usize) -> &'static str {
    THEMES[i % THEMES.len()].0
}

pub fn by_name(n: &str) -> usize {
    THEMES.iter().position(|t| t.0 == n).unwrap_or(0)
}

thread_local! {
    static CURRENT: Cell<(Rgb, Rgb)> = const { Cell::new(([255, 176, 59], [255, 95, 109])) };
}

pub fn set(i: usize) {
    let t = THEMES[i % THEMES.len()];
    CURRENT.with(|c| c.set((t.1, t.2)));
}

pub fn accent() -> Color {
    let [r, g, b] = CURRENT.with(Cell::get).0;
    Color::Rgb(r, g, b)
}

/// Text colour that stays readable on an accent background.
pub fn on_accent() -> Color {
    let [r, g, b] = CURRENT.with(Cell::get).0;
    if 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32 > 110.0 { Color::Black } else { Color::White }
}

/// Accent → gradient end, `t` in 0..=1.
pub fn grad(t: f32) -> Color {
    let (a, b) = CURRENT.with(Cell::get);
    let t = t.clamp(0.0, 1.0);
    let mix = |i: usize| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t) as u8;
    Color::Rgb(mix(0), mix(1), mix(2))
}
