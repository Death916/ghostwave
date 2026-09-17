//! ghostwave on iced: a tab of terminal panes laid out by iced's `pane_grid`,
//! painted on one clipped canvas, with pane frames, labels, click-to-focus and
//! wheel scrollback.

use ghostwave::config::Config;
use ghostwave::keymap::Action;
use iced::{
    keyboard,
    widget::{pane_grid, Stack},
    Element, Length, Size, Subscription, Task,
};
use libghostty_vt::key;

mod app;
pub mod ai;
mod chrome;
pub mod chrome_widgets;
mod config;
mod fonts;
#[allow(dead_code)]
mod grid_session;
mod input;
mod keys;
mod measure;
pub mod palette;
mod pane;
mod pane_canvas;
mod panes_grid;
pub mod preview;
mod pty_bridge;
pub mod scratchpad;
pub mod search_overlay;
pub mod selection;
mod tick;
pub mod visualizer;

use app::AppState;
use pane::PaneEntry;
use pane_canvas::ChromeData;

/// Cell metrics are measured from it and the canvas draws text with it.
const FONT_BYTES: &[u8] = include_bytes!("../../assets/JetBrainsMono-Medium.ttf");

/// Replaced by the first `resize_events` message.
const START_WINDOW: Size = Size::new(1200.0, 800.0);

/// Events the iced runtime delivers, plus the mouse events the canvas synthesizes.
#[derive(Clone, Debug)]
pub enum Message {
    /// Carries pty output already coalesced by the pane's reader thread.
    Output {
        /// The pane's own identity, not its position — see [`pane::PaneId`].
        id: pane::PaneId,
        data: Vec<u8>,
    },
    /// Signals that a pane's child exited or its pty errored out.
    Exited {
        id: pane::PaneId,
        reason: String,
    },
    /// Carries the new window size; every pane's grid follows from it.
    Resized(Size),
    /// Advances the wave and expires the toast; subscribed only while something
    /// is actually animating.
    Tick,
    /// Focuses the tab whose chip was clicked in the tab bar.
    FocusTab(usize),
    /// Focuses a pane by its tab-local index.
    FocusPane(usize),
    /// A pane-local mouse event: forwarded into the application (when it tracks
    /// the mouse) or into selection / viewport scrolling.
    Mouse {
        tab: usize,
        pane: usize,
        ev: ghostwave::term::MouseEv,
        /// Modifier state for the pane's encoder; iced's mouse events do not
        /// carry it.
        mods: libghostty_vt::key::Mods,
    },
    /// Dispatches a key press through [`keys::keypress`] and then
    /// [`input::key_press`], or [`input::repeat`] when the platform repeated it.
    KeyDown {
        key: keyboard::Key,
        physical: keyboard::key::Physical,
        modifiers: keyboard::Modifiers,
        text: Option<String>,
        /// The platform generated this one because the key is held down.
        repeat: bool,
    },
    /// Matters only to a pane: the kitty keyboard protocol lets an application ask
    /// for release events, and a modal bind must not leak its release into the shell.
    KeyUp {
        key: keyboard::Key,
        physical: keyboard::key::Physical,
        modifiers: keyboard::Modifiers,
    },
    /// The pane-grid layer's split drag: the split's new ratio, in `[0, 1]`.
    GridResize {
        split: pane_grid::Split,
        ratio: f32,
    },
    /// The pane-grid layer's pane drag: picked, dropped or cancelled.
    GridDrag(pane_grid::DragEvent),
    /// A message from the command palette overlay.
    Palette(palette::PaletteMessage),
    /// A message from the scrollback search overlay.
    Search(search_overlay::SearchMessage),
    /// A message from an AI chat pane's own widgets: its prompt input, its send
    /// button, its clear button.
    Ai {
        /// The pane's own identity, not its position.
        id: pane::PaneId,
        msg: ai::AiMessage,
    },
    /// A message no handler acts on: for a widget that must emit one but whose value
    /// is owned elsewhere.
    Noop,
}

