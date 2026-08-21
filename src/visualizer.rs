//! Ghostwave screensaver & visualizer pane.
//! Renders an interactive 3D perspective rolling synthwave grid,
//! undulating neon spectral terrain mountains, and cyber matrix particle rain.

use macroquad::prelude::*;
use crate::layout::Rect;
use crate::theme::Theme;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ColorMode {
    GhostNeon,
    MatrixGreen,
    SolarAmber,
    AbyssIce,
}

impl ColorMode {
    pub fn name(&self) -> &'static str {
        match self {
            Self::GhostNeon => "GHOST NEON (CYAN / PINK / VIOLET)",
            Self::MatrixGreen => "MATRIX PHOSPHOR (LIME / EMERALD)",
            Self::SolarAmber => "SOLAR FLARE (AMBER / SUNSET)",
            Self::AbyssIce => "ABYSS ICE (CYAN / SAPPHIRE)",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::GhostNeon => Self::MatrixGreen,
            Self::MatrixGreen => Self::SolarAmber,
            Self::SolarAmber => Self::AbyssIce,
            Self::AbyssIce => Self::GhostNeon,
        }
    }

    pub fn colors(&self) -> (Color, Color, Color, Color) {
        // (primary_neon, secondary_neon, horizon_glow, particle_glow)
        match self {
            Self::GhostNeon => (
                Color::new(0.0, 0.95, 1.0, 0.85),   // Electric Cyan
                Color::new(1.0, 0.15, 0.75, 0.80),  // Hot Magenta
                Color::new(0.45, 0.10, 0.60, 0.40), // Deep Violet
                Color::new(0.2, 1.0, 0.9, 0.9),    // Cyan particle
            ),
            Self::MatrixGreen => (
                Color::new(0.2, 1.0, 0.35, 0.85),  // Bright Matrix Green
                Color::new(0.05, 0.65, 0.25, 0.75), // Dark Emerald
                Color::new(0.02, 0.25, 0.08, 0.40), // Forest Green
                Color::new(0.7, 1.0, 0.7, 0.95),   // Phosphor White-Green
            ),
            Self::SolarAmber => (
                Color::new(1.0, 0.70, 0.10, 0.85),  // Golden Amber
                Color::new(1.0, 0.25, 0.15, 0.75),  // Sunset Orange
                Color::new(0.50, 0.10, 0.05, 0.40), // Deep Crimson
                Color::new(1.0, 0.95, 0.40, 0.9),  // Radiant Yellow
            ),
            Self::AbyssIce => (
                Color::new(0.1, 0.8, 1.0, 0.85),   // Ice Blue
                Color::new(0.2, 0.35, 0.95, 0.75),  // Deep Sapphire
                Color::new(0.05, 0.1, 0.35, 0.40),  // Midnight Navy
                Color::new(0.7, 0.9, 1.0, 0.9),    // Glacial White
            ),
        }
    }
}

struct Particle {
    x: f32, // 0.0 to 1.0 (relative width)
    y: f32, // 0.0 to 1.0 (relative height)
    speed: f32,
    glyph: char,
    brightness: f32,
    size: f32,
}

pub struct VisualizerPane {
    pub time: f32,
    pub mode: ColorMode,
    pub speed_mult: f32,
    particles: Vec<Particle>,
}

impl Default for VisualizerPane {
    fn default() -> Self {
        Self::new()
    }
}

impl VisualizerPane {
    pub fn new() -> Self {
        let glyphs = ['λ', '⚡', '◈', '∿', '⏣', '░', '▒', '▓', '0', '1', 'G', 'H', 'O', 'S', 'T', 'W', 'A', 'V', 'E'];
        let mut particles = Vec::with_capacity(64);
        for i in 0..64 {
            let seed = (i as f32 * 17.31 + 5.7).fract();
            let seed2 = (i as f32 * 31.83 + 2.1).fract();
            let glyph = glyphs[i % glyphs.len()];
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
            speed_mult: 1.0,
            particles,
        }
    }

    pub fn cycle_mode(&mut self) {
        self.mode = self.mode.next();
    }

