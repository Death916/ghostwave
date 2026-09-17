//! File preview pane for ghostwave iced front-end: text, directory, binary, or image.
//!
//! Provides file type detection, scrollable text line display with monospace font and line numbers,
//! directory sorting with icons/badges, hex dump and metadata views for binary files, and
//! zoom/pan visual preview for images.

use std::cmp::Ordering;
use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};

use ghostwave::theme::Theme as GhostTheme;
use iced::alignment;
use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path as CanvasPath, Stroke, Text as CanvasText};
use iced::widget::{column, container, row, scrollable, text, Canvas};
use iced::{
    Border, Color, Element, Fill, Font, Length, Padding, Pixels, Point, Rectangle,
    Renderer, Size, Theme,
};

/// Maximum file size (4 MiB) allowed for utf-8 text preview.
pub const MAX_TEXT: u64 = 4 * 1024 * 1024;

/// Maximum number of bytes (16 KiB) read for binary hex dump preview.
pub const MAX_HEX_BYTES: usize = 16 * 1024;

/// Categorization of previewable files.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PreviewKind {
    /// Utf-8 text with line numbers and monospace font.
    #[default]
    Text,
    /// Directory listing with folders sorted first and file icons.
    Dir,
    /// Binary data presented with metadata and hex dump.
    Binary,
    /// Image preview with dimensions, fit scaling, zoom, and pan offset.
    Image,
}

impl PreviewKind {
    /// Returns a short lowercase label for the preview kind.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Dir => "directory",
            Self::Binary => "binary",
            Self::Image => "image",
        }
    }
}

/// A directory entry representation with name, directory flag, and optional byte size.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntryItem {
    /// File or directory name.
    pub name: String,
    /// True if the entry is a directory.
    pub is_dir: bool,
    /// Size in bytes for regular files.
    pub size: Option<u64>,
}

/// A formatted row of hex dump data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HexDumpRow {
    /// Byte offset in the file.
    pub offset: usize,
    /// Space-separated hexadecimal bytes.
    pub hex: String,
    /// Printable ASCII representation of the bytes.
    pub ascii: String,
}

/// State and content for a file or directory preview pane.
#[derive(Clone, Debug)]
pub struct PreviewPane {
    /// Filesystem path of the previewed target.
    pub path: PathBuf,
    /// Detected preview kind.
    pub kind: PreviewKind,
    /// Lines of text, directory entries, or formatted hex dump rows.
    pub lines: Vec<String>,
    /// Structured directory entries when `kind` is `PreviewKind::Dir`.
    pub dir_entries: Vec<DirEntryItem>,
    /// Structured hex dump rows when `kind` is `PreviewKind::Binary`.
    pub hex_rows: Vec<HexDumpRow>,
    /// Scroll offset in lines.
    pub scroll: usize,
    /// Error message if loading or decoding failed.
    pub error: Option<String>,
    /// Natural dimensions `(width, height)` of an image file.
    pub img_size: Option<(u32, u32)>,
    /// Zoom magnification factor (1.0 = 100% / fit).
    pub zoom: f32,
    /// Horizontal pan offset in pixels.
    pub pan_x: f32,
    /// Vertical pan offset in pixels.
    pub pan_y: f32,
    /// Total file size in bytes from metadata.
    pub file_size: u64,
}

/// High-level navigation and zoom actions for a preview pane.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PreviewAction {
    /// Scrolls the viewport up by `n` lines.
    ScrollUp(usize),
    /// Scrolls the viewport down by `n` lines.
    ScrollDown(usize),
    /// Increases zoom level.
    ZoomIn,
    /// Decreases zoom level.
    ZoomOut,
    /// Resets scroll, zoom, and pan to initial defaults.
    ResetView,
}

impl PreviewPane {
    /// Opens and loads a preview pane for the given filesystem path.
    pub fn open(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut pane = Self {
            path,
            kind: PreviewKind::Binary,
            lines: Vec::new(),
            dir_entries: Vec::new(),
            hex_rows: Vec::new(),
            scroll: 0,
            error: None,
            img_size: None,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            file_size: 0,
        };
        pane.load();
        pane
    }

    /// Creates an empty preview pane with explicit kind and path without accessing the filesystem.
    pub fn empty(path: impl Into<PathBuf>, kind: PreviewKind) -> Self {
        Self {
            path: path.into(),
            kind,
            lines: Vec::new(),
            dir_entries: Vec::new(),
            hex_rows: Vec::new(),
            scroll: 0,
            error: None,
            img_size: None,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            file_size: 0,
        }
    }

