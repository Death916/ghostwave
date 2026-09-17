//! Native iced 0.14 chrome widgets for ghostwave.
//!
//! Provides the top tab bar, bottom status bar (with the live reactive wave
//! visualizer canvas), and floating toast overlay as native iced widgets,
//! replacing hand-painted canvas chrome from `CHROME-REFERENCE.md`.

use ghostwave::theme::{STATUS_BAR_H, TAB_BAR_H};
use ghostwave::theme::{Rgb, Theme as GhostTheme};
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke};
use iced::widget::{container, mouse_area, row, text, text_input, Canvas};
use iced::{
    alignment, Border, Color, Element, Font, Length, Padding, Point, Rectangle, Renderer, Theme,
};

use crate::app::AppState;
use crate::ui_fonts;
use crate::Message;

pub use crate::chrome::WaveParams;

/// Tab label font size in px.
pub const TAB_LABEL_PX: f32 = 14.0;

/// Status bar and toast font size in px.
pub const STATUS_PX: f32 = 13.0;

/// Tab pitch in px.
pub const TAB_W: f32 = 130.0;

/// Inset on the right edge of a tab chip, leaving a separator gap.
pub const TAB_CHIP_INSET: f32 = 4.0;

/// Width in px of each tab chip (`TAB_W - TAB_CHIP_INSET`).
pub const TAB_CHIP_W: f32 = TAB_W - TAB_CHIP_INSET;

/// Tab label x offset from the chip's left edge, in px.
pub const TAB_LABEL_X: f32 = 10.0;

/// Gap between the mode pill and the keybinding hints text, in px.
pub const HINTS_DX: f32 = 10.0;

/// Clearance the wave visualizer maintains from adjacent status elements, in px.
pub const WAVE_MARGIN: f32 = 24.0;

/// Minimum width required for the wave visualizer to draw, in px.
pub const WAVE_MIN_W: f32 = 50.0;

/// Horizontal distance between wave sampling points, in px.
pub const WAVE_STEP: f32 = 3.0;

/// Toast box x offset from the window's left edge, in px.
pub const TOAST_X: f32 = 8.0;

/// Toast box height, in px.
pub const TOAST_H: f32 = 24.0;

/// Gap between the toast box's bottom edge and the status bar, in px.
pub const TOAST_BOTTOM_GAP: f32 = 6.0;

/// Converts a ghostwave theme RGB triple into an iced [`Color`].
pub fn rgb(c: Rgb) -> Color {
    Color::from_rgb8(c.0, c.1, c.2)
}

/// Returns the background color for an active tab chip (`theme.tab_active`).
pub fn tab_active_color(theme: &GhostTheme) -> Color {
    rgb(theme.tab_active)
}

/// Returns the background color for an inactive tab chip (`theme.tab_bg`).
pub fn tab_inactive_color(theme: &GhostTheme) -> Color {
    rgb(theme.tab_bg)
}

/// Returns the pill color for a given mode label.
///
/// Specific modes map to their accent colors (such as search or resize
/// highlighting), while standard modes use the active frame color
/// (`theme.frame_active`).
pub fn mode_color(mode: &str, theme: &GhostTheme) -> Color {
    match mode.trim() {
        "SEARCH" | "SEARCH>" => rgb(theme.search_match),
        "RESIZE" => rgb(theme.search_match),
        "LOCKED" => rgb(theme.palette_sel),
        _ => rgb(theme.frame_active),
    }
}