    pub fn update(&mut self, dt: f32) {
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

    pub fn render(&self, rect: Rect, font: &Font, _theme: &Theme) {
        let rx = rect.x;
        let ry = rect.y;
        let rw = rect.w;
        let rh = rect.h;

        // Dark atmospheric background with gradient
        draw_rectangle(rx, ry, rw, rh, Color::new(0.04, 0.04, 0.07, 1.0));

        let (c_prim, c_sec, c_glow, c_part) = self.mode.colors();

        let horizon_y = ry + rh * 0.48;

        // 1. Horizon Atmospheric Glow Dome
        let dome_h = rh * 0.45;
        draw_rectangle(
            rx,
            horizon_y - dome_h,
            rw,
            dome_h,
            Color::new(c_glow.r, c_glow.g, c_glow.b, 0.18),
        );

        // 2. Cyber Matrix Particle Rain (Sky Region)
        for p in &self.particles {
            let px = rx + p.x * rw;
            let py = ry + p.y * (horizon_y - ry);
            if py >= ry && py <= horizon_y {
                let alpha = (p.brightness * (1.0 - (py - ry) / (horizon_y - ry).max(1.0))).clamp(0.1, 0.95);
                let col = Color::new(c_part.r, c_part.g, c_part.b, alpha);
                let glyph_str = p.glyph.to_string();
                draw_text_ex(
                    &glyph_str,
                    px,
                    py,
                    TextParams {
                        font: Some(font),
                        font_size: p.size as u16,
                        color: col,
                        ..Default::default()
                    },
                );
            }
        }

        // 3. Spectral Mountain Wireframe Range (at Horizon)
        let mountain_points = 60;
        let mut prev_mx = rx;
        let mut prev_my = horizon_y;
        for i in 0..=mountain_points {
            let prog = i as f32 / mountain_points as f32;
            let mx = rx + prog * rw;
            let m_env = (prog * std::f32::consts::PI).sin();
            let m_wave1 = (prog * 12.0 + self.time * 0.4).sin() * 25.0;
            let m_wave2 = (prog * 28.0 - self.time * 0.7).sin() * 12.0;
            let m_wave3 = (prog * 45.0 + self.time * 1.1).sin() * 6.0;
            let my = horizon_y - (m_wave1 + m_wave2 + m_wave3).abs() * m_env;

            if i > 0 {
                // Ridge line
                draw_line(prev_mx, prev_my, mx, my, 1.8, c_sec);
                // Subtle vertical ribbing down to horizon
                if i % 3 == 0 {
                    draw_line(
                        mx, my, mx, horizon_y,
                        1.0,
                        Color::new(c_sec.r, c_sec.g, c_sec.b, 0.15),
                    );
                }
            }
            prev_mx = mx;
            prev_my = my;
        }

        // 4. Horizon Divider Neon Line
        draw_line(rx, horizon_y, rx + rw, horizon_y, 2.0, c_prim);

        // 5. 3D Perspective Rolling Synthwave Grid (Bottom Half)
        let grid_h = (ry + rh) - horizon_y;
        let num_z_lines = 14;
        let speed = (self.time * 0.6 * self.speed_mult).fract();

        // Horizontal crossbars scrolling forward
        for i in 0..num_z_lines {
            let z_raw = (i as f32 + speed) / num_z_lines as f32;
            let z = z_raw * z_raw; // Quadratic projection gives 3D depth acceleration
            let line_y = horizon_y + z * grid_h;
            let alpha = (z * 0.9).clamp(0.05, 0.85);
            let col = Color::new(c_prim.r, c_prim.g, c_prim.b, alpha);
            draw_line(rx, line_y, rx + rw, line_y, 1.0 + z * 1.5, col);
        }

        // Longitudinal Perspective Lines converging to center horizon
        let num_x_lines = 16;
        let center_x = rx + rw * 0.5;
        for i in 0..=num_x_lines {
            let prog = (i as f32 / num_x_lines as f32) * 2.0 - 1.0; // -1.0 to 1.0
            let bottom_x = center_x + prog * (rw * 0.65);
            draw_line(
                center_x,
                horizon_y,
                bottom_x,
                ry + rh,
                1.2,
                Color::new(c_prim.r, c_prim.g, c_prim.b, 0.50),
            );
        }

        // 6. HUD Header Banner
        let hud_title = format!("◈ GHOSTWAVE VISUALIZER // {} ◈", self.mode.name());
        let hud_help = "[Space: Color Mode | +/-: Speed | Ctrl+w: Close]";
        draw_rectangle(rx + 8.0, ry + 8.0, rw - 16.0, 26.0, Color::new(0.0, 0.0, 0.0, 0.65));
        draw_rectangle_lines(rx + 8.0, ry + 8.0, rw - 16.0, 26.0, 1.0, Color::new(c_prim.r, c_prim.g, c_prim.b, 0.40));
        draw_text_ex(
            &hud_title,
            rx + 16.0,
            ry + 25.0,
            TextParams {
                font: Some(font),
                font_size: 11,
                color: c_prim,
                ..Default::default()
            },
        );
        let help_w = measure_text(hud_help, Some(font), 11, 1.0).width;
        if rw > help_w + 300.0 {
            draw_text_ex(
                hud_help,
                rx + rw - help_w - 20.0,
                ry + 25.0,
                TextParams {
                    font: Some(font),
                    font_size: 11,
                    color: Color::new(0.7, 0.7, 0.8, 0.8),
                    ..Default::default()
                },
            );
        }
    }
}