struct App {
    state: AppState,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        let config = Config::load();
        let cell = pane::cell_dims(FONT_BYTES, config.font_size, config.row_gap);
        let state = AppState::new(config, cell, START_WINDOW).expect("spawn the first pane");
        eprintln!(
            "[ghost-iced] cell {}x{} px, baseline {}, theme {}, frames {}, tab '{}' with {} pane(s)",
            cell.w,
            cell.h,
            cell.baseline,
            state.theme.name,
            state.show_frames,
            state.current().name,
            state.current().panes.iter().filter(|p| p.is_some()).count()
        );
        (Self { state }, Task::none())
    }

    fn title(&self) -> String {
        let tab = self.state.current();
        if let Some(content) = tab.entry(tab.active).and_then(PaneEntry::content) {
            return format!("{} — ghostwave", content.label());
        }
        if self.state.config.dynamic_title != crate::config::DynamicTitleMode::Off {
            if let Some(pane) = tab.pane(tab.active) {
                let guard = pane.lock().unwrap();
                let title = guard.cached_title();
                if !title.is_empty() {
                    return format!("{title} — ghostwave");
                }
            }
        }
        "ghostwave (iced)".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Output { id, data } => {
                if let Some(pane) = self.state.pane_by_id(id) {
                    let mut guard = pane.lock().unwrap();
                    guard.ingest(&data);
                    guard.refresh_search();
                    self.state.pty_activity = true;
                }
            }
            Message::Exited { id, reason } => {
                if let Some(pane) = self.state.pane_by_id(id) {
                    pane.lock().unwrap().exited(&reason);
                }
            }
            Message::Resized(size) => self.state.set_window(size),
            Message::Tick => self.state.tick(),
            Message::FocusTab(index) => {
                if index < self.state.tabs.len() {
                    self.state.active_tab = index;
                    self.state.relayout();
                }
            }
            Message::FocusPane(index) => self.state.focus(index),
            Message::Mouse {
                tab,
                pane,
                ev,
                mods,
            } => {
                if let Some(pane_ref) = self.state.pane_mut(tab, pane) {
                    let mut guard = pane_ref.lock().unwrap();
                    if std::env::var_os("GHOST_ICED_DEBUG").is_some() {
                        eprintln!(
                            "[ghost-iced] mouse tab {tab} pane {pane} id {} {:?} x {:.1} y {:.1} button_down {:?}",
                            guard.id.0, ev.kind, ev.x, ev.y, ev.button_down
                        );
                    }
                    let (w, h) = guard
                        .last_bounds
                        .map(|b| (b.width, b.height))
                        .unwrap_or_else(|| {
                            (
                                (guard.term.cols as u32 * self.state.cell.w) as f32,
                                (guard.term.rows as u32 * self.state.cell.h) as f32,
                            )
                        });
                    let rect = ghostwave::layout::Rect {
                        x: 0.0,
                        y: 0.0,
                        w,
                        h,
                    };
                    guard.mouse_event(
                        rect,
                        self.state.cell,
                        self.state.config.padding,
                        ev,
                        mods,
                    );
                }
            }
            Message::KeyDown {
                key,
                physical,
                modifiers,
                text,
                repeat,
            } => {
                keys::note_modifiers(modifiers);
                if !self.overlay_key(&key, modifiers, text.as_deref()) {
                    if let Some(kp) = keys::keypress(&key, physical, modifiers, text.as_deref()) {
                        if repeat {
                            input::repeat(&mut self.state, kp);
                        } else {
                            input::key_press(&mut self.state, kp, key::Action::Press);
                        }
                    }
                }
            }
            Message::KeyUp {
                key,
                physical,
                modifiers,
            } => {
                keys::note_modifiers(modifiers);
                if !self.overlay_open() {
                    if let Some(kp) = keys::keypress(&key, physical, modifiers, None) {
                        input::key_release(&mut self.state, kp);
                    }
                }
            }
            Message::GridResize { split, ratio } => self.state.grid_resize(split, ratio),
            Message::GridDrag(event) => self.state.grid_drag(event),
            Message::Palette(message) => {
                if let Some(palette) = self.state.palette.as_mut() {
                    match palette.update(message) {
                        Some(palette::PaletteAction::Execute(action)) => {
                            self.state.palette = None;
                            input::execute(&mut self.state, action);
                        }
                        Some(palette::PaletteAction::Close) => self.state.palette = None,
                        None => {}
                    }
                }
            }
            Message::Search(message) => {
                let closing = matches!(message, search_overlay::SearchMessage::Close);
                let pane = self.state.active_pane();
                let mut term = pane.as_ref().map(|pane| pane.lock().unwrap());
                if let Some(search) = self.state.search.as_mut() {
                    search.update_with_term(message, term.as_mut().map(|guard| &mut guard.term));
                }
                if let Some(guard) = term.as_mut() {
                    guard.refresh_search();
                    let matches = guard.term.search.matches.len();
                    let current = guard.term.search.current;
                    if let Some(search) = self.state.search.as_mut() {
                        search.set_total_matches(matches);
                        search.set_match_index(current);
                    }
                }
                if closing {
                    self.state.search = None;
                }
            }
            Message::Ai { id, msg } => {
                if let Some(content) = self.state.content_by_id_mut(id) {
                    if let Some(ai) = content.as_ai_mut() {
                        ai.update(msg);
                        ai.poll();
                    }
                }
            }
            Message::Noop => {}
        }
        if self.state.quit {
            return iced::exit();
        }
        Task::none()
    }

    /// Whether an overlay currently owns the keyboard.
    fn overlay_open(&self) -> bool {
        self.state.palette.is_some() || self.state.search.is_some()
    }

    /// Routes a key to the open overlay, if any; returns whether one consumed it.
    ///
    /// Overlays are exclusive: while one is up the panes are sent no key at all,
    /// and the terminal keymap is not consulted — a key an overlay does not want
    /// is dropped rather than falling through to the shell.
    fn overlay_key(
        &mut self,
        key: &keyboard::Key,
        modifiers: keyboard::Modifiers,
        text: Option<&str>,
    ) -> bool {
        if self.state.palette.is_some() {
            self.palette_key(key, modifiers, text);
            return true;
        }
        if self.state.search.is_some() {
            self.search_key(key, modifiers, text);
            return true;
        }
        false
    }

    /// Applies a key to the command palette: navigation, submission and dismissal
    /// through the palette's own iced handler, text through its query.
    fn palette_key(
        &mut self,
        key: &keyboard::Key,
        modifiers: keyboard::Modifiers,
        text: Option<&str>,
    ) {
        let action = self
            .state
            .palette
            .as_mut()
            .and_then(|palette| palette.handle_iced_key(key, &modifiers));
        match action {
            Some(palette::PaletteAction::Execute(action)) => {
                self.state.palette = None;
                input::execute(&mut self.state, action);
            }
            Some(palette::PaletteAction::Close) => self.state.palette = None,
            None => {
                if let Some(palette) = self.state.palette.as_mut() {
                    if is_backspace(key) {
                        palette.backspace();
                    } else if let Some(c) = typed_char(text) {
                        palette.type_char(c);
                    }
                }
            }
        }
    }

    /// Applies a key to the search overlay through the module's own key map,
    /// driving the search actions so the overlay and the pane's search stay in
    /// step.
    fn search_key(
        &mut self,
        key: &keyboard::Key,
        modifiers: keyboard::Modifiers,
        text: Option<&str>,
    ) {
        match search_overlay::handle_key(key, modifiers) {
            Some(search_overlay::SearchMessage::Close) => input::close_search(&mut self.state),
            Some(search_overlay::SearchMessage::NextMatch) => {
                input::execute(&mut self.state, Action::SearchNext)
            }
            Some(search_overlay::SearchMessage::PrevMatch) => {
                input::execute(&mut self.state, Action::SearchPrev)
            }
            Some(search_overlay::SearchMessage::ToggleCaseSensitive) => {
                input::execute(&mut self.state, Action::SearchToggleCase)
            }
            Some(search_overlay::SearchMessage::ToggleRegex) => {
                if let Some(search) = self.state.search.as_mut() {
                    search.toggle_regex();
                }
            }
            Some(search_overlay::SearchMessage::QueryChanged(_)) => {}
            None => {
                if is_backspace(key) {
                    input::search_backspace(&mut self.state);
                } else if let Some(c) = typed_char(text) {
                    input::search_push_char(&mut self.state, c);
                }
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let tab = self.state.current();
        let mouse_tracking: Vec<usize> = tab
            .panes
            .iter()
            .enumerate()
            .filter_map(|(index, entry)| {
                let handle = entry.as_ref()?.term()?;
                handle.lock().unwrap().mouse_tracking().then_some(index)
            })
            .collect();
        let chrome = ChromeData {
            tabs: self
                .state
                .tabs
                .iter()
                .enumerate()
                .map(|(i, t)| (t.label(i), i == self.state.active_tab))
                .collect(),
            mode_label: self.state.mode.label().to_string(),
            hints: self.state.mode.hints().to_string(),
            theme_name: self.state.config.theme.clone(),
            sync_panes: tab.sync_panes,
            wave: self.state.config.visual_wave.then_some(chrome::WaveParams {
                energy: self.state.wave_energy,
                phase: self.state.wave_phase,
                amplitude: self.state.config.wave_amplitude,
            }),
            toast: self.state.toast.as_ref().map(|(m, _)| m.clone()),
            rename: match self.state.mode {
                ghostwave::keymap::Mode::RenameTab => {
                    Some(("Tab".to_string(), self.state.rename_buf.clone()))
                }
                ghostwave::keymap::Mode::RenamePane => {
                    Some(("Pane".to_string(), self.state.rename_buf.clone()))
                }
                _ => None,
            },
        };

        let base = panes_grid::view(&self.state, mouse_tracking, chrome);
        if self.state.search.is_none() && self.state.palette.is_none() {
            return base;
        }
        let mut layers: Vec<Element<'_, Message>> = vec![base];
        if let Some(search) = self.state.search.as_ref() {
            layers.push(search_overlay::search_overlay_mapped(
                search,
                &self.state.theme,
                search_overlay::OverlayPosition::TopRight,
                Message::Search,
            ));
        }
        if let Some(palette) = self.state.palette.as_ref() {
            layers.push(palette::view(palette, &self.state.theme).map(Message::Palette));
        }
        Stack::with_children(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            iced::event::listen_with(key_listener),
            iced::window::resize_events().map(|(_id, size)| Message::Resized(size)),
        ];
        match self.state.tick_rate() {
            app::TickRate::Off => {}
            rate => subs.push(Subscription::run_with(
                tick::TickSlot { rate },
                tick::stream,
            )),
        }
        for tab in &self.state.tabs {
            for handle in tab.term_handles() {
                let slot = { handle.lock().unwrap().rx.clone() };
                subs.push(Subscription::run_with(slot, pty_bridge::stream));
            }
        }
        Subscription::batch(subs)
    }
}

/// Whether the event is the Backspace key.
fn is_backspace(key: &keyboard::Key) -> bool {
    matches!(key, keyboard::Key::Named(keyboard::key::Named::Backspace))
}

/// The single printable character a key event produced, if it produced one.
///
/// A composed sequence (a dead-key pair, an IME pre-edit run) or a control
/// character — Ctrl+C hands over `\u{3}` — is not text to type.
fn typed_char(text: Option<&str>) -> Option<char> {
    let mut chars = text?.chars();
    let c = chars.next()?;
    if chars.next().is_none() && !c.is_control() {
        Some(c)
    } else {
        None
    }
}

/// Key events are the only runtime events this front-end listens for. Mouse input
/// goes through the canvas ([`pane_canvas::TerminalCanvas`]), which knows the rects.
fn key_listener(
    event: iced::Event,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        iced::Event::Keyboard(keyboard::Event::KeyPressed {
            key,
            physical_key,
            modifiers,
            text,
            repeat,
            ..
        }) => Some(Message::KeyDown {
            key,
            physical: physical_key,
            modifiers,
            text: text.map(|t| t.to_string()),
            repeat,
        }),
        iced::Event::Keyboard(keyboard::Event::KeyReleased {
            key,
            physical_key,
            modifiers,
            ..
        }) => Some(Message::KeyUp {
            key,
            physical: physical_key,
            modifiers,
        }),
        iced::Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
            keys::note_modifiers(modifiers);
            None
        }
        _ => None,
    }
}

fn main() -> iced::Result {
    let mut app = iced::application(App::new, App::update, App::view)
        .title(App::title)
        .subscription(App::subscription)
        .window_size(START_WINDOW)
        .font(fonts::PRIMARY_BYTES);
    for (_family, bytes) in fonts::fallback_bytes(12) {
        app = app.font(bytes);
    }
    app.run()
}