/// Native iced tab bar widget.
///
/// Displays a row of tab chips with active/inactive styling:
/// - Active chip: background `theme.tab_active`, text `theme.palette_fg`.
/// - Inactive chip: background `theme.tab_bg`, text `theme.tab_inactive_text`.
/// - Each chip dispatches [`Message::FocusTab(i)`] when clicked.
/// - When `rename_target` is active (`Some(draft)`), the active tab chip
///   displays an inline [`text_input`] showing the draft name instead of static text.
pub fn tab_bar<'a>(
    tabs: &[(String, bool)],
    rename_target: Option<&'a str>,
    theme: &GhostTheme,
) -> Element<'a, Message> {
    let theme = *theme;
    let mut chips: Vec<Element<'a, Message>> = Vec::with_capacity(tabs.len());

    for (i, (label, active)) in tabs.iter().enumerate() {
        let is_active = *active;
        if is_active && rename_target.is_some() {
            let draft = rename_target.unwrap_or_default();
            let input = text_input("Tab name", draft)
                .size(TAB_LABEL_PX)
                .font(ui_fonts::primary())
                .padding(Padding {
                    top: 2.0,
                    bottom: 2.0,
                    left: 6.0,
                    right: 6.0,
                })
                .style(move |_theme, _status| text_input::Style {
                    background: rgb(theme.palette_bg).into(),
                    border: Border {
                        color: rgb(theme.frame_active),
                        width: 1.0,
                        radius: 2.0.into(),
                    },
                    icon: Color::TRANSPARENT,
                    placeholder: rgb(theme.tab_inactive_text),
                    value: rgb(theme.palette_fg),
                    selection: rgb(theme.selection),
                });

            let chip_container = container(input)
                .width(Length::Fixed(TAB_CHIP_W))
                .height(Length::Fixed(TAB_BAR_H))
                .center_y(Length::Fill)
                .padding(Padding {
                    left: 4.0,
                    right: 4.0,
                    top: 2.0,
                    bottom: 2.0,
                })
                .style(move |_| container::Style {
                    background: Some(rgb(theme.tab_active).into()),
                    ..container::Style::default()
                });

            chips.push(chip_container.into());
        } else {
            let display_text = if label.is_empty() {
                format!("Tab {}", i + 1)
            } else {
                label.clone()
            };

            let text_color = if is_active {
                rgb(theme.palette_fg)
            } else {
                rgb(theme.tab_inactive_text)
            };

            let chip_bg = if is_active {
                rgb(theme.tab_active)
            } else {
                rgb(theme.tab_bg)
            };

            let chip_content = container(
                text(display_text)
                    .size(TAB_LABEL_PX)
                    .color(text_color)
                    .font(ui_fonts::primary()),
            )
            .width(Length::Fixed(TAB_CHIP_W))
            .height(Length::Fixed(TAB_BAR_H))
            .align_left(Length::Fill)
            .center_y(Length::Fill)
            .padding(Padding {
                left: TAB_LABEL_X,
                right: 4.0,
                top: 0.0,
                bottom: 0.0,
            })
            .style(move |_| container::Style {
                background: Some(chip_bg.into()),
                ..container::Style::default()
            });

            let clickable = mouse_area(chip_content).on_press(Message::FocusTab(i));
            chips.push(clickable.into());
        }
    }

    let chips_row = row(chips)
        .spacing(TAB_CHIP_INSET)
        .height(Length::Fixed(TAB_BAR_H))
        .align_y(alignment::Vertical::Center);

    container(chips_row)
        .width(Length::Fill)
        .height(Length::Fixed(TAB_BAR_H))
        .style(move |_| container::Style {
            background: Some(rgb(theme.tab_bg).into()),
            ..container::Style::default()
        })
        .into()
}

/// Convenience helper to build the [`tab_bar`] directly from [`AppState`].
pub fn tab_bar_for_state<'a>(state: &'a AppState) -> Element<'a, Message> {
    let tabs: Vec<(String, bool)> = state
        .tabs
        .iter()
        .enumerate()
        .map(|(i, t)| (t.label(i), i == state.active_tab))
        .collect();

    let rename_target = if state.mode == ghostwave::keymap::Mode::RenameTab {
        Some(state.rename_buf.as_str())
    } else {
        None
    };

    tab_bar(&tabs, rename_target, &state.theme)
}

/// Builder for constructing a tab bar widget with fluent options.
pub struct TabBar<'a> {
    tabs: &'a [(String, bool)],
    rename_target: Option<&'a str>,
    theme: &'a GhostTheme,
}

impl<'a> TabBar<'a> {
    /// Creates a new tab bar builder for the given tabs and theme.
    pub fn new(tabs: &'a [(String, bool)], theme: &'a GhostTheme) -> Self {
        Self {
            tabs,
            rename_target: None,
            theme,
        }
    }

    /// Sets the active rename draft string.
    pub fn rename_target(mut self, target: Option<&'a str>) -> Self {
        self.rename_target = target;
        self
    }