    /// Loads or reloads file content, directory listing, image dimensions, or hex dump from disk.
    pub fn load(&mut self) {
        let p = &self.path;
        if !p.exists() {
            self.error = Some(format!("no such file: {}", p.display()));
            return;
        }

        if p.is_dir() {
            self.kind = PreviewKind::Dir;
            match fs::read_dir(p) {
                Ok(rd) => {
                    let mut entries: Vec<DirEntryItem> = rd
                        .filter_map(|e| e.ok())
                        .map(|e| {
                            let is_dir = e.path().is_dir();
                            let name = e.file_name().to_string_lossy().into_owned();
                            let size = if is_dir {
                                None
                            } else {
                                e.metadata().ok().map(|m| m.len())
                            };
                            DirEntryItem { name, is_dir, size }
                        })
                        .collect();

                    sort_dir_entries(&mut entries);

                    self.lines = entries
                        .iter()
                        .map(|e| {
                            if e.is_dir {
                                format!("{}/", e.name)
                            } else if let Some(sz) = e.size {
                                format!("{} ({})", e.name, format_size(sz))
                            } else {
                                e.name.clone()
                            }
                        })
                        .collect();

                    self.dir_entries = entries;
                }
                Err(e) => self.error = Some(e.to_string()),
            }
            return;
        }

        if let Ok(md) = fs::metadata(p) {
            self.file_size = md.len();
        }

        let is_image = p
            .extension()
            .and_then(|e| e.to_str())
            .map(is_image_extension)
            .unwrap_or(false);

        if is_image {
            self.kind = PreviewKind::Image;
            match fs::read(p) {
                Ok(bytes) => {
                    self.img_size = parse_image_dimensions(&bytes);
                }
                Err(e) => self.error = Some(e.to_string()),
            }
            return;
        }

        if self.file_size > MAX_TEXT {
            self.error = Some(format!(
                "file too large for text preview ({}, limit 4 MiB)",
                format_size(self.file_size)
            ));
            self.kind = PreviewKind::Binary;
            if let Ok(bytes) = fs::read(p) {
                let limit = bytes.len().min(MAX_HEX_BYTES);
                self.hex_rows = format_hex_dump(&bytes[..limit]);
                self.lines = self
                    .hex_rows
                    .iter()
                    .map(|r| format!("{:08x}: {}  |{}|", r.offset, r.hex, r.ascii))
                    .collect();
            }
            return;
        }

        match fs::read(p) {
            Ok(bytes) => {
                if detect_is_binary(&bytes) {
                    self.kind = PreviewKind::Binary;
                    let limit = bytes.len().min(MAX_HEX_BYTES);
                    self.hex_rows = format_hex_dump(&bytes[..limit]);
                    self.lines = self
                        .hex_rows
                        .iter()
                        .map(|r| format!("{:08x}: {}  |{}|", r.offset, r.hex, r.ascii))
                        .collect();
                    return;
                }

                self.kind = PreviewKind::Text;
                let text_content = String::from_utf8_lossy(&bytes);
                self.lines = text_content.lines().map(|l| l.to_string()).collect();
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Scrolls the viewport up by `delta` lines.
    pub fn scroll_up(&mut self, delta: usize) {
        self.scroll = self.scroll.saturating_sub(delta);
    }

    /// Scrolls the viewport down by `delta` lines.
    pub fn scroll_down(&mut self, delta: usize) {
        self.scroll = self.scroll.saturating_add(delta);
    }

    /// Scrolls by a signed number of lines (negative moves up, positive moves down).
    pub fn scroll_lines(&mut self, delta: i32) {
        if delta < 0 {
            self.scroll_up((-delta) as usize);
        } else {
            self.scroll_down(delta as usize);
        }
    }

    /// Increases the image zoom level by 25%, clamped at 10x.
    pub fn zoom_in(&mut self) {
        self.zoom = (self.zoom * 1.25).min(10.0);
    }

    /// Decreases the image zoom level by 25%, clamped at 0.1x.
    pub fn zoom_out(&mut self) {
        self.zoom = (self.zoom / 1.25).max(0.1);
    }

    /// Resets the zoom level to 1.0 and pan offsets to zero.
    pub fn reset_zoom(&mut self) {
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Resets viewport scroll, zoom factor, and pan offsets to initial defaults.
    pub fn reset_view(&mut self) {
        self.scroll = 0;
        self.zoom = 1.0;
        self.pan_x = 0.0;
        self.pan_y = 0.0;
    }

    /// Translates the image pan offset by `(dx, dy)` pixels.
    pub fn pan(&mut self, dx: f32, dy: f32) {
        self.pan_x += dx;
        self.pan_y += dy;
    }

    /// Applies a high-level preview action.
    pub fn handle_action(&mut self, action: PreviewAction) {
        match action {
            PreviewAction::ScrollUp(d) => self.scroll_up(d),
            PreviewAction::ScrollDown(d) => self.scroll_down(d),
            PreviewAction::ZoomIn => self.zoom_in(),
            PreviewAction::ZoomOut => self.zoom_out(),
            PreviewAction::ResetView => self.reset_view(),
        }
    }

    /// Builds the iced widget element tree for this preview pane.
    pub fn view<'a, Message: 'a>(&'a self, theme: &GhostTheme) -> Element<'a, Message> {
        let theme = *theme;
        let (badge_text, badge_color) = match self.kind {
            PreviewKind::Text => ("TEXT", rgb(theme.frame_active)),
            PreviewKind::Dir => ("DIR", rgb(theme.tab_active)),
            PreviewKind::Binary => ("BIN", rgb(theme.search_match)),
            PreviewKind::Image => ("IMG", rgb(theme.cursor)),
        };

        let badge = container(
            text(badge_text)
                .font(Font::MONOSPACE)
                .size(10.0)
                .color(rgb(theme.bg)),
        )
        .padding(Padding::from([2.0, 6.0]))
        .style(move |_theme| container::Style {
            background: Some(badge_color.into()),
            border: Border {
                radius: 2.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });

        let path_label = text(self.path.display().to_string())
            .font(Font::MONOSPACE)
            .size(13.0)
            .color(rgb(theme.fg));

        let meta_info = match self.kind {
            PreviewKind::Text => {
                format!("{} lines • {}", self.lines.len(), format_size(self.file_size))
            }
            PreviewKind::Dir => format!("{} entries", self.dir_entries.len()),
            PreviewKind::Binary => format_size(self.file_size),
            PreviewKind::Image => {
                let (w, h) = self.img_size.unwrap_or((0, 0));
                format!("{w} × {h} px • zoom {:.0}%", self.zoom * 100.0)
            }
        };

        let meta_label = text(meta_info)
            .font(Font::MONOSPACE)
            .size(12.0)
            .color(rgb(theme.tab_inactive_text));

        let header = container(
            row![
                badge,
                path_label,
                iced::widget::Space::new().width(Length::Fill),
                meta_label
            ]
            .spacing(10.0)
            .align_y(alignment::Vertical::Center),
        )
        .padding(Padding::from([6.0, 10.0]))
        .width(Fill)
        .style(move |_theme| container::Style {
            background: Some(rgb(theme.tab_bg).into()),
            border: Border {
                color: rgb(theme.frame),
                width: 1.0,
                radius: 0.0.into(),
            },
            ..container::Style::default()
        });

        let content_element: Element<'a, Message> = if let Some(err) = &self.error {
            container(
                column![
                    text("Preview Error")
                        .font(Font::MONOSPACE)
                        .size(14.0)
                        .color(rgb(theme.search_match)),
                    text(err.clone())
                        .font(Font::MONOSPACE)
                        .size(12.0)
                        .color(rgb(theme.tab_inactive_text)),
                ]
                .spacing(6.0),
            )
            .padding(16.0)
            .into()
        } else {
            match self.kind {
                PreviewKind::Text => {
                    let total = self.lines.len();
                    let num_width = if total < 100 {
                        2
                    } else if total < 1000 {
                        3
                    } else if total < 10000 {
                        4
                    } else {
                        5
                    };
                    let mut lines_col = column![].spacing(1.0);
                    for (i, line) in self.lines.iter().enumerate() {
                        let num_str = format!("{:>width$} ", i + 1, width = num_width);
                        let line_row = row![
                            text(num_str)
                                .font(Font::MONOSPACE)
                                .size(13.0)
                                .color(rgb(theme.tab_inactive_text)),
                            text(line.clone())
                                .font(Font::MONOSPACE)
                                .size(13.0)
                                .color(rgb(theme.fg)),
                        ];
                        lines_col = lines_col.push(line_row);
                    }
                    scrollable(container(lines_col).padding(8.0))
                        .width(Fill)
                        .height(Fill)
                        .into()
                }
                PreviewKind::Dir => {
                    let mut dir_col = column![].spacing(3.0);
                    for entry in &self.dir_entries {
                        let (icon, color, name_str) = if entry.is_dir {
                            ("📁 ", rgb(theme.frame_active), format!("{}/", entry.name))
                        } else {
                            ("📄 ", rgb(theme.fg), entry.name.clone())
                        };
                        let mut entry_row = row![
                            text(icon).font(Font::MONOSPACE).size(13.0),
                            text(name_str).font(Font::MONOSPACE).size(13.0).color(color),
                        ]
                        .spacing(4.0)
                        .align_y(alignment::Vertical::Center);

                        if let Some(sz) = entry.size {
                            entry_row = entry_row.push(
                                text(format!(" ({})", format_size(sz)))
                                    .font(Font::MONOSPACE)
                                    .size(11.0)
                                    .color(rgb(theme.tab_inactive_text)),
                            );
                        }
                        dir_col = dir_col.push(entry_row);
                    }
                    scrollable(container(dir_col).padding(8.0))
                        .width(Fill)
                        .height(Fill)
                        .into()
                }
                PreviewKind::Binary => {
                    let mut bin_col = column![].spacing(2.0);
                    for row_item in &self.hex_rows {
                        let offset_str = format!("{:08x}: ", row_item.offset);
                        let hex_row_elem = row![
                            text(offset_str)
                                .font(Font::MONOSPACE)
                                .size(12.0)
                                .color(rgb(theme.tab_inactive_text)),
                            text(row_item.hex.clone())
                                .font(Font::MONOSPACE)
                                .size(12.0)
                                .color(rgb(theme.fg)),
                            text(format!("  |{}|", row_item.ascii))
                                .font(Font::MONOSPACE)
                                .size(12.0)
                                .color(rgb(theme.search_match)),
                        ];
                        bin_col = bin_col.push(hex_row_elem);
                    }
                    scrollable(container(bin_col).padding(8.0))
                        .width(Fill)
                        .height(Fill)
                        .into()
                }
                PreviewKind::Image => {
                    let (w, h) = self.img_size.unwrap_or((0, 0));
                    let canvas_view = Canvas::new(ImageCanvas {
                        img_size: (w, h),
                        zoom: self.zoom,
                        pan_x: self.pan_x,
                        pan_y: self.pan_y,
                        theme,
                    })
                    .width(Fill)
                    .height(Fill);

                    let details_bar = container(
                        row![
                            text(format!("Dimensions: {w} × {h} px"))
                                .font(Font::MONOSPACE)
                                .size(11.0)
                                .color(rgb(theme.tab_inactive_text)),
                            text(format!("Zoom: {:.0}%", self.zoom * 100.0))
                                .font(Font::MONOSPACE)
                                .size(11.0)
                                .color(rgb(theme.tab_inactive_text)),
                            text(format!("Pan: ({:.1}, {:.1})", self.pan_x, self.pan_y))
                                .font(Font::MONOSPACE)
                                .size(11.0)
                                .color(rgb(theme.tab_inactive_text)),
                        ]
                        .spacing(16.0),
                    )
                    .padding(Padding::from([4.0, 10.0]))
                    .width(Fill)
                    .style(move |_theme| container::Style {
                        background: Some(rgb(theme.tab_bg).into()),
                        ..container::Style::default()
                    });

                    column![canvas_view, details_bar]
                        .width(Fill)
                        .height(Fill)
                        .into()
                }
            }
        };

        container(column![header, content_element].width(Fill).height(Fill))
            .width(Fill)
            .height(Fill)
            .style(move |_theme| container::Style {
                background: Some(rgb(theme.bg).into()),
                ..container::Style::default()
            })
            .into()
    }

    /// Paints the preview pane directly onto an iced canvas [`Frame`].
    pub fn paint(
        &self,
        frame: &mut Frame,
        bounds: Rectangle,
        font: Font,
        font_px: f32,
        theme: &GhostTheme,
    ) {
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), rgb(theme.bg));

        let kind_str = match self.kind {
            PreviewKind::Text => "text".to_string(),
            PreviewKind::Image => format!("image (zoom {:.0}%)", self.zoom * 100.0),
            PreviewKind::Dir => "directory".to_string(),
            PreviewKind::Binary => "binary".to_string(),
        };

        let header_text = format!("{} — {}", self.path.display(), kind_str);
        frame.fill_text(CanvasText {
            content: header_text,
            position: Point::new(8.0, 6.0),
            color: rgb(theme.tab_inactive_text),
            size: Pixels(font_px),
            font,
            align_x: alignment::Horizontal::Left.into(),
            align_y: alignment::Vertical::Top,
            shaping: iced::widget::text::Shaping::Advanced,
            ..CanvasText::default()
        });

        let header_h = font_px + 16.0;

        if let Some(err) = &self.error {
            frame.fill_text(CanvasText {
                content: err.clone(),
                position: Point::new(8.0, header_h + 8.0),
                color: rgb(theme.search_match),
                size: Pixels(font_px),
                font,
                align_x: alignment::Horizontal::Left.into(),
                align_y: alignment::Vertical::Top,
                shaping: iced::widget::text::Shaping::Advanced,
                ..CanvasText::default()
            });
            return;
        }

        match self.kind {
            PreviewKind::Image => {
                if let Some((w, h)) = self.img_size {
                    let avail_w = (bounds.width - 16.0).max(1.0);
                    let avail_h = (bounds.height - header_h - 8.0).max(1.0);
                    let (x, y, dw, dh) = transform_image_rect(
                        w,
                        h,
                        avail_w,
                        avail_h,
                        self.zoom,
                        self.pan_x,
                        self.pan_y,
                    );
                    let img_rect =
                        Rectangle::new(Point::new(x + 8.0, y + header_h), Size::new(dw, dh));

                    frame.fill_rectangle(
                        img_rect.position(),
                        img_rect.size(),
                        Color::from_rgba(0.2, 0.2, 0.3, 0.2),
                    );
                    frame.stroke_rectangle(
                        img_rect.position(),
                        img_rect.size(),
                        Stroke::default()
                            .with_color(rgb(theme.frame_active))
                            .with_width(1.5),
                    );

                    let info = format!("{w} × {h} px");
                    frame.fill_text(CanvasText {
                        content: info,
                        position: Point::new(img_rect.x + 8.0, img_rect.y + 8.0),
                        color: rgb(theme.tab_inactive_text),
                        size: Pixels(12.0),
                        font,
                        align_x: alignment::Horizontal::Left.into(),
                        align_y: alignment::Vertical::Top,
                        shaping: iced::widget::text::Shaping::Advanced,
                        ..CanvasText::default()
                    });
                }
            }
            PreviewKind::Text | PreviewKind::Dir | PreviewKind::Binary => {
                let line_h = (font_px * 1.25).max(12.0);
                let avail_h = (bounds.height - header_h).max(1.0);
                let visible = (avail_h / line_h) as usize;
                let range = paginate_lines(self.scroll, visible, self.lines.len());
                let mut y = header_h;

                for (idx, line) in self.lines[range.clone()].iter().enumerate() {
                    let line_num = range.start + idx + 1;
                    let display_str = if self.kind == PreviewKind::Text {
                        format!("{line_num:>4} {line}")
                    } else {
                        line.clone()
                    };

                    frame.fill_text(CanvasText {
                        content: display_str,
                        position: Point::new(8.0, y),
                        color: rgb(theme.fg),
                        size: Pixels(font_px),
                        font,
                        align_x: alignment::Horizontal::Left.into(),
                        align_y: alignment::Vertical::Top,
                        shaping: iced::widget::text::Shaping::Advanced,
                        ..CanvasText::default()
                    });
                    y += line_h;
                }
            }
        }
    }
}

/// Canvas program displaying image dimensions, zoom factor, and pan boundaries.
#[derive(Clone, Copy, Debug)]
pub struct ImageCanvas {
    /// Image width and height in pixels.
    pub img_size: (u32, u32),
    /// Zoom magnification factor.
    pub zoom: f32,
    /// Horizontal pan offset.
    pub pan_x: f32,
    /// Vertical pan offset.
    pub pan_y: f32,
    /// UI theme palette.
    pub theme: GhostTheme,
}

impl<Message> canvas::Program<Message> for ImageCanvas {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), rgb(self.theme.bg));

        let grid_sz = 16.0;
        let cols = (bounds.width / grid_sz).ceil() as usize;
        let rows = (bounds.height / grid_sz).ceil() as usize;
        let check_color = Color::from_rgba(1.0, 1.0, 1.0, 0.03);
        for r in 0..rows {
            for c in 0..cols {
                if (r + c) % 2 == 1 {
                    frame.fill_rectangle(
                        Point::new(c as f32 * grid_sz, r as f32 * grid_sz),
                        Size::new(grid_sz, grid_sz),
                        check_color,
                    );
                }
            }
        }

        let (img_w, img_h) = self.img_size;
        let (x, y, dw, dh) = transform_image_rect(
            img_w,
            img_h,
            bounds.width,
            bounds.height,
            self.zoom,
            self.pan_x,
            self.pan_y,
        );
        let img_rect = Rectangle::new(Point::new(x, y), Size::new(dw, dh));

        frame.fill_rectangle(
            img_rect.position(),
            img_rect.size(),
            Color::from_rgba(0.2, 0.2, 0.3, 0.2),
        );
        frame.stroke_rectangle(
            img_rect.position(),
            img_rect.size(),
            Stroke::default()
                .with_color(rgb(self.theme.frame_active))
                .with_width(1.5),
        );

        if dw > 20.0 && dh > 20.0 {
            let center_x = x + dw / 2.0;
            let center_y = y + dh / 2.0;
            let ch_len = 8.0;
            let ch_stroke = Stroke::default()
                .with_color(Color::from_rgba(1.0, 1.0, 1.0, 0.25))
                .with_width(1.0);
            frame.stroke(
                &CanvasPath::line(
                    Point::new(center_x - ch_len, center_y),
                    Point::new(center_x + ch_len, center_y),
                ),
                ch_stroke,
            );
            frame.stroke(
                &CanvasPath::line(
                    Point::new(center_x, center_y - ch_len),
                    Point::new(center_x, center_y + ch_len),
                ),
                ch_stroke,
            );
        }

        vec![frame.into_geometry()]
    }
}

