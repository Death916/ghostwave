//! Ghostwave synthwave visualizer pane for iced 0.14.
//!
//! Renders an interactive 3D perspective rolling synthwave grid,
//! undulating neon spectral terrain peaks, and cyber matrix particle rain
//! as an iced [`canvas::Program`].

use ghostwave::theme::Theme as GhostTheme;
use iced::keyboard::key::Named;
use iced::keyboard::{self, Key};
use iced::widget::canvas::{self, Action, Event, Frame, Geometry, Text as CanvasText};
use iced::widget::text;
use iced::{
    alignment, Border, Color, Element, Font, Length, Padding, Pixels, Point, Rectangle,
    Renderer, Size, Theme, mouse,
};

use crate::ui_fonts;

/// Cyber matrix glyph pool.
pub const GLYPHS: [char; 19] = [
    'λ', '⚡', '◈', '∿', '⏣', '░', '▒', '▓', '0', '1', 'G', 'H', 'O', 'S', 'T', 'W', 'A', 'V', 'E',
];

/// Minimum speed multiplier.
pub const MIN_SPEED: f32 = 0.25;

/// Maximum speed multiplier.
pub const MAX_SPEED: f32 = 4.0;

/// Speed multiplier adjustment step for +/- keys.
pub const SPEED_STEP: f32 = 0.25;

/// Horizon line relative position (48% of pane height).
pub const HORIZON_RATIO: f32 = 0.48;

/// Default background color: RGB(10, 10, 15).
pub const DEFAULT_BG_COLOR: Color = Color::from_rgb8(10, 10, 15);

/// Number of longitudinal perspective lines on the rolling grid.
pub const NUM_X_LINES: usize = 16;

/// Number of transverse depth lines on the rolling grid.
pub const NUM_Z_LINES: usize = 14;

/// Number of sampled points along the mountain ridge.
pub const MOUNTAIN_POINTS: usize = 60;

/// Number of cyber rain particles.
pub const NUM_PARTICLES: usize = 64;

/// Visualizer presentation style.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum VizType {
    /// The classic 3D synthwave horizon with rolling grid, mountain wireframe, and falling particle rain.
    #[default]
    SynthwaveClassic,
    /// The retro iced-widget matrix terminal structure.
    MatrixTerminal,
}

impl VizType {
    pub fn next(&self) -> Self {
        match self {
            Self::SynthwaveClassic => Self::MatrixTerminal,
            Self::MatrixTerminal => Self::SynthwaveClassic,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::SynthwaveClassic => "SYNTHWAVE 3D",
            Self::MatrixTerminal => "MATRIX TERMINAL",
        }
    }
}

/// Color palettes for the synthwave visualizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ColorMode {
    #[default]
    GhostNeon,
    MatrixGreen,
    SolarAmber,
    AbyssIce,
}

impl ColorMode {
    /// Human-readable name and color description.
    pub fn name(&self) -> &'static str {
        match self {
            Self::GhostNeon => "GHOST NEON (CYAN / PINK / VIOLET)",
            Self::MatrixGreen => "MATRIX PHOSPHOR (LIME / EMERALD)",
            Self::SolarAmber => "SOLAR FLARE (AMBER / SUNSET)",
            Self::AbyssIce => "ABYSS ICE (CYAN / SAPPHIRE)",
        }
    }

    /// Cycles to the next color mode.
    pub fn next(&self) -> Self {
        match self {
            Self::GhostNeon => Self::MatrixGreen,
            Self::MatrixGreen => Self::SolarAmber,
            Self::SolarAmber => Self::AbyssIce,
            Self::AbyssIce => Self::GhostNeon,
        }
    }

    /// Returns the palette tuple `(primary_neon, secondary_neon, horizon_glow, particle_glow)`
    /// converted to iced [`Color`].
    pub fn colors(&self) -> (Color, Color, Color, Color) {
        match self {
            Self::GhostNeon => (
                Color::from_rgba(0.0, 0.95, 1.0, 0.85),
                Color::from_rgba(1.0, 0.15, 0.75, 0.80),
                Color::from_rgba(0.45, 0.10, 0.60, 0.40),
                Color::from_rgba(0.2, 1.0, 0.9, 0.9),
            ),
            Self::MatrixGreen => (
                Color::from_rgba(0.2, 1.0, 0.35, 0.85),
                Color::from_rgba(0.05, 0.65, 0.25, 0.75),
                Color::from_rgba(0.02, 0.25, 0.08, 0.40),
                Color::from_rgba(0.7, 1.0, 0.7, 0.95),
            ),
            Self::SolarAmber => (
                Color::from_rgba(1.0, 0.70, 0.10, 0.85),
                Color::from_rgba(1.0, 0.25, 0.15, 0.75),
                Color::from_rgba(0.50, 0.10, 0.05, 0.40),
                Color::from_rgba(1.0, 0.95, 0.40, 0.9),
            ),
            Self::AbyssIce => (
                Color::from_rgba(0.1, 0.8, 1.0, 0.85),
                Color::from_rgba(0.2, 0.35, 0.95, 0.75),
                Color::from_rgba(0.05, 0.1, 0.35, 0.40),
                Color::from_rgba(0.7, 0.9, 1.0, 0.9),
            ),
        }
    }
}