    /// Builds the iced [`Element`].
    pub fn build(self) -> Element<'a, Message> {
        tab_bar(self.tabs, self.rename_target, self.theme)
    }
}

impl<'a> From<TabBar<'a>> for Element<'a, Message> {
    fn from(tb: TabBar<'a>) -> Self {
        tb.build()
    }
}

/// Live reactive wave canvas program.
///
/// Draws the 2-harmonic sine wave ribbons (primary H1 and counter-phase H2 glow)
/// driven by the current `wave_phase`, `wave_energy`, and `amplitude`.
#[derive(Clone, Copy, Debug)]
pub struct WaveProgram {
    /// Wave oscillation and energy parameters.
    pub params: WaveParams,
    /// Color theme.
    pub theme: GhostTheme,
}

impl<Message> canvas::Program<Message> for WaveProgram {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: iced::mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        let wave_w = bounds.width;

        if wave_w <= WAVE_MIN_W {
            return vec![frame.into_geometry()];
        }

        let center_y = bounds.height / 2.0;
        let max_amp = (bounds.height * 0.35 * self.params.amplitude).clamp(2.0, 10.0);
        let num_points = (wave_w / WAVE_STEP).max(10.0) as usize;
        let dt = 1.0 / num_points as f32;

        let envelope = |progress: f32| (progress * std::f32::consts::PI).sin();
        let x_at = |progress: f32| progress * wave_w;

        let h1_alpha0 = 0.20 + 0.50 * (self.params.energy / 2.0).min(1.0);
        let mut prev = {
            let x = x_at(0.0);
            let y_off = ((x * 0.035 + self.params.phase).sin() * 0.7
                + (x * 0.035 * 1.8 - self.params.phase * 1.4).sin() * 0.3)
                * max_amp
                * self.params.energy
                * envelope(0.0);
            Point::new(x, center_y + y_off)
        };

        for i in 0..num_points {
            let progress = i as f32 * dt;
            let next_progress = progress + dt;
            let x = x_at(next_progress);
            let y_off = ((x * 0.035 + self.params.phase).sin() * 0.7
                + (x * 0.035 * 1.8 - self.params.phase * 1.4).sin() * 0.3)
                * max_amp
                * self.params.energy
                * envelope(next_progress);
            let next = Point::new(x, center_y + y_off);
            let alpha = h1_alpha0 * envelope(progress);
            stroke_segment(
                &mut frame,
                prev,
                next,
                1.5,
                Color {
                    a: alpha,
                    ..rgb(self.theme.frame_active)
                },
            );
            prev = next;
        }

        let h2_alpha0 = 0.12 + 0.30 * (self.params.energy / 2.0).min(1.0);
        let amp_h2 = max_amp * 0.65;
        let mut prev = {
            let x = x_at(0.0);
            let y_off = ((x * 0.022 * 1.3 - self.params.phase * 0.8).sin() * 0.8
                + (x * 0.022 * 2.2 + self.params.phase * 1.9).sin() * 0.2)
                * amp_h2
                * self.params.energy
                * envelope(0.0);
            Point::new(x, center_y + y_off)
        };

        for i in 0..num_points {
            let progress = i as f32 * dt;
            let next_progress = progress + dt;
            let x = x_at(next_progress);
            let y_off = ((x * 0.022 * 1.3 - self.params.phase * 0.8).sin() * 0.8
                + (x * 0.022 * 2.2 + self.params.phase * 1.9).sin() * 0.2)
                * amp_h2
                * self.params.energy
                * envelope(next_progress);
            let next = Point::new(x, center_y + y_off);
            let alpha = h2_alpha0 * envelope(progress);
            stroke_segment(
                &mut frame,
                prev,
                next,
                1.0,
                Color {
                    a: alpha,
                    ..rgb(self.theme.palette_sel)
                },
            );
            prev = next;
        }

        vec![frame.into_geometry()]
    }
}

/// Strokes a straight line segment from `from` to `to` on the canvas frame.
fn stroke_segment(frame: &mut Frame, from: Point, to: Point, width: f32, color: Color) {
    let path = Path::new(|p| {
        p.move_to(from);
        p.line_to(to);
    });
    frame.stroke(&path, Stroke::default().with_color(color).with_width(width));
}

