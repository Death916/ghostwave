//! Color themes for the UI chrome (tab bar, frames, status bar, palette).
//!
//! Terminal content colors come from libghostty's built-in palette; the theme
//! controls default fg/bg (applied via terminal default color setters) and all
//! application chrome.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn hex(h: u32) -> Self {
        Self(((h >> 16) & 0xff) as u8, ((h >> 8) & 0xff) as u8, (h & 0xff) as u8)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Theme {
    pub name: &'static str,
    /// Terminal default background (also chrome background).
    pub bg: Rgb,
    /// Terminal default foreground.
    pub fg: Rgb,
    /// Cursor color hint.
    pub cursor: Rgb,
    /// Tab bar background.
    pub tab_bg: Rgb,
    /// Active tab background.
    pub tab_active: Rgb,
    /// Inactive tab text.
    pub tab_inactive_text: Rgb,
    /// Pane frame border (unfocused).
    pub frame: Rgb,
    /// Pane frame border (focused) + accent color.
    pub frame_active: Rgb,
    /// Status bar background.
    pub status_bg: Rgb,
    /// Status bar text.
    pub status_fg: Rgb,
    /// Command palette background.
    pub palette_bg: Rgb,
    /// Command palette text.
    pub palette_fg: Rgb,
    /// Command palette selected item highlight.
    pub palette_sel: Rgb,
    /// Selection highlight (inverse of fg).
    pub selection: Rgb,
    /// Search match highlight.
    pub search_match: Rgb,
}

const fn t(
    name: &'static str,
    bg: u32,
    fg: u32,
    cursor: u32,
    tab_bg: u32,
    tab_active: u32,
    tab_inactive_text: u32,
    frame: u32,
    frame_active: u32,
    status_bg: u32,
    status_fg: u32,
    palette_bg: u32,
    palette_fg: u32,
    palette_sel: u32,
    selection: u32,
    search_match: u32,
) -> Theme {
    Theme {
        name,
        bg: Rgb::hex(bg),
        fg: Rgb::hex(fg),
        cursor: Rgb::hex(cursor),
        tab_bg: Rgb::hex(tab_bg),
        tab_active: Rgb::hex(tab_active),
        tab_inactive_text: Rgb::hex(tab_inactive_text),
        frame: Rgb::hex(frame),
        frame_active: Rgb::hex(frame_active),
        status_bg: Rgb::hex(status_bg),
        status_fg: Rgb::hex(status_fg),
        palette_bg: Rgb::hex(palette_bg),
        palette_fg: Rgb::hex(palette_fg),
        palette_sel: Rgb::hex(palette_sel),
        selection: Rgb::hex(selection),
        search_match: Rgb::hex(search_match),
    }
}

pub const THEMES: &[Theme] = &[
    t("tokyonight", 0x1a1b26, 0xc0caf5, 0xc0caf5, 0x16161e, 0x283457, 0x565f89, 0x283457, 0x7aa2f7, 0x16161e, 0xa9b1d6, 0x16161e, 0xc0caf5, 0x283457, 0x283457, 0xe0af68),
    t("dracula", 0x282a36, 0xf8f8f2, 0xf8f8f2, 0x21222c, 0x44475a, 0x6272a4, 0x44475a, 0xbd93f9, 0x21222c, 0xf8f8f2, 0x21222c, 0xf8f8f2, 0x44475a, 0x44475a, 0xffb86c),
    t("gruvbox", 0x282828, 0xebdbb2, 0xebdbb2, 0x1d2021, 0x3c3836, 0x928374, 0x3c3836, 0xb8bb26, 0x1d2021, 0xebdbb2, 0x1d2021, 0xebdbb2, 0x3c3836, 0x3c3836, 0xfb4934),
    t("catppuccin", 0x1e1e2e, 0xcdd6f4, 0xcdd6f4, 0x181825, 0x313244, 0x6c7086, 0x313244, 0x89b4fa, 0x181825, 0xcdd6f4, 0x181825, 0xcdd6f4, 0x313244, 0x313244, 0xf9e2af),
    t("nord", 0x2e3440, 0xd8dee9, 0xd8dee9, 0x272c36, 0x3b4252, 0x616e88, 0x3b4252, 0x88c0d0, 0x272c36, 0xd8dee9, 0x272c36, 0xd8dee9, 0x3b4252, 0x3b4252, 0xebcb8b),
    t("solarized", 0x002b36, 0x839496, 0x839496, 0x00212c, 0x073642, 0x586e75, 0x073642, 0x2aa198, 0x00212c, 0x839496, 0x00212c, 0x839496, 0x073642, 0x073642, 0xb58900),
    t("monokai", 0x272822, 0xf8f8f2, 0xf8f8f2, 0x1f201b, 0x3e3d32, 0x75715e, 0x3e3d32, 0xa6e22e, 0x1f201b, 0xf8f8f2, 0x1f201b, 0xf8f8f2, 0x3e3d32, 0x3e3d32, 0xe6db74),
    t("ghostty", 0x1d1f21, 0xd5d5d5, 0xd5d5d5, 0x17181a, 0x2d2f31, 0x707880, 0x2d2f31, 0x3aa675, 0x17181a, 0xd5d5d5, 0x17181a, 0xd5d5d5, 0x2d2f31, 0x2d2f31, 0xfb7192),
];

pub fn by_name(name: &str) -> Theme {
    THEMES
        .iter()
        .copied()
        .find(|t| t.name == name)
        .unwrap_or(THEMES[0])
}

pub fn by_name_opt(name: &str) -> Option<Theme> {
    THEMES.iter().copied().find(|t| t.name == name)
}

/// Theme index by name (for palette listing).
pub fn names() -> Vec<&'static str> {
    THEMES.iter().map(|t| t.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn by_name_known_and_unknown() {
        assert_eq!(by_name("tokyonight").name, "tokyonight");
        assert_eq!(by_name("dracula").name, "dracula");
        // unknown falls back to first theme — and we verify it's the actual first element, not a copy
        let fallback = by_name("not-a-theme");
        assert_eq!(fallback.name, THEMES[0].name);
        assert_eq!(fallback.bg, THEMES[0].bg, "fallback should be THEMES[0] exactly");
    }

    #[test]
    fn by_name_opt_none_for_unknown() {
        assert!(by_name_opt("tokyonight").is_some());
        assert!(by_name_opt("nope").is_none());
    }

    #[test]
    fn names_matches_themes_len() {
        let n = names();
        assert_eq!(n.len(), THEMES.len());
        assert!(n.contains(&"tokyonight"));
        assert!(n.contains(&"gruvbox"));
        // all names unique
        let mut dedup = n.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(dedup.len(), n.len());
    }

    #[test]
    fn rgb_hex_splits_correctly() {
        assert_eq!(Rgb::hex(0x1a2b3c), Rgb(0x1a, 0x2b, 0x3c));
        assert_eq!(Rgb::hex(0xffffff), Rgb(255, 255, 255));
        assert_eq!(Rgb::hex(0x000000), Rgb(0, 0, 0));
    }
}