/// A cyber matrix falling particle glyph.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Particle {
    /// Relative horizontal position in `[0.0, 1.0]`.
    pub x: f32,
    /// Relative vertical position in `[0.0, 1.0]`.
    pub y: f32,
    /// Fall speed multiplier.
    pub speed: f32,
    /// Display glyph character.
    pub glyph: char,
    /// Brightness factor in `[0.0, 1.0]`.
    pub brightness: f32,
    /// Font size in pixels.
    pub size: f32,
}

/// Ghostwave synthwave visualizer pane state and renderer.
#[derive(Clone, Debug)]
pub struct VisualizerPane {
    /// Simulation time in seconds.
    pub time: f32,
    /// Current color mode palette.
    pub mode: ColorMode,
    /// Visualizer presentation style (Synthwave 3D vs Matrix Terminal).
    pub viz_type: VizType,
    /// Speed multiplier for animation.
    pub speed_mult: f32,
    /// Active cyber rain particles.
    pub particles: Vec<Particle>,
    /// Font face used for HUD and glyph rendering.
    pub font: Font,
    /// Optional background fill override.
    pub bg_color: Option<Color>,
}

impl Default for VisualizerPane {
    fn default() -> Self {
        Self::new()
    }
}

impl VisualizerPane {
    /// Creates a new visualizer pane with 64 pseudo-randomized cyber rain particles.
    pub fn new() -> Self {
        let mut particles = Vec::with_capacity(NUM_PARTICLES);
        for i in 0..NUM_PARTICLES {
            let seed = (i as f32 * 17.31 + 5.7).fract();
            let seed2 = (i as f32 * 31.83 + 2.1).fract();
            let glyph = GLYPHS[i % GLYPHS.len()];
            particles.push(Particle {
                x: seed,
                y: seed2,
                speed: 0.15 + (i as f32 * 7.13).fract() * 0.35,
                glyph,
                brightness: 0.4 + (i as f32 * 13.7).fract() * 0.6,
                size: 10.0 + (i as f32 * 23.9).fract() * 8.0,
            });
        }

        Self {
            time: 0.0,
            mode: ColorMode::GhostNeon,
            viz_type: VizType::default(),
            speed_mult: 1.0,
            particles,
            font: ui_fonts::primary(),
            bg_color: None,
        }
    }

    /// Overrides the font face used for HUD and particle rendering.
    pub fn with_font(mut self, font: Font) -> Self {
        self.font = font;
        self
    }

    /// Overrides the background color.
    pub fn with_bg_color(mut self, color: Color) -> Self {
        self.bg_color = Some(color);
        self
    }

    /// Sets the color mode palette.
    pub fn with_mode(mut self, mode: ColorMode) -> Self {
        self.mode = mode;
        self
    }

    /// Sets the visualizer presentation style.
    pub fn with_viz_type(mut self, viz_type: VizType) -> Self {
        self.viz_type = viz_type;
        self
    }

    /// Sets the speed multiplier clamped within `[MIN_SPEED, MAX_SPEED]`.
    pub fn with_speed(mut self, speed: f32) -> Self {
        self.set_speed(speed);
        self
    }

    /// Cycles to the next color mode.
    pub fn cycle_mode(&mut self) {
        self.mode = self.mode.next();
    }

    /// Cycles between visualizer presentation styles (Synthwave 3D vs Matrix Terminal).
    pub fn cycle_viz_type(&mut self) {
        self.viz_type = self.viz_type.next();
    }

    /// Increases animation speed by [`SPEED_STEP`], capped at [`MAX_SPEED`].
    pub fn speed_up(&mut self) {
        self.speed_mult = (self.speed_mult + SPEED_STEP).min(MAX_SPEED);
    }