/// Canvas program wrapping a [`PreviewPane`] reference.
pub struct PreviewCanvas<'a> {
    /// Borrowed preview pane state.
    pub pane: &'a PreviewPane,
    /// UI theme palette.
    pub theme: GhostTheme,
    /// Font size in pixels.
    pub font_px: f32,
}

impl<'a, Message> canvas::Program<Message> for PreviewCanvas<'a> {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        self.pane
            .paint(&mut frame, bounds, Font::MONOSPACE, self.font_px, &self.theme);
        vec![frame.into_geometry()]
    }
}

/// Detects the preview kind based on file path and optional sample bytes.
pub fn detect_kind(path: &Path, sample_bytes: Option<&[u8]>) -> PreviewKind {
    if path.is_dir() {
        return PreviewKind::Dir;
    }
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if is_image_extension(ext) {
            return PreviewKind::Image;
        }
    }
    if let Some(bytes) = sample_bytes {
        if detect_is_binary(bytes) {
            return PreviewKind::Binary;
        }
    }
    PreviewKind::Text
}

/// Returns whether the given file extension corresponds to a recognized image format.
pub fn is_image_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "svg" | "tiff" | "tif" | "avif"
    )
}

/// Heuristic check for binary content: inspects up to 4096 bytes for NUL characters.
pub fn detect_is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(4096).any(|&b| b == 0)
}