/// Constructs a live reactive wave canvas widget.
pub fn wave_widget<'a>(
    wave: Option<WaveParams>,
    theme: &GhostTheme,
) -> Element<'a, Message> {
    if let Some(params) = wave {
        Canvas::new(WaveProgram {
            params,
            theme: *theme,
        })
        .width(Length::Fill)
        .height(Length::Fixed(STATUS_BAR_H))
        .into()
    } else {
        container(text(""))
            .width(Length::Fill)
            .height(Length::Fixed(STATUS_BAR_H))
            .into()
    }
}

/// Native iced status bar widget.
///
/// Features:
/// - Mode pill (`container` with `theme.mode_*` color and bold mode label).
/// - Keybinding hints text (`theme.status_fg`).
/// - Live reactive wave canvas widget (using [`Canvas`] to draw the 2-harmonic sine wave).
/// - Sync panes chip (`[SYNC]` when active, `theme.search_match` fill).
/// - Theme name badge (`theme.status_fg`).
pub fn status_bar<'a>(
    mode_label: &'a str,
    hints: &'a str,
    wave: Option<WaveParams>,
    sync_panes: bool,
    theme_name: &'a str,
    theme: &GhostTheme,
) -> Element<'a, Message> {
    let theme = *theme;
    let pill_color = mode_color(mode_label, &theme);
    let mode_pill = container(
        text(mode_label)
            .size(STATUS_PX)
            .color(rgb(theme.bg))
            .font(Font {
                weight: iced::font::Weight::Bold,
                ..ui_fonts::primary()
            }),
    )
    .padding(Padding {
        top: 3.0,
        bottom: 3.0,
        left: 8.0,
        right: 8.0,
    })
    .center_y(Length::Shrink)
    .style(move |_| container::Style {
        background: Some(pill_color.into()),
        border: Border {
            radius: 2.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    });

    let hints_text = text(hints)
        .size(STATUS_PX)
        .color(rgb(theme.status_fg))
        .font(ui_fonts::primary());

    let mut left_row = row![mode_pill]
        .spacing(HINTS_DX)
        .align_y(alignment::Vertical::Center);

    if !hints.is_empty() {
        left_row = left_row.push(hints_text);
    }

    let mut right_row = row![]
        .spacing(8.0)
        .align_y(alignment::Vertical::Center);

    if sync_panes {
        let sync_chip = container(
            text("[SYNC]")
                .size(STATUS_PX)
                .color(rgb(theme.bg))
                .font(Font {
                    weight: iced::font::Weight::Bold,
                    ..ui_fonts::primary()
                }),
        )
        .padding(Padding {
            top: 2.0,
            bottom: 2.0,
            left: 6.0,
            right: 6.0,
        })
        .style(move |_| container::Style {
            background: Some(rgb(theme.search_match).into()),
            border: Border {
                radius: 2.0.into(),
                ..Border::default()
            },
            ..container::Style::default()
        });

        right_row = right_row.push(sync_chip);
    }

    if !theme_name.is_empty() {
        let theme_badge = container(
            text(theme_name)
                .size(STATUS_PX)
                .color(rgb(theme.status_fg))
                .font(ui_fonts::primary()),
        )
        .padding(Padding {
            top: 2.0,
            bottom: 2.0,
            left: 6.0,
            right: 6.0,
        });

        right_row = right_row.push(theme_badge);
    }

    let wave_element = wave_widget(wave, &theme);

    let wave_container = container(wave_element)
        .width(Length::Fill)
        .padding(Padding {
            left: WAVE_MARGIN,
            right: WAVE_MARGIN,
            top: 0.0,
            bottom: 0.0,
        });

    let bar_row = row![left_row, wave_container, right_row]
        .width(Length::Fill)
        .height(Length::Fixed(STATUS_BAR_H))
        .align_y(alignment::Vertical::Center);

    container(bar_row)
        .width(Length::Fill)
        .height(Length::Fixed(STATUS_BAR_H))
        .padding(Padding {
            left: 0.0,
            right: 8.0,
            top: 0.0,
            bottom: 0.0,
        })
        .style(move |_| container::Style {
            background: Some(rgb(theme.status_bg).into()),
            ..container::Style::default()
        })
        .into()
}