    /// Decreases animation speed by [`SPEED_STEP`], floored at [`MIN_SPEED`].
    pub fn speed_down(&mut self) {
        self.speed_mult = (self.speed_mult - SPEED_STEP).max(MIN_SPEED);
    }

    /// Sets the speed multiplier clamped to `[MIN_SPEED, MAX_SPEED]`.
    pub fn set_speed(&mut self, speed: f32) {
        self.speed_mult = speed.clamp(MIN_SPEED, MAX_SPEED);
    }

    /// Advances simulation time and updates falling particle positions.
    pub fn tick(&mut self, dt: f32) {
        let effective_dt = dt * self.speed_mult;
        self.time += effective_dt;

        for p in &mut self.particles {
            p.y += p.speed * effective_dt * 0.4;
            if p.y > 1.05 {
                p.y = -0.05;
                p.x = ((p.x * 123.45 + self.time * 0.1).fract()).clamp(0.02, 0.98);
            }
        }
    }

    /// Alias for [`tick`](Self::tick) for backwards compatibility.
    pub fn update(&mut self, dt: f32) {
        self.tick(dt);
    }

    /// Handles keyboard input directly on the pane.
    ///
    /// Spacebar cycles color mode; `+`/`=` increases speed; `-`/`_` decreases speed.
    /// Returns `true` if the key was handled.
    pub fn handle_key(&mut self, key: &Key) -> bool {
        match key {
            Key::Named(Named::Space) => {
                self.cycle_mode();
                true
            }
            Key::Character(c) if c.as_ref() == "+" || c.as_ref() == "=" => {
                self.speed_up();
                true
            }
            Key::Character(c) if c.as_ref() == "-" || c.as_ref() == "_" => {
                self.speed_down();
                true
            }
            _ => false,
        }
    }

    /// Builds the visualizer pane according to its active presentation style.
    pub fn view<'a, Message: 'a>(&'a self, _theme: &GhostTheme) -> Element<'a, Message> {
        match self.viz_type {
            VizType::SynthwaveClassic => self.view_classic(),
            VizType::MatrixTerminal => self.view_terminal(),
        }
    }

    /// Renders the classic 3D synthwave visualizer: header banner + smooth 3D horizon canvas.
    pub fn view_classic<'a, Message: 'a>(&'a self) -> Element<'a, Message> {
        let (c_prim, _c_sec, _c_glow, _c_part) = self.mode.colors();

        let hud_title = format!("◈ GHOSTWAVE // {} // {} ◈", self.viz_type.name(), self.mode.name());
        let hud_hints = format!("Speed: {:.2}x  [v/t: Style | Space/c: Palette | +/-: Speed]", self.speed_mult);

        let header = iced::widget::container(
            iced::widget::row![
                iced::widget::text(hud_title)
                    .size(11.0)
                    .font(self.font)
                    .color(c_prim),
                iced::widget::text(hud_hints)
                    .size(11.0)
                    .font(self.font)
                    .color(Color::from_rgba(0.7, 0.7, 0.8, 0.8)),
            ]
            .spacing(12.0)
            .align_y(alignment::Vertical::Center),
        )
        .width(Length::Fill)
        .padding(Padding::from([4.0, 10.0]))
        .style(move |_| iced::widget::container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.70).into()),
            border: Border {
                color: Color::from_rgba(c_prim.r, c_prim.g, c_prim.b, 0.40),
                width: 1.0,
                radius: 2.0.into(),
            },
            ..Default::default()
        });

        let canvas_widget = iced::widget::Canvas::new(self)
            .width(Length::Fill)
            .height(Length::Fill);

        let content = iced::widget::column![
            header,
            canvas_widget,
        ]
        .spacing(2.0)
        .width(Length::Fill)
        .height(Length::Fill);

