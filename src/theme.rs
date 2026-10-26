//! Follow the desktop's active theme.
//!
//! NIRUWM and NIRUBAR share one appearance contract: the selected theme file
//! supplies the palette, and `appearance.toml` may override semantic colours.
//! This reads the same files in the same order, so the window cannot look like
//! a different desktop. Nothing here writes a theme.

use std::path::PathBuf;

/// The colours this window draws with, as X11 pixel values.
///
/// These are the five keys a NIRUX theme file defines plus one derived shade
/// for raised surfaces, so a custom theme drives the window without needing a
/// new contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    pub background: u32,
    pub surface: u32,
    pub foreground: u32,
    pub muted: u32,
    pub border: u32,
    /// Theme files call the highlight colour `focus`.
    pub accent: u32,
}

impl Palette {
    /// A shade just above the background, for the list and the control panel.
    ///
    /// Derived rather than themed so the window keeps working with the five
    /// keys a theme actually defines.
    fn surface_from(background: u32) -> u32 {
        let r = (background >> 16) & 0xff;
        let g = (background >> 8) & 0xff;
        let b = background & 0xff;
        let lift = |v: u32| (v + 10).min(255);
        (lift(r) << 16) | (lift(g) << 8) | lift(b)
    }

    /// Linear blend between two X11 pixels. `amount` is how much of `to` is used.
    pub fn blend(from: u32, to: u32, amount: f32) -> u32 {
        let amount = amount.clamp(0.0, 1.0);
        let channel = |shift: u32| {
            let a = (from >> shift) & 0xff;
            let b = (to >> shift) & 0xff;
            (f32::from(a as u8) * (1.0 - amount) + f32::from(b as u8) * amount)
                .round()
                .clamp(0.0, 255.0) as u32
        };
        (channel(16) << 16) | (channel(8) << 8) | channel(0)
    }

    /// The screen area, deliberately lighter than the canvas behind it.
    pub fn screen(&self) -> u32 {
        Self::blend(self.background, self.foreground, 0.075)
    }

    /// A selected screen, still distinct from the canvas and the frame.
    pub fn screen_selected(&self) -> u32 {
        Self::blend(self.screen(), self.accent, 0.22)
    }

    /// A visible frame for every screen, not just the selected one.
    pub fn frame(&self) -> u32 {
        Self::blend(self.border, self.muted, 0.45)
    }

    pub fn error(&self) -> u32 {
        Self::blend(self.background, 0xe35c5c, 0.82)
    }

    pub fn warning(&self) -> u32 {
        Self::blend(self.background, 0xe0b050, 0.80)
    }
}

impl Palette {
    /// The Noir palette, used when no theme can be read at all. These are the
    /// values in the shipped theme file, not invented ones.
    pub fn fallback() -> Self {
        Self {
            background: 0x0f0f10,
            surface: Self::surface_from(0x0f0f10),
            foreground: 0xe5e5e5,
            muted: 0xa8a8a8,
            border: 0x2a2a2d,
            accent: 0x5a5a60,
        }
    }
}

fn config_dir() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".config")
        })
        .join("niruwm")
}

/// Read a `key = "rrggbb"` value from a TOML file without a TOML dependency.
///
/// The files are small and flat, and every colour is a six-digit string, so a
/// scan is enough and keeps the binary free of a parser it would otherwise use
/// for eight values.
fn colour(text: &str, key: &str) -> Option<u32> {
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || !line.starts_with(key) {
            continue;
        }
        let rest = line[key.len()..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let value = rest.trim().trim_matches('"');
        if value.len() == 6 && value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return u32::from_str_radix(value, 16).ok();
        }
    }
    None
}

fn selected_theme() -> Option<String> {
    let text = std::fs::read_to_string(config_dir().join("config.toml")).ok()?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("theme"))
        .and_then(|rest| rest.trim_start().strip_prefix('='))
        .map(|v| v.trim().trim_matches('"').to_string())
        .filter(|v| !v.is_empty())
}