/// Convenience helper to build the [`status_bar`] directly from [`AppState`].
pub fn status_bar_for_state<'a>(state: &'a AppState) -> Element<'a, Message> {
    let wave = if state.config.visual_wave {
        Some(WaveParams {
            energy: state.wave_energy,
            phase: state.wave_phase,
            amplitude: state.config.wave_amplitude,
        })
    } else {
        None
    };

    status_bar(
        state.mode.label(),
        state.mode.hints(),
        wave,
        state.current().sync_panes,
        &state.config.theme,
        &state.theme,
    )
}

/// Builder for constructing a status bar widget with fluent options.
pub struct StatusBar<'a> {
    mode_label: &'a str,
    hints: &'a str,
    wave: Option<WaveParams>,
    sync_panes: bool,
    theme_name: &'a str,
    theme: &'a GhostTheme,
}

impl<'a> StatusBar<'a> {
    /// Creates a new status bar builder with the given mode label and theme.
    pub fn new(mode_label: &'a str, theme: &'a GhostTheme) -> Self {
        Self {
            mode_label,
            hints: "",
            wave: None,
            sync_panes: false,
            theme_name: "",
            theme,
        }
    }

    /// Sets the keybinding hints text.
    pub fn hints(mut self, hints: &'a str) -> Self {
        self.hints = hints;
        self
    }

    /// Sets the optional wave visualizer parameters.
    pub fn wave(mut self, wave: Option<WaveParams>) -> Self {
        self.wave = wave;
        self
    }

    /// Sets whether synchronized input across panes is active.
    pub fn sync_panes(mut self, sync: bool) -> Self {
        self.sync_panes = sync;
        self
    }

    /// Sets the theme name to display on the badge.
    pub fn theme_name(mut self, name: &'a str) -> Self {
        self.theme_name = name;
        self
    }

    /// Builds the iced [`Element`].
    pub fn build(self) -> Element<'a, Message> {
        status_bar(
            self.mode_label,
            self.hints,
            self.wave,
            self.sync_panes,
            self.theme_name,
            self.theme,
        )
    }
}

impl<'a> From<StatusBar<'a>> for Element<'a, Message> {
    fn from(sb: StatusBar<'a>) -> Self {
        sb.build()
    }
}

/// Native iced toast pill widget.
///
/// Builds the compact inner pill showing the message text with padding,
/// rounded border, and theme background coloring (`theme.palette_sel`).
pub fn toast_box<'a>(msg: &'a str, theme: &GhostTheme) -> Element<'a, Message> {
    let theme = *theme;
    container(
        text(msg)
            .size(STATUS_PX)
            .color(rgb(theme.bg))
            .font(ui_fonts::primary()),
    )
    .padding(Padding {
        top: 4.0,
        bottom: 4.0,
        left: 12.0,
        right: 12.0,
    })
    .style(move |_| container::Style {
        background: Some(rgb(theme.palette_sel).into()),
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        ..container::Style::default()
    })
    .into()
}

/// Floating toast overlay container widget.
///
/// Returns `Some(Element)` when a toast message is present, or `None` if absent.
/// Positions the toast overlay near the bottom-left edge, `TOAST_X` (8 px) from
/// the window's left border and `TOAST_BOTTOM_GAP` (6 px) above the status bar.
pub fn toast<'a>(
    toast_msg: Option<&'a str>,
    theme: &GhostTheme,
) -> Option<Element<'a, Message>> {
    let theme = *theme;
    let msg = toast_msg?;
    if msg.is_empty() {
        return None;
    }

    let box_widget = toast_box(msg, &theme);

    let overlay = container(box_widget)
        .width(Length::Fill)
        .height(Length::Fill)
        .align_left(Length::Fill)
        .align_bottom(Length::Fill)
        .padding(Padding {
            left: TOAST_X,
            bottom: STATUS_BAR_H + TOAST_BOTTOM_GAP,
            right: 0.0,
            top: 0.0,
        });

    Some(overlay.into())
}