        iced::widget::container(content)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::new(2.0))
            .style(move |_| iced::widget::container::Style {
                background: Some(Color::from_rgb8(10, 10, 15).into()),
                ..Default::default()
            })
            .into()
    }

    /// Renders the matrix terminal visualizer constructed entirely from native iced layout primitives.
    pub fn view_terminal<'a, Message: 'a>(&'a self) -> Element<'a, Message> {
        let (c_prim, c_sec, _c_glow, c_part) = self.mode.colors();

        let hud_title = format!("◈ GHOSTWAVE // {} // {} ◈", self.viz_type.name(), self.mode.name());
        let hud_hints = format!("Speed: {:.2}x  [v/t: Style | Space/c: Palette | +/-: Speed]", self.speed_mult);

        let header = iced::widget::container(
            iced::widget::row![
                iced::widget::text(hud_title)
                    .size(11.0)
                    .font(self.font)
                    .color(c_prim),
                iced::widget::text(hud_hints)
                    .size(11.0)
                    .font(self.font)
                    .color(Color::from_rgba(0.7, 0.7, 0.8, 0.8)),
            ]
            .spacing(12.0)
            .align_y(alignment::Vertical::Center),
        )
        .width(Length::Fill)
        .padding(Padding::from([4.0, 10.0]))
        .style(move |_| iced::widget::container::Style {
            background: Some(Color::from_rgba(0.0, 0.0, 0.0, 0.70).into()),
            border: Border {
                color: Color::from_rgba(c_prim.r, c_prim.g, c_prim.b, 0.40),
                width: 1.0,
                radius: 2.0.into(),
            },
            ..Default::default()
        });

        // Upper Region: Cyber Matrix Rain Sky (FillPortion 5)
        let num_cols = 18;
        let num_rows = 9;
        let mut rain_columns: Vec<Element<'a, Message>> = Vec::with_capacity(num_cols);

        for col_idx in 0..num_cols {
            let speed = 0.6 + ((col_idx as f32 * 5.13).fract()) * 0.8;
            let head = (self.time * speed * self.speed_mult * 0.25 + (col_idx as f32 * 17.37).fract()) % 1.0;
            let mut col_items: Vec<Element<'a, Message>> = Vec::with_capacity(num_rows);
            for row_idx in 0..num_rows {
                let r_norm = row_idx as f32 / num_rows as f32;
                let diff = (r_norm - head).abs();
                let brightness = (1.0 - diff * 2.2).clamp(0.08, 0.95);
                let char_idx = (col_idx * 7 + row_idx * 3 + (self.time * 2.0) as usize) % GLYPHS.len();
                let ch = GLYPHS[char_idx];
                let color = Color::from_rgba(c_part.r, c_part.g, c_part.b, brightness);
                col_items.push(
                    iced::widget::text(ch.to_string())
                        .size(12.0)
                        .font(self.font)
                        .color(color)
                        .into(),
                );
            }
            rain_columns.push(
                iced::widget::column(col_items)
                    .spacing(1.0)
                    .align_x(alignment::Horizontal::Center)
                    .into(),
            );
        }

        let sky_area = iced::widget::container(
            iced::widget::row(rain_columns)
                .spacing(16.0)
                .align_y(alignment::Vertical::Center),
        )
        .width(Length::Fill)
        .height(Length::FillPortion(5))
        .center_x(Length::Fill)
        .center_y(Length::Fill);

        // Mountain Ridge & Neon Horizon Divider
        let mountain_line = "      ▲         ▲       ━ ━     ◆     ▲   ━   ◆   ━   ▲       ▲   ━   ◆      ";
        let mountains = iced::widget::container(
            iced::widget::text(mountain_line)
                .size(12.0)
                .font(self.font)
                .color(c_sec),
        )
        .width(Length::Fill)
        .center_x(Length::Fill);

        let horizon_glow = iced::widget::container(iced::widget::text(""))
            .width(Length::Fill)
            .height(Length::Fixed(2.0))
            .style(move |_| iced::widget::container::Style {
                background: Some(c_prim.into()),
                border: Border {
                    color: Color::from_rgba(c_prim.r, c_prim.g, c_prim.b, 0.60),
                    width: 1.0,
                    radius: 1.0.into(),
                },
                ..Default::default()
            });

        // Lower Region: 3D Rolling Perspective Synthwave Grid (FillPortion 5)
        let num_bands = 7;
        let scroll_phase = grid_scroll_phase(self.time, self.speed_mult);
        let mut grid_bands: Vec<Element<'a, Message>> = Vec::with_capacity(num_bands);

        for b in 0..num_bands {
            let z_raw = (b as f32 + scroll_phase) / num_bands as f32;
            let z = project_z(z_raw);
            let alpha = (z * 0.9).clamp(0.15, 0.95);
            let line_col = Color::from_rgba(c_prim.r, c_prim.g, c_prim.b, alpha);

            let grid_chars = match b % 3 {
                0 => "╱   ╱   ╱   ╱   │   │   ╲   ╲   ╲   ╲",
                1 => "  ╱   ╱   ╱   │   │   │   ╲   ╲   ╲  ",
                _ => "    ╱   ╱   │   │   │   │   ╲   ╲    ",
            };

            let font_size = 10.0 + z * 5.0;
            let grid_text = iced::widget::container(
                iced::widget::text(grid_chars)
                    .size(font_size)
                    .font(self.font)
                    .color(line_col),
            )
            .width(Length::Fill)
            .center_x(Length::Fill);

            let crossbar_h = (1.0 + z * 2.5).round();
            let crossbar = iced::widget::container(iced::widget::text(""))
                .width(Length::Fill)
                .height(Length::Fixed(crossbar_h))
                .style(move |_| iced::widget::container::Style {
                    background: Some(line_col.into()),
                    ..Default::default()
                });

            grid_bands.push(
                iced::widget::column![grid_text, crossbar]
                    .spacing(1.0)
                    .width(Length::Fill)
                    .into(),
            );
        }

        let grid_area = iced::widget::container(
            iced::widget::column(grid_bands)
                .spacing(4.0)
                .width(Length::Fill)
                .align_x(alignment::Horizontal::Center),
        )
        .width(Length::Fill)
        .height(Length::FillPortion(5))
        .center_x(Length::Fill)
        .center_y(Length::Fill);

        let main_col = iced::widget::column![
            header,
            sky_area,
            mountains,
            horizon_glow,
            grid_area,
        ]
        .spacing(2.0)
        .width(Length::Fill)
        .height(Length::Fill);

        iced::widget::container(main_col)
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::new(4.0))
            .style(move |_| iced::widget::container::Style {
                background: Some(Color::from_rgb8(10, 10, 15).into()),
                ..Default::default()
            })
            .into()
    }
}