/// The theme file, preferring a user copy in the config directory and falling
/// back to the one shipped inside the installed generation.
fn theme_text(theme: &str) -> Option<String> {
    let name = format!("{theme}.toml");
    let mut candidates = vec![config_dir().join("themes").join(&name)];
    if let Ok(exe) = std::env::current_exe() {
        // <prefix>/bin/niruwallpaper -> <prefix>/themes, and the generation's
        // own copy, which is where a suite install keeps it.
        if let Some(prefix) = exe.parent().and_then(|p| p.parent()) {
            candidates.push(prefix.join("themes").join(&name));
            candidates.push(prefix.join("share/themes").join(&name));
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        candidates.push(home.join(".local/share/nirux/current/themes").join(&name));
    }
    candidates
        .into_iter()
        .find_map(|path| std::fs::read_to_string(path).ok())
}

/// Load the palette for the selected theme, honouring `appearance.toml`
/// overrides in the `[colors]` section.
pub fn load() -> Palette {
    let mut palette = Palette::fallback();
    if let Some(theme) = selected_theme() {
        if let Some(text) = theme_text(&theme) {
            for (key, slot) in [
                ("background", &mut palette.background),
                ("surface", &mut palette.surface),
                ("foreground", &mut palette.foreground),
                ("muted", &mut palette.muted),
                ("border", &mut palette.border),
                ("focus", &mut palette.accent),
            ] {
                if let Some(value) = colour(&text, key) {
                    *slot = value;
                }
            }
            palette.surface = Palette::surface_from(palette.background);
        }
    }
    // `appearance.toml` carries commented examples outside `[colors]`, so only
    // read it when the section actually exists.
    if let Ok(text) = std::fs::read_to_string(config_dir().join("appearance.toml")) {
        if let Some(section) = text.split("[colors]").nth(1) {
            let section: String = section
                .lines()
                .take_while(|l| !l.trim_start().starts_with('['))
                .collect::<Vec<_>>()
                .join("\n");
            for (key, slot) in [
                ("background", &mut palette.background),
                ("surface", &mut palette.surface),
                ("foreground", &mut palette.foreground),
                ("muted", &mut palette.muted),
                ("border", &mut palette.border),
                ("accent", &mut palette.accent),
            ] {
                if let Some(value) = colour(&section, key) {
                    *slot = value;
                }
            }
            palette.surface = Palette::surface_from(palette.background);
        }
    }
    palette
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screens_and_semantic_colours_are_distinct() {
        let palette = Palette::fallback();
        assert_ne!(palette.screen(), palette.background);
        assert_ne!(palette.screen_selected(), palette.screen());
        assert_ne!(palette.error(), palette.background);
        assert_ne!(palette.warning(), palette.error());
        assert_ne!(palette.frame(), palette.background);
    }

    #[test]
    fn a_colour_is_read_from_a_theme_file() {
        let text = "background = \"0f0f10\"\nforeground = \"e5e5e5\"\n";
        assert_eq!(colour(text, "background"), Some(0x0f0f10));
        assert_eq!(colour(text, "foreground"), Some(0xe5e5e5));
    }

    #[test]
    fn commented_values_are_not_read() {
        let text = "# background = \"ffffff\"\nbackground = \"101010\"\n";
        assert_eq!(colour(text, "background"), Some(0x101010));
    }

    #[test]
    fn an_invalid_colour_is_ignored() {
        assert_eq!(colour("background = \"zzzzzz\"\n", "background"), None);
        assert_eq!(colour("background = \"fff\"\n", "background"), None);
        assert_eq!(colour("background = 123\n", "background"), None);
    }

    #[test]
    fn the_fallback_matches_the_shipped_noir_theme() {
        let noir = include_str!("../themes/niru-noir.toml");
        let p = Palette::fallback();
        assert_eq!(p.background, colour(noir, "background").unwrap());
        assert_eq!(p.foreground, colour(noir, "foreground").unwrap());
        assert_eq!(p.accent, colour(noir, "focus").unwrap());
        // The surface shade is derived from the background, never a fixed
        // colour, so a light theme does not get a dark panel.
        assert_eq!(p.surface, Palette::surface_from(p.background));
    }
}
