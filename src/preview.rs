//! File preview pane (waveterm-style): text, image, or directory listing.

use std::path::{Path, PathBuf};

use macroquad::prelude::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreviewKind {
    Text,
    Image,
    Dir,
    Binary,
}

#[derive(Debug)]
pub struct PreviewPane {
    pub path: PathBuf,
    pub kind: PreviewKind,
    /// Wrapped text lines (for Text kind).
    pub lines: Vec<String>,
    pub scroll: usize,
    pub error: Option<String>,
    /// Decoded image (for Image kind).
    image: Option<Texture2D>,
    img_size: Option<(u32, u32)>,
}

const MAX_TEXT: u64 = 4 * 1024 * 1024;

impl PreviewPane {
    pub fn open(path: String) -> Self {
        let path = PathBuf::from(path);
        let mut pane = Self {
            path,
            kind: PreviewKind::Binary,
            lines: Vec::new(),
            scroll: 0,
            error: None,
            image: None,
            img_size: None,
        };
        pane.load();
        pane
    }

    fn load(&mut self) {
        let p = &self.path;
        if !p.exists() {
            self.error = Some(format!("no such file: {}", p.display()));
            return;
        }
        if p.is_dir() {
            self.kind = PreviewKind::Dir;
            match std::fs::read_dir(p) {
                Ok(rd) => {
                    let mut names: Vec<String> = rd
                        .filter_map(|e| e.ok())
                        .map(|e| {
                            let is_dir = e.path().is_dir();
                            let name = e.file_name().to_string_lossy().into_owned();
                            if is_dir { format!("{name}/") } else { name }
                        })
                        .collect();
                    names.sort();
                    self.lines = names;
                }
                Err(e) => self.error = Some(e.to_string()),
            }
            return;
        }

        let is_image = matches!(
            p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref(),
            Some("png") | Some("jpg") | Some("jpeg") | Some("gif") | Some("bmp") | Some("webp") | Some("ico")
        );

        if is_image {
            match std::fs::read(p) {
                Ok(bytes) => match image::load_from_memory(&bytes) {
                    Ok(img) => {
                        let rgba = img.to_rgba8();
                        let (w, h) = (rgba.width(), rgba.height());
                        let tex = Texture2D::from_rgba8(w as u16, h as u16, rgba.as_raw());
                        tex.set_filter(FilterMode::Linear);
                        self.image = Some(tex);
                        self.img_size = Some((w, h));
                        self.kind = PreviewKind::Image;
                    }
                    Err(e) => self.error = Some(format!("image decode: {e}")),
                },
                Err(e) => self.error = Some(e.to_string()),
            }
            return;
        }

        // Text-ish: read up to MAX_TEXT bytes, lossy decode.
        match std::fs::metadata(p) {
            Ok(md) if md.len() > MAX_TEXT => {
                self.error = Some("file too large for preview (limit 4 MiB)".into());
                self.kind = PreviewKind::Binary;
                return;
            }
            _ => {}
        }
        match std::fs::read(p) {
            Ok(bytes) => {
                // Heuristic: NUL bytes → binary.
                if bytes.iter().take(4096).any(|&b| b == 0) {
                    self.kind = PreviewKind::Binary;
                    self.error = Some("binary file".into());
                    return;
                }
                self.kind = PreviewKind::Text;
                let text = String::from_utf8_lossy(&bytes);
                self.lines = text.lines().map(|l| l.to_string()).collect();
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    pub fn render(&mut self, rect: crate::layout::Rect, font: &Font, font_px: u16, theme: crate::theme::Theme) {
        draw_rectangle(rect.x, rect.y, rect.w, rect.h, color_u8!(theme.bg.0, theme.bg.1, theme.bg.2, 255));

        // Header: path
        draw_text_ex(
            &format!("{} — {}", self.path.display(), match self.kind {
                PreviewKind::Text => "text",
                PreviewKind::Image => "image",
                PreviewKind::Dir => "directory",
                PreviewKind::Binary => "binary",
            }),
            rect.x + 8.0,
            rect.y + font_px as f32 + 4.0,
            TextParams { font: Some(font), font_size: font_px, color: color_u8!(theme.tab_inactive_text.0, theme.tab_inactive_text.1, theme.tab_inactive_text.2, 255), ..Default::default() },
        );
        let header_h = font_px as f32 + 16.0;

        if let Some(err) = &self.error {
            draw_text_ex(err, rect.x + 8.0, rect.y + header_h + 16.0, TextParams {
                font: Some(font), font_size: font_px, color: color_u8!(theme.search_match.0, theme.search_match.1, theme.search_match.2, 255), ..Default::default()
            });
            return;
        }

        match self.kind {
            PreviewKind::Image => {
                if let (Some(tex), Some((w, h))) = (&self.image, self.img_size) {
                    let avail_w = rect.w - 16.0;
                    let avail_h = rect.h - header_h - 8.0;
                    let scale = (avail_w / w as f32).min(avail_h / h as f32).min(1.0);
                    let dw = w as f32 * scale;
                    let dh = h as f32 * scale;
                    draw_texture_ex(
                        tex,
                        rect.x + 8.0 + (avail_w - dw) / 2.0,
                        rect.y + header_h + (avail_h - dh) / 2.0,
                        WHITE,
                        DrawTextureParams { dest_size: Some(vec2(dw, dh)), ..Default::default() },
                    );
                }
            }
            PreviewKind::Text | PreviewKind::Dir => {
                let line_h = (font_px as f32 * 1.25).max(12.0);
                let visible = (avail_h(rect, header_h) / line_h) as usize;
                let start = self.scroll.min(self.lines.len().saturating_sub(visible));
                let mut y = rect.y + header_h;
                for line in self.lines.iter().skip(start).take(visible) {
                    let mut s = line.as_str();
                    // hard-wrap long lines
                    let max_chars = (rect.w / (font_px as f32 * 0.6)) as usize;
                    while s.len() > max_chars {
                        let (a, b) = s.split_at(max_chars);
                        draw_text_ex(a, rect.x + 8.0, y, TextParams { font: Some(font), font_size: font_px, color: color_u8!(theme.fg.0, theme.fg.1, theme.fg.2, 255), ..Default::default() });
                        y += line_h;
                        s = b;
                    }
                    draw_text_ex(s, rect.x + 8.0, y, TextParams { font: Some(font), font_size: font_px, color: color_u8!(theme.fg.0, theme.fg.1, theme.fg.2, 255), ..Default::default() });
                    y += line_h;
                }
            }
            PreviewKind::Binary => {}
        }
    }

    pub fn scroll_lines(&mut self, delta: i32) {
        self.scroll = (self.scroll as i32 + delta).max(0) as usize;
    }
}

fn avail_h(rect: crate::layout::Rect, header_h: f32) -> f32 {
    (rect.h - header_h).max(1.0)
}