/// Sorts directory entries with directories first, followed by regular files, in case-insensitive alphabetical order.
pub fn sort_dir_entries(entries: &mut [DirEntryItem]) {
    entries.sort_by(|a, b| match (a.is_dir, b.is_dir) {
        (true, false) => Ordering::Less,
        (false, true) => Ordering::Greater,
        _ => {
            let a_lower = a.name.to_lowercase();
            let b_lower = b.name.to_lowercase();
            a_lower.cmp(&b_lower).then_with(|| a.name.cmp(&b.name))
        }
    });
}

/// Computes the visible line range given the scroll offset, visible capacity, and total lines.
pub fn paginate_lines(scroll: usize, visible_count: usize, total_lines: usize) -> Range<usize> {
    if total_lines == 0 || visible_count == 0 {
        return 0..0;
    }
    let max_scroll = total_lines.saturating_sub(visible_count);
    let start = scroll.min(max_scroll);
    let end = (start + visible_count).min(total_lines);
    start..end
}

/// Hard-wraps a single text line into chunks with at most `max_chars` characters.
pub fn wrap_line(line: &str, max_chars: usize) -> Vec<String> {
    if max_chars == 0 || line.len() <= max_chars {
        return vec![line.to_string()];
    }
    let mut chunks = Vec::new();
    let mut remainder = line;
    while remainder.len() > max_chars {
        let (head, tail) = remainder.split_at(max_chars);
        chunks.push(head.to_string());
        remainder = tail;
    }
    if !remainder.is_empty() {
        chunks.push(remainder.to_string());
    }
    chunks
}

