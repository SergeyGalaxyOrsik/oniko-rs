//! Colors and type, after the Electron app's shadcn neutral palette. Light and dark follow the system.

use gpui::{App, Global, Hsla, Rgba, Window, WindowAppearance, rgb};
use oniko_core::model::StatusColor;

#[derive(Clone, Copy)]
pub struct Theme {
    pub background: Hsla,
    pub foreground: Hsla,
    /// The window behind the inset card, and the sidebar.
    pub sidebar: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub accent: Hsla,
    pub border: Hsla,
    pub input: Hsla,
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub destructive: Hsla,
    pub ring: Hsla,
    pub selection: Hsla,
    pub code_background: Hsla,
    pub sky: Hsla,
    pub amber: Hsla,
    pub emerald: Hsla,
    pub red: Hsla,
    pub diff_added: Hsla,
    pub diff_removed: Hsla,
    pub dark: bool,
}

fn c(hex: u32) -> Hsla {
    rgb(hex).into()
}

fn ca(hex: u32, alpha: f32) -> Hsla {
    let Rgba { r, g, b, .. } = rgb(hex);
    Rgba { r, g, b, a: alpha }.into()
}

impl Theme {
    pub fn light() -> Theme {
        Theme {
            background: c(0xffffff),
            foreground: c(0x0a0a0a),
            sidebar: c(0xfafafa),
            muted: c(0xf5f5f5),
            muted_foreground: c(0x737373),
            accent: c(0xf0f0f0),
            border: c(0xe5e5e5),
            input: c(0xe5e5e5),
            primary: c(0x171717),
            primary_foreground: c(0xfafafa),
            destructive: c(0xdc2626),
            ring: c(0xa1a1a1),
            selection: ca(0x3b82f6, 0.25),
            code_background: c(0xf5f5f5),
            sky: c(0x0ea5e9),
            amber: c(0xf59e0b),
            emerald: c(0x10b981),
            red: c(0xef4444),
            diff_added: ca(0x10b981, 0.12),
            diff_removed: ca(0xef4444, 0.12),
            dark: false,
        }
    }

    pub fn dark() -> Theme {
        Theme {
            background: c(0x0a0a0a),
            foreground: c(0xfafafa),
            sidebar: c(0x171717),
            muted: c(0x262626),
            muted_foreground: c(0xa1a1a1),
            accent: c(0x262626),
            border: ca(0xffffff, 0.1),
            input: ca(0xffffff, 0.15),
            primary: c(0xe5e5e5),
            primary_foreground: c(0x171717),
            destructive: c(0xf87171),
            ring: c(0x737373),
            selection: ca(0x3b82f6, 0.35),
            code_background: c(0x171717),
            sky: c(0x38bdf8),
            amber: c(0xfbbf24),
            emerald: c(0x34d399),
            red: c(0xf87171),
            diff_added: ca(0x10b981, 0.18),
            diff_removed: ca(0xef4444, 0.18),
            dark: true,
        }
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Theme {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Theme::dark(),
            WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::light(),
        }
    }

    pub fn status(&self, color: StatusColor) -> Hsla {
        match color {
            StatusColor::Slate => c(if self.dark { 0x71717a } else { 0xa1a1aa }),
            StatusColor::Blue => c(0x3b82f6),
            StatusColor::Amber => c(0xf59e0b),
            StatusColor::Green => c(0x10b981),
            StatusColor::Violet => c(0x6366f1),
            StatusColor::Rose => c(0xf43f5e),
            StatusColor::Cyan => c(0x06b6d4),
            StatusColor::Orange => c(0xf97316),
        }
    }
}

impl Global for Theme {}

pub const SANS: &str = ".SystemUIFont";
pub const MONO: &str = "Menlo";

/// The theme for the current frame.
pub fn theme(cx: &App) -> Theme {
    *cx.global::<Theme>()
}

/// Picks the palette for the window's appearance; call when it changes.
pub fn sync_theme(window: &Window, cx: &mut App) {
    let theme = Theme::for_appearance(window.appearance());
    cx.set_global(theme);
}