/// Convenience helper to build the [`toast`] overlay directly from [`AppState`].
pub fn toast_for_state<'a>(state: &'a AppState) -> Option<Element<'a, Message>> {
    let msg = state.toast.as_ref().map(|(m, _)| m.as_str());
    toast(msg, &state.theme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ghostwave::theme::THEMES;

    #[test]
    fn rgb_converts_correctly() {
        let c = rgb(Rgb(0x12, 0x34, 0x56));
        assert!((c.r - 0x12 as f32 / 255.0).abs() < 1e-4);
        assert!((c.g - 0x34 as f32 / 255.0).abs() < 1e-4);
        assert!((c.b - 0x56 as f32 / 255.0).abs() < 1e-4);
        assert_eq!(c.a, 1.0);
    }

    #[test]
    fn mode_colors_match_theme_roles() {
        let theme = THEMES[0];
        assert_eq!(mode_color("NORMAL", &theme), rgb(theme.frame_active));
        assert_eq!(mode_color("PANE", &theme), rgb(theme.frame_active));
        assert_eq!(mode_color("TAB", &theme), rgb(theme.frame_active));
        assert_eq!(mode_color("SEARCH", &theme), rgb(theme.search_match));
        assert_eq!(mode_color("SEARCH>", &theme), rgb(theme.search_match));
        assert_eq!(mode_color("RESIZE", &theme), rgb(theme.search_match));
        assert_eq!(mode_color("LOCKED", &theme), rgb(theme.palette_sel));
    }

    #[test]
    fn tab_colors_match_roles() {
        let theme = THEMES[0];
        assert_eq!(tab_active_color(&theme), rgb(theme.tab_active));
        assert_eq!(tab_inactive_color(&theme), rgb(theme.tab_bg));
    }

    #[test]
    fn constants_match_reference_spec() {
        assert_eq!(TAB_BAR_H, 30.0);
        assert_eq!(STATUS_BAR_H, 26.0);
        assert_eq!(TAB_W, 130.0);
        assert_eq!(TAB_CHIP_INSET, 4.0);
        assert_eq!(TAB_CHIP_W, 126.0);
        assert_eq!(TAB_LABEL_X, 10.0);
        assert_eq!(HINTS_DX, 10.0);
        assert_eq!(WAVE_MARGIN, 24.0);
        assert_eq!(WAVE_MIN_W, 50.0);
        assert_eq!(WAVE_STEP, 3.0);
        assert_eq!(TOAST_X, 8.0);
        assert_eq!(TOAST_H, 24.0);
        assert_eq!(TOAST_BOTTOM_GAP, 6.0);
    }

    #[test]
    fn tab_bar_constructs_normal_and_rename() {
        let theme = THEMES[0];
        let tabs = vec![
            ("Tab 1".to_string(), true),
            ("Tab 2".to_string(), false),
        ];

        let _el = tab_bar(&tabs, None, &theme);
        let _el_rename = tab_bar(&tabs, Some("new name"), &theme);

        let builder = TabBar::new(&tabs, &theme).rename_target(Some("draft"));
        let _built: Element<'_, Message> = builder.into();
    }

    #[test]
    fn status_bar_constructs_with_and_without_wave() {
        let theme = THEMES[0];
        let _el_plain = status_bar("NORMAL", "hints line", None, false, "tokyonight", &theme);

        let wave = WaveParams {
            energy: 1.2,
            phase: 0.5,
            amplitude: 1.0,
        };
        let _el_wave = status_bar("PANE", "hints line", Some(wave), true, "tokyonight", &theme);

        let builder = StatusBar::new("SEARCH", &theme)
            .hints("type to search")
            .sync_panes(true)
            .theme_name("tokyonight")
            .wave(Some(wave));
        let _built: Element<'_, Message> = builder.into();
    }

    #[test]
    fn toast_returns_none_when_empty_and_some_when_present() {
        let theme = THEMES[0];
        assert!(toast(None, &theme).is_none());
        assert!(toast(Some(""), &theme).is_none());
        assert!(toast(Some("synchronized input ON"), &theme).is_some());

        let _pill = toast_box("test message", &theme);
    }

    #[test]
    fn wave_widget_constructs_elements() {
        let theme = THEMES[0];
        let _none_wave = wave_widget(None, &theme);
        let wave = WaveParams {
            energy: 0.8,
            phase: 1.5,
            amplitude: 1.0,
        };
        let _live_wave = wave_widget(Some(wave), &theme);
    }
}