/// Formats a byte buffer into standard 16-byte hex dump rows.
pub fn format_hex_dump(bytes: &[u8]) -> Vec<HexDumpRow> {
    bytes
        .chunks(16)
        .enumerate()
        .map(|(i, chunk)| {
            let offset = i * 16;
            let mut hex_parts = Vec::with_capacity(16);
            let mut ascii = String::with_capacity(16);
            for &b in chunk {
                hex_parts.push(format!("{b:02x}"));
                if (0x20..=0x7E).contains(&b) {
                    ascii.push(b as char);
                } else {
                    ascii.push('.');
                }
            }
            let hex_str = if chunk.len() < 16 {
                let mut s = hex_parts.join(" ");
                let missing = 16 - chunk.len();
                s.push_str(&"   ".repeat(missing));
                s
            } else {
                let (left, right) = hex_parts.split_at(8);
                format!("{}  {}", left.join(" "), right.join(" "))
            };
            HexDumpRow {
                offset,
                hex: hex_str,
                ascii,
            }
        })
        .collect()
}

/// Formats a raw byte count into a human-readable string with binary units (B, KiB, MiB, GiB).
pub fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Calculates destination rectangle `(x, y, width, height)` for an image fitted, scaled, and panned within bounds.
pub fn transform_image_rect(
    img_w: u32,
    img_h: u32,
    avail_w: f32,
    avail_h: f32,
    zoom: f32,
    pan_x: f32,
    pan_y: f32,
) -> (f32, f32, f32, f32) {
    if img_w == 0 || img_h == 0 || avail_w <= 0.0 || avail_h <= 0.0 {
        return (pan_x, pan_y, 0.0, 0.0);
    }
    let base_scale = (avail_w / img_w as f32).min(avail_h / img_h as f32);
    let scale = base_scale * zoom;
    let dw = img_w as f32 * scale;
    let dh = img_h as f32 * scale;
    let x = (avail_w - dw) / 2.0 + pan_x;
    let y = (avail_h - dh) / 2.0 + pan_y;
    (x, y, dw, dh)
}