/// Computes the horizon line Y coordinate given total pane height.
#[inline]
pub fn horizon_y(height: f32) -> f32 {
    height * HORIZON_RATIO
}

/// Computes the grid scrolling phase in `[0.0, 1.0)`.
#[inline]
pub fn grid_scroll_phase(time: f32, speed_mult: f32) -> f32 {
    let p = (time * 0.6 * speed_mult).fract();
    if p < 0.0 { p + 1.0 } else { p }
}

/// Projects a normalized linear depth `z_raw` into quadratic perspective depth.
///
/// Returns a value in `[0.0, 1.0]` when `z_raw` is in `[0.0, 1.0]`.
#[inline]
pub fn project_z(z_raw: f32) -> f32 {
    z_raw * z_raw
}

/// Computes the Y coordinate of a transverse grid line.
#[inline]
pub fn project_grid_line_y(horizon_y: f32, grid_h: f32, z_raw: f32) -> f32 {
    horizon_y + project_z(z_raw) * grid_h
}

/// Computes the bottom X coordinate for a longitudinal perspective line.
///
/// `prog` is in `[-1.0, 1.0]`, where -1.0 is leftmost and 1.0 is rightmost.
#[inline]
pub fn longitudinal_bottom_x(center_x: f32, width: f32, prog: f32) -> f32 {
    center_x + prog * (width * 0.65)
}

/// Computes the mountain peak height offset at horizontal progress `prog` in `[0.0, 1.0]`
/// and elapsed time `time`.
#[inline]
pub fn mountain_height(prog: f32, time: f32) -> f32 {
    let m_env = (prog * std::f32::consts::PI).sin();
    let m_wave1 = (prog * 12.0 + time * 0.4).sin() * 25.0;
    let m_wave2 = (prog * 28.0 - time * 0.7).sin() * 12.0;
    let m_wave3 = (prog * 45.0 + time * 1.1).sin() * 6.0;
    (m_wave1 + m_wave2 + m_wave3).abs() * m_env
}

/// Canvas interaction state tracking runtime overrides.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct VisualizerState {
    /// Color mode override set via interactive canvas events.
    pub mode: Option<ColorMode>,
    /// Speed multiplier override set via interactive canvas events.
    pub speed_mult: Option<f32>,
}

impl VisualizerState {
    /// Clears any interactive overrides.
    pub fn reset(&mut self) {
        self.mode = None;
        self.speed_mult = None;
    }
}



impl<Message> canvas::Program<Message> for VisualizerPane {
    type State = VisualizerState;