/// Converts a ghostwave [`ghostwave::theme::Rgb`] color into an opaque iced [`Color`].
pub fn rgb(c: ghostwave::theme::Rgb) -> Color {
    Color::from_rgb8(c.0, c.1, c.2)
}

/// Parses image dimensions `(width, height)` from raw header bytes (PNG, GIF, BMP, WebP, JPEG).
pub fn parse_image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() >= 24 && bytes.starts_with(b"\x89PNG\r\n\x1a\n") && &bytes[12..16] == b"IHDR" {
        let w = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let h = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return Some((w, h));
    }

    if (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) && bytes.len() >= 10 {
        let w = u16::from_le_bytes(bytes[6..8].try_into().ok()?) as u32;
        let h = u16::from_le_bytes(bytes[8..10].try_into().ok()?) as u32;
        return Some((w, h));
    }

    if bytes.starts_with(b"BM") && bytes.len() >= 26 {
        let w = i32::from_le_bytes(bytes[18..22].try_into().ok()?).unsigned_abs();
        let h = i32::from_le_bytes(bytes[22..26].try_into().ok()?).unsigned_abs();
        if w > 0 && h > 0 {
            return Some((w, h));
        }
    }

    if bytes.len() >= 30 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        let chunk_type = &bytes[12..16];
        if chunk_type == b"VP8 " && bytes.len() >= 30 && bytes[23..26] == [0x9d, 0x01, 0x2a] {
            let w = (u16::from_le_bytes(bytes[26..28].try_into().ok()?) & 0x3fff) as u32;
            let h = (u16::from_le_bytes(bytes[28..30].try_into().ok()?) & 0x3fff) as u32;
            return Some((w, h));
        }
        if chunk_type == b"VP8L" && bytes.len() >= 25 && bytes[20] == 0x2f {
            let b0 = bytes[21] as u32;
            let b1 = bytes[22] as u32;
            let b2 = bytes[23] as u32;
            let b3 = bytes[24] as u32;
            let w = 1 + (((b1 & 0x3F) << 8) | b0);
            let h = 1 + (((b3 & 0x0F) << 10) | (b2 << 2) | (b1 >> 6));
            return Some((w, h));
        }
        if chunk_type == b"VP8X" && bytes.len() >= 30 {
            let w = 1 + (bytes[24] as u32 | ((bytes[25] as u32) << 8) | ((bytes[26] as u32) << 16));
            let h = 1 + (bytes[27] as u32 | ((bytes[28] as u32) << 8) | ((bytes[29] as u32) << 16));
            return Some((w, h));
        }
    }

    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        let mut offset = 2;
        while offset + 4 <= bytes.len() {
            if bytes[offset] != 0xFF {
                offset += 1;
                continue;
            }
            let marker = bytes[offset + 1];
            offset += 2;
            if marker == 0xFF || marker == 0x00 || (0xD0..=0xD7).contains(&marker) {
                continue;
            }
            if marker == 0xD9 || marker == 0xDA {
                break;
            }
            if offset + 2 > bytes.len() {
                break;
            }
            let length = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
            if length < 2 || offset + length > bytes.len() {
                break;
            }
            if matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF) {
                if length >= 7 && offset + 7 <= bytes.len() {
                    let height = u16::from_be_bytes([bytes[offset + 3], bytes[offset + 4]]) as u32;
                    let width = u16::from_be_bytes([bytes[offset + 5], bytes[offset + 6]]) as u32;
                    return Some((width, height));
                }
                break;
            }
            offset += length;
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_kind_by_path() {
        assert_eq!(detect_kind(Path::new("image.png"), None), PreviewKind::Image);
        assert_eq!(detect_kind(Path::new("photo.jpg"), None), PreviewKind::Image);
        assert_eq!(detect_kind(Path::new("doc.txt"), None), PreviewKind::Text);
        assert_eq!(detect_kind(Path::new("code.rs"), None), PreviewKind::Text);
        assert_eq!(detect_kind(Path::new("config.toml"), None), PreviewKind::Text);
    }

    #[test]
    fn test_detect_binary_content() {
        let text_bytes = b"Hello, world!\nLine 2";
        assert!(!detect_is_binary(text_bytes));
        assert_eq!(
            detect_kind(Path::new("unknown.dat"), Some(text_bytes)),
            PreviewKind::Text
        );

        let bin_bytes = b"ELF\x00\x01\x02";
        assert!(detect_is_binary(bin_bytes));
        assert_eq!(
            detect_kind(Path::new("unknown.dat"), Some(bin_bytes)),
            PreviewKind::Binary
        );
    }

    #[test]
    fn test_directory_sorting() {
        let mut entries = vec![
            DirEntryItem {
                name: "zeta.txt".into(),
                is_dir: false,
                size: Some(100),
            },
            DirEntryItem {
                name: "src".into(),
                is_dir: true,
                size: None,
            },
            DirEntryItem {
                name: "alpha.rs".into(),
                is_dir: false,
                size: Some(200),
            },
            DirEntryItem {
                name: "Assets".into(),
                is_dir: true,
                size: None,
            },
        ];

        sort_dir_entries(&mut entries);

        assert!(entries[0].is_dir);
        assert_eq!(entries[0].name, "Assets");
        assert!(entries[1].is_dir);
        assert_eq!(entries[1].name, "src");

        assert!(!entries[2].is_dir);
        assert_eq!(entries[2].name, "alpha.rs");
        assert!(!entries[3].is_dir);
        assert_eq!(entries[3].name, "zeta.txt");
    }

    #[test]
    fn test_paginate_lines() {
        assert_eq!(paginate_lines(0, 5, 20), 0..5);
        assert_eq!(paginate_lines(10, 5, 20), 10..15);
        assert_eq!(paginate_lines(18, 5, 20), 15..20);
        assert_eq!(paginate_lines(0, 10, 4), 0..4);
        assert_eq!(paginate_lines(5, 0, 20), 0..0);
        assert_eq!(paginate_lines(5, 5, 0), 0..0);
    }

    #[test]
    fn test_wrap_line() {
        let line = "abcdefghij";
        assert_eq!(wrap_line(line, 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap_line(line, 20), vec!["abcdefghij"]);
        assert_eq!(wrap_line("", 5), vec![""]);
    }

    #[test]
    fn test_zoom_and_pan_transformations() {
        let mut p = PreviewPane::empty("test.png", PreviewKind::Image);
        assert_eq!(p.zoom, 1.0);

        p.zoom_in();
        assert!((p.zoom - 1.25).abs() < 1e-4);
        p.zoom_out();
        assert!((p.zoom - 1.0).abs() < 1e-4);

        p.pan(40.0, -20.0);
        assert_eq!(p.pan_x, 40.0);
        assert_eq!(p.pan_y, -20.0);

        p.scroll_down(8);
        assert_eq!(p.scroll, 8);
        p.scroll_up(3);
        assert_eq!(p.scroll, 5);
        p.scroll_up(20);
        assert_eq!(p.scroll, 0);

        p.reset_view();
        assert_eq!(p.zoom, 1.0);
        assert_eq!(p.pan_x, 0.0);
        assert_eq!(p.pan_y, 0.0);
        assert_eq!(p.scroll, 0);
    }

    #[test]
    fn test_transform_image_rect() {
        let (x, y, dw, dh) = transform_image_rect(800, 600, 400.0, 300.0, 1.0, 0.0, 0.0);
        assert_eq!(dw, 400.0);
        assert_eq!(dh, 300.0);
        assert_eq!(x, 0.0);
        assert_eq!(y, 0.0);

        let (x, y, dw, dh) = transform_image_rect(800, 600, 400.0, 300.0, 2.0, 10.0, -5.0);
        assert_eq!(dw, 800.0);
        assert_eq!(dh, 600.0);
        assert_eq!(x, -190.0);
        assert_eq!(y, -155.0);
    }

    #[test]
    fn test_format_hex_dump() {
        let bytes = b"ABCDEF\x00\x01\x02\x7E";
        let rows = format_hex_dump(bytes);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].offset, 0);
        assert!(rows[0].hex.starts_with("41 42 43 44 45 46 00 01"));
        assert_eq!(rows[0].ascii, "ABCDEF...~");
    }

    #[test]
    fn test_parse_png_dimensions() {
        let mut png = Vec::new();
        png.extend_from_slice(b"\x89PNG\r\n\x1a\n");
        png.extend_from_slice(&13u32.to_be_bytes());
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1920u32.to_be_bytes());
        png.extend_from_slice(&1080u32.to_be_bytes());

        assert_eq!(parse_image_dimensions(&png), Some((1920, 1080)));
    }

    #[test]
    fn test_parse_gif_dimensions() {
        let mut gif = Vec::new();
        gif.extend_from_slice(b"GIF89a");
        gif.extend_from_slice(&640u16.to_le_bytes());
        gif.extend_from_slice(&480u16.to_le_bytes());

        assert_eq!(parse_image_dimensions(&gif), Some((640, 480)));
    }

    #[test]
    fn test_parse_bmp_dimensions() {
        let mut bmp = vec![0u8; 26];
        bmp[0] = b'B';
        bmp[1] = b'M';
        bmp[18..22].copy_from_slice(&800i32.to_le_bytes());
        bmp[22..26].copy_from_slice(&600i32.to_le_bytes());

        assert_eq!(parse_image_dimensions(&bmp), Some((800, 600)));
    }

    #[test]
    fn test_format_size() {
        assert_eq!(format_size(500), "500 B");
        assert_eq!(format_size(2048), "2.0 KiB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MiB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GiB");
    }
}