    fn update(
        &self,
        state: &mut Self::State,
        event: &Event,
        _bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Option<Action<Message>> {
        match event {
            Event::Keyboard(keyboard::Event::KeyPressed { key, .. }) => match key {
                Key::Named(Named::Space) => {
                    let current = state.mode.unwrap_or(self.mode);
                    state.mode = Some(current.next());
                    Some(Action::capture())
                }
                Key::Character(c) if c.as_ref() == "+" || c.as_ref() == "=" => {
                    let current = state.speed_mult.unwrap_or(self.speed_mult);
                    state.speed_mult = Some((current + SPEED_STEP).min(MAX_SPEED));
                    Some(Action::capture())
                }
                Key::Character(c) if c.as_ref() == "-" || c.as_ref() == "_" => {
                    let current = state.speed_mult.unwrap_or(self.speed_mult);
                    state.speed_mult = Some((current - SPEED_STEP).max(MIN_SPEED));
                    Some(Action::capture())
                }
                _ => None,
            },
            _ => None,
        }
    }

    fn draw(
        &self,
        state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let w = bounds.width;
        let h = bounds.height;
        if std::env::var_os("GHOST_ICED_DEBUG").is_some() {
            eprintln!(
                "[ghostwave] visualizer draw: bounds {}x{} at ({:.1},{:.1})",
                w, h, bounds.x, bounds.y
            );
        }
        if w <= 0.0 || h <= 0.0 {
            return vec![frame.into_geometry()];
        }

        let mode = state.mode.unwrap_or(self.mode);
        let speed_mult = state.speed_mult.unwrap_or(self.speed_mult);
        let (c_prim, c_sec, c_glow, c_part) = mode.colors();
        let horizon = horizon_y(h);

        let bg = self.bg_color.unwrap_or(DEFAULT_BG_COLOR);
        frame.fill_rectangle(Point::ORIGIN, bounds.size(), bg);

        let dome_h = h * 0.45;
        let dome_top = (horizon - dome_h).max(0.0);
        frame.fill_rectangle(
            Point::new(0.0, dome_top),
            Size::new(w, dome_h),
            Color::from_rgba(c_glow.r, c_glow.g, c_glow.b, 0.20),
        );

        // 1. Cyber Matrix Particle Rain (Sky Region)
        for p in &self.particles {
            let px = p.x * w;
            let py = p.y * horizon;
            if py >= 0.0 && py <= horizon {
                let alpha = (p.brightness * (1.0 - py / horizon.max(1.0))).clamp(0.12, 0.95);
                let col = Color::from_rgba(c_part.r, c_part.g, c_part.b, alpha);
                frame.fill_text(CanvasText {
                    content: p.glyph.to_string(),
                    position: Point::new(px, py),
                    color: col,
                    size: Pixels(p.size),
                    font: self.font,
                    align_x: alignment::Horizontal::Center.into(),
                    align_y: alignment::Vertical::Center,
                    shaping: text::Shaping::Advanced,
                    ..CanvasText::default()
                });
            }
        }

        // 2. Spectral Mountain Wireframe Range (at Horizon)
        for i in 0..=MOUNTAIN_POINTS {
            let prog = i as f32 / MOUNTAIN_POINTS as f32;
            let mx = prog * w;
            let m_h = mountain_height(prog, self.time);
            let my = horizon - m_h;

            // Mountain ridge peak glyphs
            let peak_glyph = if i % 2 == 0 { "▲" } else { "━" };
            frame.fill_text(CanvasText {
                content: peak_glyph.to_string(),
                position: Point::new(mx, my),
                color: c_sec,
                size: Pixels(11.0),
                font: self.font,
                align_x: alignment::Horizontal::Center.into(),
                align_y: alignment::Vertical::Center,
                shaping: text::Shaping::Advanced,
                ..CanvasText::default()
            });

            // Vertical ribbing down to horizon
            if i % 3 == 0 && m_h > 6.0 {
                let ribs = (m_h / 12.0).ceil() as usize;
                for r in 1..=ribs {
                    let ry = my + (r as f32 * 12.0).min(m_h - 2.0);
                    let rib_alpha = (0.25 * (1.0 - (ry - my) / m_h)).clamp(0.08, 0.35);
                    frame.fill_text(CanvasText {
                        content: "┆".to_string(),
                        position: Point::new(mx, ry),
                        color: Color::from_rgba(c_sec.r, c_sec.g, c_sec.b, rib_alpha),
                        size: Pixels(10.0),
                        font: self.font,
                        align_x: alignment::Horizontal::Center.into(),
                        align_y: alignment::Vertical::Center,
                        shaping: text::Shaping::Advanced,
                        ..CanvasText::default()
                    });
                }
            }
        }

        // 3. Horizon Divider Neon Line
        let char_count = (w / 8.0).ceil() as usize + 4;
        let horizon_str = "━".repeat(char_count);
        frame.fill_text(CanvasText {
            content: horizon_str,
            position: Point::new(0.0, horizon),
            color: c_prim,
            size: Pixels(14.0),
            font: self.font,
            align_x: alignment::Horizontal::Left.into(),
            align_y: alignment::Vertical::Center,
            shaping: text::Shaping::Advanced,
            ..CanvasText::default()
        });

        // 4. 3D Perspective Rolling Synthwave Grid (Bottom Half)
        let grid_h = h - horizon;
        let speed = grid_scroll_phase(self.time, speed_mult);

        // Transverse horizontal crossbars scrolling forward
        for i in 0..NUM_Z_LINES {
            let z_raw = (i as f32 + speed) / NUM_Z_LINES as f32;
            let z = project_z(z_raw);
            let line_y = project_grid_line_y(horizon, grid_h, z_raw);
            let alpha = (z * 0.9).clamp(0.18, 0.95);
            let col = Color::from_rgba(c_prim.r, c_prim.g, c_prim.b, alpha);

            let crossbar_glyph = if z > 0.6 { "━" } else if z > 0.25 { "─" } else { "┄" };
            let crossbar_str = crossbar_glyph.repeat(char_count);

            frame.fill_text(CanvasText {
                content: crossbar_str,
                position: Point::new(0.0, line_y),
                color: col,
                size: Pixels(10.0 + z * 6.0),
                font: self.font,
                align_x: alignment::Horizontal::Left.into(),
                align_y: alignment::Vertical::Center,
                shaping: text::Shaping::Advanced,
                ..CanvasText::default()
            });

            // Longitudinal intersection points radiating outwards at each crossbar
            let center_x = w * 0.5;
            for j in 0..=NUM_X_LINES {
                let prog = (j as f32 / NUM_X_LINES as f32) * 2.0 - 1.0;
                let px = center_x + prog * (w * 0.65) * (0.05 + 0.95 * z);
                let node_glyph = if prog.abs() < 0.1 { "┃" } else if prog < 0.0 { "╱" } else { "╲" };
                frame.fill_text(CanvasText {
                    content: node_glyph.to_string(),
                    position: Point::new(px, line_y),
                    color: Color::from_rgba(c_prim.r, c_prim.g, c_prim.b, (alpha * 0.8).clamp(0.15, 0.9)),
                    size: Pixels(10.0 + z * 6.0),
                    font: self.font,
                    align_x: alignment::Horizontal::Center.into(),
                    align_y: alignment::Vertical::Center,
                    shaping: text::Shaping::Advanced,
                    ..CanvasText::default()
                });
            }
        }

        vec![frame.into_geometry()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_color_mode_rotation() {
        let mut mode = ColorMode::GhostNeon;
        assert_eq!(mode, ColorMode::GhostNeon);
        assert!(!mode.name().is_empty());

        mode = mode.next();
        assert_eq!(mode, ColorMode::MatrixGreen);
        assert!(!mode.name().is_empty());

        mode = mode.next();
        assert_eq!(mode, ColorMode::SolarAmber);
        assert!(!mode.name().is_empty());

        mode = mode.next();
        assert_eq!(mode, ColorMode::AbyssIce);
        assert!(!mode.name().is_empty());

        mode = mode.next();
        assert_eq!(mode, ColorMode::GhostNeon);

        let mut pane = VisualizerPane::new();
        assert_eq!(pane.mode, ColorMode::GhostNeon);
        pane.cycle_mode();
        assert_eq!(pane.mode, ColorMode::MatrixGreen);
        pane.cycle_mode();
        assert_eq!(pane.mode, ColorMode::SolarAmber);
        pane.cycle_mode();
        assert_eq!(pane.mode, ColorMode::AbyssIce);
        pane.cycle_mode();
        assert_eq!(pane.mode, ColorMode::GhostNeon);

        for m in [
            ColorMode::GhostNeon,
            ColorMode::MatrixGreen,
            ColorMode::SolarAmber,
            ColorMode::AbyssIce,
        ] {
            let (c1, c2, c3, c4) = m.colors();
            for c in [c1, c2, c3, c4] {
                assert!(c.r >= 0.0 && c.r <= 1.0);
                assert!(c.g >= 0.0 && c.g <= 1.0);
                assert!(c.b >= 0.0 && c.b <= 1.0);
                assert!(c.a >= 0.0 && c.a <= 1.0);
            }
        }
    }

    #[test]
    fn test_particle_ticking() {
        let mut pane = VisualizerPane::new();
        assert_eq!(pane.particles.len(), NUM_PARTICLES);
        assert_eq!(pane.time, 0.0);

        let initial_y = pane.particles[0].y;
        let initial_speed = pane.particles[0].speed;
        let dt = 0.5;

        pane.tick(dt);
        assert_eq!(pane.time, dt * pane.speed_mult);

        let expected_y = initial_y + initial_speed * dt * pane.speed_mult * 0.4;
        assert!((pane.particles[0].y - expected_y).abs() < 1e-5);

        pane.particles[0].y = 1.06;
        pane.tick(0.01);
        assert_eq!(pane.particles[0].y, -0.05);
        assert!(pane.particles[0].x >= 0.02 && pane.particles[0].x <= 0.98);

        let prev_time = pane.time;
        pane.update(0.2);
        assert!((pane.time - (prev_time + 0.2 * pane.speed_mult)).abs() < 1e-5);
    }

    #[test]
    fn test_speed_bounds() {
        let mut pane = VisualizerPane::new();
        assert_eq!(pane.speed_mult, 1.0);

        pane.speed_up();
        assert_eq!(pane.speed_mult, 1.25);

        for _ in 0..20 {
            pane.speed_up();
        }
        assert_eq!(pane.speed_mult, MAX_SPEED);

        pane.speed_up();
        assert_eq!(pane.speed_mult, MAX_SPEED);

        for _ in 0..30 {
            pane.speed_down();
        }
        assert_eq!(pane.speed_mult, MIN_SPEED);

        pane.speed_down();
        assert_eq!(pane.speed_mult, MIN_SPEED);

        pane.set_speed(100.0);
        assert_eq!(pane.speed_mult, MAX_SPEED);
        pane.set_speed(-5.0);
        assert_eq!(pane.speed_mult, MIN_SPEED);
        pane.set_speed(2.0);
        assert_eq!(pane.speed_mult, 2.0);

        pane.handle_key(&Key::Character("+".into()));
        assert_eq!(pane.speed_mult, 2.25);
        pane.handle_key(&Key::Character("=".into()));
        assert_eq!(pane.speed_mult, 2.5);
        pane.handle_key(&Key::Character("-".into()));
        assert_eq!(pane.speed_mult, 2.25);
        pane.handle_key(&Key::Character("_".into()));
        assert_eq!(pane.speed_mult, 2.0);

        let handled = pane.handle_key(&Key::Character("x".into()));
        assert!(!handled);
        assert_eq!(pane.speed_mult, 2.0);
    }

    #[test]
    fn test_projection_math() {
        let h = 800.0;
        let horiz = horizon_y(h);
        assert_eq!(horiz, 800.0 * 0.48);

        assert_eq!(grid_scroll_phase(0.0, 1.0), 0.0);
        let phase = grid_scroll_phase(1.5, 1.0);
        assert!(phase >= 0.0 && phase < 1.0);

        assert_eq!(project_z(0.0), 0.0);
        assert_eq!(project_z(1.0), 1.0);
        assert_eq!(project_z(0.5), 0.25);
        assert!(project_z(0.2) < 0.2);

        let grid_h = 400.0;
        let y0 = project_grid_line_y(horiz, grid_h, 0.0);
        let y1 = project_grid_line_y(horiz, grid_h, 1.0);
        let y_mid = project_grid_line_y(horiz, grid_h, 0.5);
        assert_eq!(y0, horiz);
        assert_eq!(y1, horiz + grid_h);
        assert_eq!(y_mid, horiz + 0.25 * grid_h);

        let center_x = 500.0;
        let width = 1000.0;
        let left_x = longitudinal_bottom_x(center_x, width, -1.0);
        let right_x = longitudinal_bottom_x(center_x, width, 1.0);
        let mid_x = longitudinal_bottom_x(center_x, width, 0.0);
        assert_eq!(mid_x, center_x);
        assert_eq!((left_x + right_x) / 2.0, center_x);
        assert_eq!(right_x - center_x, 650.0);

        let m_start = mountain_height(0.0, 10.0);
        let m_end = mountain_height(1.0, 10.0);
        assert!(m_start.abs() < 1e-5);
        assert!(m_end.abs() < 1e-5);
        let m_peak = mountain_height(0.5, 0.0);
        assert!(m_peak >= 0.0);
    }
}
