//! The input path: iced key events -> keymap modes -> `Action`s -> effects.
//!
//! The bind table belongs to the app crate, so there is no second copy here;
//! actions whose feature is not implemented yet raise a toast instead of acting.
//!
//! The focused pane decides where a key goes: a terminal takes it through the
//! pty, while a content pane (preview, AI chat, visualizer) takes it through its
//! own state, and a terminal-only action skips a content pane entirely.

use ghostwave::keymap::{self, Action, Dispatch, KeyPress, Mode};
use ghostwave::layout::SplitDir;
use libghostty_vt::key::{self, Mods};

use crate::ai::AiPane;
use crate::app::AppState;
use crate::pane::{ContentPane, PaneEntry, SharedPane};
use crate::preview::{PreviewAction, PreviewPane};
use crate::search_overlay::SearchState;

/// Lines one preview page key moves.
const PREVIEW_PAGE: usize = 20;

/// Lines one AI pane page key scrolls.
const AI_PAGE: i32 = 10;

/// Resolves a key press against the bind table and applies the resulting action.
///
/// `action` is the platform's own action for the event: `Press`, or `Repeat`
/// for a repeat generated because the key is held (see [`repeat`]).
pub fn key_press(state: &mut AppState, kp: KeyPress, action: key::Action) {
    let k = kp.key;
    let m = kp.mods;

    state.bump_wave();

    if state.mode == Mode::Locked {
        if k == key::Key::G && m.contains(Mods::CTRL) {
            state.mode = Mode::Normal;
            state.toast("unlocked");
        } else {
            send(state, &kp, action);
        }
        return;
    }

    if keymap::is_quit(k, m) {
        state.quit = true;
        return;
    }

    if state.mode == Mode::Normal && remote_dead_key(state, &kp) {
        return;
    }

    if let Some(new_mode) = keymap::entry_bind(k, m) {
        state.mode = if state.mode == new_mode {
            Mode::Normal
        } else {
            new_mode
        };
        return;
    }

    match keymap::handle(state.mode, &kp) {
        Dispatch::Action(action) => {
            let was_modal = matches!(
                state.mode,
                Mode::Pane | Mode::Tab | Mode::Resize | Mode::Session
            );
            execute(state, action);
            if was_modal
                && matches!(
                    state.mode,
                    Mode::Pane | Mode::Tab | Mode::Resize | Mode::Session
                )
            {
                state.mode = Mode::Normal;
            }
        }
        Dispatch::Pass => {
            if content_key(state, &kp) {
                return;
            }
            if state.mode == Mode::Normal {
                if kp.key == key::Key::ArrowUp && kp.mods == Mods::SHIFT {
                    execute(state, Action::ScrollUp(1));
                    return;
                }
                if kp.key == key::Key::ArrowDown && kp.mods == Mods::SHIFT {
                    execute(state, Action::ScrollDown(1));
                    return;
                }
            }
            send(state, &kp, action);
        }
        Dispatch::Char(c) => match state.mode {
            Mode::RenameTab | Mode::RenamePane | Mode::RenameSession => {
                state.rename_buf.push(c);
            }
            _ => {}
        },
        Dispatch::Ignore => {}
    }
}

/// Keys a DISCONNECTED remote pane takes: Enter connects it, `k` kills its remote
/// session, Esc stops the auto-reconnect loop. Every other key is swallowed —
/// there is no process in the pane to send it to.
///
/// Reports whether the key belonged to that state, which is also how the keys
/// that follow are kept away from a pane that cannot receive them.
fn remote_dead_key(state: &mut AppState, kp: &KeyPress) -> bool {
    if !state.remote_pane_disconnected() {
        return false;
    }
    let (tab, pane) = (state.active_tab, state.current().active);
    match kp.key {
        key::Key::Enter => state.connect_remote_pane(tab, pane),
        key::Key::K => {
            state.kill_remote_pane(tab, pane);
        }
        key::Key::Escape => state.stop_remote_reconnect(),
        _ => {}
    }
    true
}

/// Sends a platform key repeat to the active pane, bypassing the bind table.
///
/// In the modal modes repeats are dropped: the press there was consumed as an
/// [`Action`], so the shell must not also see the held key.
pub fn repeat(state: &mut AppState, kp: KeyPress) {
    if !matches!(state.mode, Mode::Normal | Mode::Locked) {
        return;
    }
    if content_key(state, &kp) {
        return;
    }
    send(state, &kp, key::Action::Repeat);
}

/// Forwards a key release to the pane unless a bind consumed the press.
///
/// A key the app consumed on press must not have its release reach the terminal,
/// or a modal bind leaks stray releases into the shell.
pub fn key_release(state: &mut AppState, kp: KeyPress) {
    if state.mode == Mode::Locked {
        if kp.key == key::Key::G && kp.mods.contains(Mods::CTRL) {
            return;
        }
        send(state, &kp, key::Action::Release);
        return;
    }
    if state.mode == Mode::Normal {
        let (k, m) = (kp.key, kp.mods);
        if keymap::is_quit(k, m)
            || keymap::entry_bind(k, m).is_some()
            || keymap::global_binds(k, m).is_some()
        {
            return;
        }
        if let Some(pane) = state.active_pane() {
            pane.lock().unwrap().key_press(&kp, key::Action::Release);
        }
    }
}

/// Hands a key to the terminal: the active pane, or every terminal in the tab
/// when synchronised input is on.
///
/// Terminals only: a content pane is neither the active terminal nor part of the
/// broadcast, so a key never reaches one through this path.
fn send(state: &mut AppState, kp: &KeyPress, action: key::Action) {
    if state.current().sync_panes {
        for handle in state.current().term_handles() {
            handle.lock().unwrap().key_press(kp, action);
        }
        return;
    }
    if let Some(pane) = state.active_pane() {
        pane.lock().unwrap().key_press(kp, action);
    }
}

/// Routes a key to the focused content pane; returns whether one took it.
///
/// A terminal returns `false` here, so the terminal path is untouched; a content
/// pane returns whether the key is one of its own.
fn content_key(state: &mut AppState, kp: &KeyPress) -> bool {
    let Some(entry) = state.focused_entry_mut() else {
        return false;
    };
    let PaneEntry::Content(content) = entry else {
        return false;
    };
    match &mut **content {
        ContentPane::Preview { pane, .. } => preview_key(pane, kp),
        ContentPane::Ai { pane, .. } => ai_key(pane, kp),
        ContentPane::Visualizer { pane, .. } => visualizer_key(pane, kp),
    }
}

/// The preview action a press maps to, if it maps to one.
fn preview_action(kp: &KeyPress) -> Option<PreviewAction> {
    match kp.text {
        Some('j') => Some(PreviewAction::ScrollDown(1)),
        Some('k') => Some(PreviewAction::ScrollUp(1)),
        Some('+') | Some('=') => Some(PreviewAction::ZoomIn),
        Some('-') | Some('_') => Some(PreviewAction::ZoomOut),
        Some('0') => Some(PreviewAction::ResetView),
        None => match kp.key {
            key::Key::ArrowUp => Some(PreviewAction::ScrollUp(1)),
            key::Key::ArrowDown => Some(PreviewAction::ScrollDown(1)),
            key::Key::PageUp => Some(PreviewAction::ScrollUp(PREVIEW_PAGE)),
            key::Key::PageDown => Some(PreviewAction::ScrollDown(PREVIEW_PAGE)),
            _ => None,
        },
        _ => None,
    }
}

/// Applies a preview's own key: scroll, zoom and reset. `j`/`k` and the arrows
/// scroll lines, the page keys scroll pages, `+`/`-` zoom and `0` resets.
fn preview_key(pane: &mut PreviewPane, kp: &KeyPress) -> bool {
    match preview_action(kp) {
        Some(action) => {
            pane.handle_action(action);
            true
        }
        None => false,
    }
}

/// Applies an AI pane's own key: text is input, Enter submits, the editing and
/// scrolling keys act on the prompt and the transcript.
fn ai_key(pane: &mut AiPane, kp: &KeyPress) -> bool {
    match kp.key {
        key::Key::Enter => {
            pane.update(crate::ai::AiMessage::Submit);
            true
        }
        key::Key::Backspace => {
            pane.backspace();
            true
        }
        key::Key::ArrowUp => {
            pane.scroll_lines(-1);
            true
        }
        key::Key::ArrowDown => {
            pane.scroll_lines(1);
            true
        }
        key::Key::PageUp => {
            pane.scroll_lines(-AI_PAGE);
            true
        }
        key::Key::PageDown => {
            pane.scroll_lines(AI_PAGE);
            true
        }
        _ => match kp.text {
            Some(c) if !c.is_control() => {
                pane.type_char(c);
                true
            }
            _ => false,
        },
    }
}

/// Applies the visualizer's own key: `v`/`t` cycles visualizer style; `c`/Space cycles colour mode; `+`/`-` adjusts speed.
fn visualizer_key(pane: &mut crate::visualizer::VisualizerPane, kp: &KeyPress) -> bool {
    match kp.text {
        Some('v') | Some('t') => {
            pane.cycle_viz_type();
            true
        }
        Some('c') | Some(' ') => {
            pane.cycle_mode();
            true
        }
        Some('+') | Some('=') => {
            pane.speed_up();
            true
        }
        Some('-') | Some('_') => {
            pane.speed_down();
            true
        }
        _ => false,
    }
}

/// Arms whose feature is not implemented yet raise a toast instead of acting.
///
/// Shared with the overlay key path in [`crate::main`], so an action reached from
/// the palette and the same action reached from a bind have one implementation.
pub(crate) fn execute(state: &mut AppState, action: Action) {
    match action {
        Action::NewPane | Action::NewPaneDown | Action::NewPaneRight => {
            let dir = if action == Action::NewPaneRight {
                SplitDir::Horizontal
            } else {
                SplitDir::Vertical
            };
            split(state, dir);
        }
        Action::NewPaneStacked => match state.split_stacked() {
            Ok(_) => {}
            Err(e) => state.toast(format!("spawn failed: {e}")),
        },
        Action::ClosePane => state.close_active(),
        Action::MoveFocus(dir) => state.focus_dir(dir),
        Action::SwitchFocus => state.focus_next(),
        Action::ToggleZoom => state.toggle_zoom(),
        Action::ToggleFrames => state.toggle_frames(),
        Action::RenamePane => {
            state.mode = Mode::RenamePane;
            state.rename_buf.clear();
        }
        Action::CycleStack => state.cycle_stack(),
        Action::ToggleFloating => {
            let mut scratchpad = std::mem::take(&mut state.scratchpad);
            scratchpad.toggle_float_pane(state);
            state.scratchpad = scratchpad;
        }
        Action::ToggleHideFloat => {
            let mut scratchpad = std::mem::take(&mut state.scratchpad);
            scratchpad.toggle_hide_float(state);
            state.scratchpad = scratchpad;
        }
        Action::CycleFloat => {
            let mut scratchpad = std::mem::take(&mut state.scratchpad);
            scratchpad.cycle_float(state);
            state.scratchpad = scratchpad;
        }
        Action::ToggleScratchpad => {
            let mut scratchpad = std::mem::take(&mut state.scratchpad);
            scratchpad.toggle(state);
            state.scratchpad = scratchpad;
        }

        Action::NewTab => state.new_tab(),
        Action::CloseTab => state.close_tab(),
        Action::NextTab => {
            let n = state.tabs.len();
            if n > 0 {
                state.goto_tab((state.active_tab + 1) % n);
            }
        }
        Action::PrevTab => {
            let n = state.tabs.len();
            if n > 0 {
                state.goto_tab((state.active_tab + n - 1) % n);
            }
        }
        Action::GoToTab(i) => {
            let n = state.tabs.len();
            if i >= 1 && (i as usize) <= n {
                state.goto_tab(i as usize - 1);
            }
        }
        Action::ToggleLastTab => state.toggle_last_tab(),
        Action::RenameTab => {
            state.mode = Mode::RenameTab;
            state.rename_buf.clear();
        }
        Action::BreakPane => state.break_pane(),
        Action::MoveTab(dir) => state.move_tab(dir),

        Action::Resize(dir, increase) => {
            state.resize(dir, if increase { 0.06 } else { -0.06 });
        }
        Action::ResizeAll(increase) => {
            let delta = if increase { 0.06 } else { -0.06 };
            state.resize(ghostwave::layout::Dir::Right, delta);
            state.resize(ghostwave::layout::Dir::Down, delta);
        }

        Action::ScrollUp(n) => scroll(
            state,
            |p| p.scroll(-n),
            |p| p.handle_action(PreviewAction::ScrollUp(n as usize)),
        ),
        Action::ScrollDown(n) => scroll(
            state,
            |p| p.scroll(n),
            |p| p.handle_action(PreviewAction::ScrollDown(n as usize)),
        ),
        Action::PageUp => scroll(
            state,
            |p| p.scroll_page(-1),
            |p| p.handle_action(PreviewAction::ScrollUp(PREVIEW_PAGE)),
        ),
        Action::PageDown => scroll(
            state,
            |p| p.scroll_page(1),
            |p| p.handle_action(PreviewAction::ScrollDown(PREVIEW_PAGE)),
        ),
        Action::HalfPageUp => scroll(
            state,
            |p| p.scroll_half_page(-1),
            |p| p.handle_action(PreviewAction::ScrollUp(PREVIEW_PAGE / 2)),
        ),
        Action::HalfPageDown => scroll(
            state,
            |p| p.scroll_half_page(1),
            |p| p.handle_action(PreviewAction::ScrollDown(PREVIEW_PAGE / 2)),
        ),
        Action::ScrollToBottom => scroll(state, |p| p.scroll_to_bottom(), |_| {}),
        Action::EditScrollback => state.toast("edit-scrollback: not implemented yet"),

        Action::OpenSearch | Action::SearchStart => open_search(state),
        Action::SearchNext => step_search(state, true),
        Action::SearchPrev => step_search(state, false),
        Action::SearchToggleCase => {
            if let Some(search) = state.search.as_mut() {
                search.toggle_case_sensitive();
            }
            sync_search(state);
        }
        Action::SearchToggleWholeWord => toggle_whole_word(state),
        Action::SearchToggleWrap => toggle_wrap(state),
        Action::ConfirmSearch => sync_search(state),

        Action::MovePane(dir) => state.move_pane(dir),
        Action::MovePaneNext => state.move_pane_next(),
        Action::MovePanePrev => state.move_pane_prev(),

        Action::Quit => state.quit = true,
        Action::Lock => state.mode = Mode::Locked,
        Action::Unlock => state.mode = Mode::Normal,
        Action::Copy => {
            if let Some(pane) = state.active_pane() {
                let ok = pane.lock().unwrap().copy_selection().unwrap_or(false);
                state.toast(if ok { "copied" } else { "nothing to copy" });
            }
        }
        Action::Paste => {
            if let Some(pane) = state.active_pane() {
                pane.lock().unwrap().paste_clipboard();
            }
        }
        Action::ClearTerminal => {
            if let Some(pane) = state.active_pane() {
                let _ = pane.lock().unwrap().clear_terminal();
            }
        }
        Action::Theme(name) => state.apply_theme(&name),
        Action::FocusPane(i) => state.focus_index(i as usize),
        Action::ToggleSyncPanes => {
            let on = state.toggle_sync();
            state.toast(if on {
                "synchronized input ON (broadcasting to all panes)"
            } else {
                "synchronized input OFF"
            });
        }
        Action::OpenCommandBar => {
            state.palette = Some(crate::palette::PaletteState::new(
                crate::palette::default_items(&ghostwave::theme::names()),
            ));
        }
        Action::Ghostwave => new_content(state, ContentPane::visualizer()),
        Action::NewAi => new_content(state, ContentPane::ai(state.config.ai.clone())),
        Action::OpenFile(path) => new_content(state, ContentPane::preview(path)),
        Action::SessionManager => crate::ui_session::open_picker(state),
        Action::SessionNew => {
            let name = crate::ui_session::next_name();
            crate::ui_session::start(state, &name);
        }
        Action::SessionRename => {
            state.mode = Mode::RenameSession;
            state.rename_buf = state.current_session.clone();
            state.rename_target = state.current_session.clone();
        }
        Action::SessionDelete => crate::ui_session::delete_current(state),
        Action::Detach => {
            crate::ui_session::save(state);
            let name = state.current_session.clone();
            state.toast(format!("session {name} saved — quitting"));
            state.quit = true;
        }
        Action::ConnectSsh(host) => state.connect_ssh(&host),
        Action::ConnectDurableSsh(host) => state.connect_durable_ssh(&host),
        Action::KillRemoteSession => state.kill_active_remote_pane(),

        Action::Backspace => match state.mode {
            Mode::RenameTab | Mode::RenamePane | Mode::RenameSession => {
                state.rename_buf.pop();
            }
            _ => {}
        },
        Action::ConfirmRename => {
            let name = std::mem::take(&mut state.rename_buf);
            match state.mode {
                Mode::RenameTab => {
                    state.mode = Mode::Normal;
                    if !name.is_empty() {
                        state.tabs[state.active_tab].name = name;
                    }
                }
                Mode::RenamePane => {
                    state.mode = Mode::Normal;
                    if !name.is_empty() {
                        if let Some(pane) = state.active_pane() {
                            pane.lock().unwrap().term.name = name;
                        }
                    }
                }
                Mode::RenameSession => {
                    state.mode = Mode::Normal;
                    crate::ui_session::rename(state, &name);
                }
                _ => state.mode = Mode::Normal,
            }
        }
        Action::ExitMode => state.mode = Mode::Normal,
        Action::Toast(msg) => state.toast(msg),
    }
}

/// A spawn failure is reported as a toast rather than a panic.
fn split(state: &mut AppState, dir: SplitDir) {
    if let Err(e) = state.split(dir) {
        state.toast(format!("spawn failed: {e}"));
    }
}

/// Adds a content pane beside the focused one and reports what opened.
///
/// The pane takes its own cell, as a split terminal does, so nothing is
/// replaced; a preview, an AI chat and the visualizer all arrive this way.
fn new_content(state: &mut AppState, content: ContentPane) {
    let label = content.label();
    state.add_content(SplitDir::Horizontal, content);
    state.toast(format!("{label} pane opened"));
}

/// Re-runs a pane's own match pass and mirrors the result into the open overlay.
///
/// The core recomputes search matches only inside its own render function, which this
/// front-end never calls, so every point where the query or its options change drives
/// the pass itself ([`crate::pane::Pane::refresh_search`]) — and the overlay that shows
/// the count reads it back from the pane.
fn pane_search_refresh(state: &mut AppState, pane: &SharedPane) {
    let (matches, current) = {
        let mut guard = pane.lock().unwrap();
        guard.refresh_search();
        (guard.term.search.matches.len(), guard.term.search.current)
    };
    if let Some(overlay) = state.search.as_mut() {
        overlay.set_total_matches(matches);
        overlay.set_match_index(current);
    }
}

/// Opens the scrollback search overlay and arms the focused pane's search with
/// the overlay's query.
///
/// Re-opening with a query already typed keeps it: the overlay is the query's
/// owner while it is up.
pub(crate) fn open_search(state: &mut AppState) {
    let pane = state.active_pane();
    let overlay = state.search.get_or_insert_with(SearchState::new);
    let query = overlay.query.clone();
    let case_sensitive = overlay.case_sensitive;
    if let Some(pane) = &pane {
        {
            let mut guard = pane.lock().unwrap();
            guard.term.search.start();
            if !query.is_empty() {
                guard.term.search.set_query(query);
            }
            guard.term.search.case_sensitive = case_sensitive;
            guard.term.search.dirty = true;
            guard.term.force_repaint = true;
        }
        pane_search_refresh(state, pane);
    }
}

/// Closes the overlay and disarms the focused pane's search.
pub(crate) fn close_search(state: &mut AppState) {
    if let Some(pane) = state.active_pane() {
        let mut guard = pane.lock().unwrap();
        guard.term.search.stop();
        guard.term.force_repaint = true;
    }
    state.search = None;
}

/// Pushes the overlay's query and case setting into the focused pane's search and
/// re-runs the pane's match pass for the new query.
pub(crate) fn sync_search(state: &mut AppState) {
    let pane = state.active_pane();
    {
        let Some(overlay) = state.search.as_ref() else {
            return;
        };
        if let Some(pane) = &pane {
            let mut guard = pane.lock().unwrap();
            overlay.sync_to_term(&mut guard.term);
        }
    }
    if let Some(pane) = &pane {
        pane_search_refresh(state, pane);
    }
}

/// Appends a typed character to the overlay's query and re-runs the pane's search.
pub(crate) fn search_push_char(state: &mut AppState, c: char) {
    if let Some(overlay) = state.search.as_mut() {
        let query = format!("{}{c}", overlay.query);
        overlay.set_query(query);
    }
    sync_search(state);
}

/// Drops the overlay query's last character and re-runs the pane's search.
pub(crate) fn search_backspace(state: &mut AppState) {
    if let Some(overlay) = state.search.as_mut() {
        let mut query = overlay.query.clone();
        if query.pop().is_some() {
            overlay.set_query(query);
        }
    }
    sync_search(state);
}

/// Steps the overlay and the focused pane's search to the next or previous match.
///
/// The core's matches are viewport rows, so there is no scroll to follow: the
/// scan and the highlight both live in the painted viewport.
fn step_search(state: &mut AppState, forward: bool) {
    let pane = state.active_pane();
    if let Some(pane) = &pane {
        pane_search_refresh(state, pane);
    }
    let overlay = state.search.get_or_insert_with(SearchState::new);
    match pane {
        Some(pane) => {
            let mut guard = pane.lock().unwrap();
            if forward {
                overlay.step_next_with_term(&mut guard.term);
            } else {
                overlay.step_prev_with_term(&mut guard.term);
            }
        }
        None if forward => overlay.next_match(),
        None => overlay.prev_match(),
    }
}

/// Flips the focused pane's whole-word search flag, re-runs the scan and reports
/// the new value.
fn toggle_whole_word(state: &mut AppState) {
    let Some(pane) = state.active_pane() else {
        return;
    };
    let on = {
        let mut guard = pane.lock().unwrap();
        let on = !guard.term.search.whole_word;
        guard.term.search.whole_word = on;
        guard.term.search.dirty = true;
        guard.term.force_repaint = true;
        on
    };
    pane_search_refresh(state, &pane);
    state.toast(if on {
        "search: whole word on"
    } else {
        "search: whole word off"
    });
}

/// Flips the focused pane's search wrap flag and reports its new value.
fn toggle_wrap(state: &mut AppState) {
    let Some(pane) = state.active_pane() else {
        return;
    };
    let on = {
        let mut guard = pane.lock().unwrap();
        let on = !guard.term.search.wrap;
        guard.term.search.wrap = on;
        on
    };
    state.toast(if on {
        "search: wrap on"
    } else {
        "search: wrap off"
    });
}

/// Routes a scroll to the focused pane: a terminal moves its viewport, a preview
/// pane its own scroll offset, and any other content pane ignores it.
fn scroll(
    state: &mut AppState,
    term: impl FnOnce(&mut crate::pane::Pane),
    preview: impl FnOnce(&mut PreviewPane),
) {
    let Some(entry) = state.focused_entry_mut() else {
        return;
    };
    match entry {
        PaneEntry::Term(pane) => term(&mut pane.lock().unwrap()),
        PaneEntry::Content(content) => {
            if let Some(pane) = content.as_preview_mut() {
                preview(pane);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    //! The keymap modes, driven through the real dispatch path with real panes:
    //! each test presses the keys a user would and asserts the resulting state.

    use super::*;
    use ghostwave::config::Config;
    use ghostwave::keymap::Mode;
    use ghostwave::term::{ReconnectState, RemoteSpec};
    use iced::widget::pane_grid::{Axis, Node as GridNode};
    use libghostty_vt::key::Key;

    use crate::app::GridPane;
    use crate::preview::PreviewKind;

    fn state() -> AppState {
        let config = Config::load();
        let cell = crate::pane::cell_dims(crate::FONT_BYTES, config.font_size, config.row_gap);
        AppState::new(config, cell, iced::Size::new(1200.0, 800.0)).expect("spawn the first pane")
    }

    /// A press with no text payload, as the encoder sees a modifier chord.
    fn press(state: &mut AppState, key: Key, mods: Mods) {
        key_press(
            state,
            KeyPress {
                key,
                mods,
                text: None,
            },
            key::Action::Press,
        );
    }

    fn typed(state: &mut AppState, key: Key, c: char) {
        key_press(
            state,
            KeyPress {
                key,
                mods: Mods::empty(),
                text: Some(c),
            },
            key::Action::Press,
        );
    }

    fn ctrl(state: &mut AppState, key: Key) {
        press(state, key, Mods::CTRL);
    }

    fn live_panes(state: &AppState) -> usize {
        state.current().panes.iter().filter(|p| p.is_some()).count()
    }

    fn toast(state: &AppState) -> String {
        state
            .toast
            .as_ref()
            .map(|(m, _)| m.clone())
            .unwrap_or_default()
    }

    /// A session name no other test in this process shares.
    fn unique_session(prefix: &str) -> String {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        format!("iced-{prefix}-{}-{n}", std::process::id())
    }

    /// Types text a character at a time, the way the rename prompt receives it.
    fn type_str(state: &mut AppState, text: &str) {
        for c in text.chars() {
            key_press(
                state,
                KeyPress {
                    key: Key::A,
                    mods: Mods::empty(),
                    text: Some(c),
                },
                key::Action::Press,
            );
        }
    }

    /// Sends a key to the open session picker through the same two calls main's
    /// key path makes: the overlay's own key map, then the session module.
    fn pick(state: &mut AppState, text: &str) {
        let key = iced::keyboard::Key::Character(text.into());
        let message = crate::sessions_overlay::handle_key(&key, iced::keyboard::Modifiers::default())
            .expect("a picker bind");
        crate::ui_session::picker_message(state, message);
    }

    #[test]
    fn mode_entry_binds_enter_and_leave_each_mode() {
        let mut s = state();
        assert_eq!(s.mode, Mode::Normal);
        ctrl(&mut s, Key::P);
        assert_eq!(s.mode, Mode::Pane, "Ctrl+p enters pane mode");
        ctrl(&mut s, Key::P);
        assert_eq!(s.mode, Mode::Normal);
        ctrl(&mut s, Key::T);
        assert_eq!(s.mode, Mode::Tab);
        ctrl(&mut s, Key::N);
        assert_eq!(s.mode, Mode::Resize);
        ctrl(&mut s, Key::S);
        assert_eq!(s.mode, Mode::Scroll);
        ctrl(&mut s, Key::H);
        assert_eq!(s.mode, Mode::Move);
        ctrl(&mut s, Key::O);
        assert_eq!(s.mode, Mode::Session);
        ctrl(&mut s, Key::G);
        assert_eq!(s.mode, Mode::Locked);
    }

    #[test]
    fn pane_mode_splits_down_right_and_closes() {
        let mut s = state();
        assert_eq!(live_panes(&s), 1);
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::D, 'd');
        assert_eq!(live_panes(&s), 2);
        assert!(
            matches!(
                s.current().grid.layout(),
                GridNode::Split {
                    axis: Axis::Horizontal,
                    ..
                }
            ),
            "d must split downward, got {:?}",
            s.current().grid.layout()
        );
        assert_eq!(s.mode, Mode::Normal);

        ctrl(&mut s, Key::P);
        typed(&mut s, Key::R, 'r');
        assert_eq!(live_panes(&s), 3);
        assert_eq!(s.mode, Mode::Normal);

        ctrl(&mut s, Key::P);
        typed(&mut s, Key::X, 'x');
        assert_eq!(live_panes(&s), 2);

        ctrl(&mut s, Key::P);
        press(&mut s, Key::Escape, Mods::empty());
        assert_eq!(s.mode, Mode::Normal);
        assert_eq!(live_panes(&s), 2);
    }

    #[test]
    fn pane_mode_stacked_and_zoom_and_frames() {
        let mut s = state();
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::S, 's');
        let cell = {
            let tab = s.current();
            let focused = tab.focused().expect("a focused cell");
            tab.grid.get(focused).expect("its payload").clone()
        };
        assert_eq!(
            cell.panes.len(),
            2,
            "pane mode `s` stacks the new pane into the focused cell"
        );
        assert_eq!(cell.active_id(), Some(cell.panes[1]));
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::Z, 'z');
        assert!(s.current().grid.maximized().is_some());
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::Z, 'z');
        assert!(s.current().grid.maximized().is_none());
        let frames = s.show_frames;
        ctrl(&mut s, Key::P);
        press(&mut s, Key::Z, Mods::SHIFT);
        assert_eq!(s.show_frames, !frames);
    }

    #[test]
    fn tab_mode_creates_switches_and_renames() {
        let mut s = state();
        ctrl(&mut s, Key::T);
        typed(&mut s, Key::N, 'n');
        assert_eq!(s.tabs.len(), 2, "tab mode `n` opens a tab");
        assert_eq!(s.active_tab, 1);
        assert_eq!(s.mode, Mode::Normal);

        ctrl(&mut s, Key::T);
        typed(&mut s, Key::H, 'h');
        assert_eq!(s.active_tab, 0);
        ctrl(&mut s, Key::T);
        typed(&mut s, Key::L, 'l');
        assert_eq!(s.active_tab, 1);

        ctrl(&mut s, Key::T);
        typed(&mut s, Key::Digit1, '1');
        assert_eq!(s.active_tab, 0);

        ctrl(&mut s, Key::T);
        typed(&mut s, Key::R, 'r');
        assert_eq!(s.mode, Mode::RenameTab);
        typed(&mut s, Key::H, 'h');
        typed(&mut s, Key::I, 'i');
        assert_eq!(s.rename_buf, "hi");
        press(&mut s, Key::Backspace, Mods::empty());
        assert_eq!(s.rename_buf, "h");
        press(&mut s, Key::Enter, Mods::empty());
        assert_eq!(s.mode, Mode::Normal);
        assert_eq!(s.tabs[0].name, "h");
        assert_eq!(s.current().label(0), "h");

        ctrl(&mut s, Key::T);
        typed(&mut s, Key::X, 'x');
        assert_eq!(s.tabs.len(), 1);
        ctrl(&mut s, Key::T);
        typed(&mut s, Key::X, 'x');
        assert_eq!(s.tabs.len(), 1, "the last tab is replaced, not closed");
    }

    #[test]
    fn global_binds_work_from_normal_mode() {
        let mut s = state();
        press(&mut s, Key::D, Mods::CTRL | Mods::SHIFT);
        assert_eq!(live_panes(&s), 2);
        assert!(matches!(
            s.current().grid.layout(),
            GridNode::Split {
                axis: Axis::Horizontal,
                ..
            }
        ));
        press(&mut s, Key::D, Mods::CTRL);
        assert_eq!(live_panes(&s), 3);
        press(&mut s, Key::W, Mods::CTRL);
        assert_eq!(live_panes(&s), 2, "Ctrl+w closes the focused pane");
        press(&mut s, Key::T, Mods::CTRL | Mods::SHIFT);
        assert_eq!(s.tabs.len(), 2, "Ctrl+Shift+t opens a tab");
        press(&mut s, Key::W, Mods::CTRL | Mods::SHIFT);
        assert_eq!(s.tabs.len(), 1, "Ctrl+Shift+w closes the tab");

        press(&mut s, Key::M, Mods::CTRL);
        assert!(s.current().grid.maximized().is_some());
    }

    #[test]
    fn resize_and_move_modes_change_the_tree() {
        let mut s = state();
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::R, 'r');
        let before = format!("{:?}", s.current().grid.layout());
        ctrl(&mut s, Key::N);
        assert_eq!(s.mode, Mode::Resize);
        typed(&mut s, Key::L, 'l');
        assert_ne!(format!("{:?}", s.current().grid.layout()), before);

        ctrl(&mut s, Key::H);
        typed(&mut s, Key::L, 'l');
        assert_eq!(live_panes(&s), 2);
    }

    #[test]
    fn scrolling_and_clear_reach_the_pane() {
        let mut s = state();
        ctrl(&mut s, Key::S);
        assert_eq!(s.mode, Mode::Scroll);
        typed(&mut s, Key::K, 'k');
        typed(&mut s, Key::J, 'j');
        typed(&mut s, Key::D, 'd');
        typed(&mut s, Key::U, 'u');
        assert_eq!(s.mode, Mode::Scroll);
        press(&mut s, Key::Escape, Mods::empty());
        assert_eq!(s.mode, Mode::Normal);

        if let Some(pane) = s.active_pane() {
            let mut guard = pane.lock().unwrap();
            for i in 0..100 {
                let _ = guard.term.term.vt_write(format!("line {}\r\n", i).as_bytes());
            }
        }
        let sb_before = s.active_pane().unwrap().lock().unwrap().term.term.scrollbar().unwrap();
        press(&mut s, Key::PageUp, Mods::SHIFT);
        let sb_after = s.active_pane().unwrap().lock().unwrap().term.term.scrollbar().unwrap();
        assert!(sb_after.offset < sb_before.offset, "Shift+PageUp must scroll upward");

        press(&mut s, Key::ArrowUp, Mods::SHIFT);
        let sb_line = s.active_pane().unwrap().lock().unwrap().term.term.scrollbar().unwrap();
        assert!(sb_line.offset < sb_after.offset, "Shift+ArrowUp must scroll 1 line upward");

        press(&mut s, Key::ArrowDown, Mods::SHIFT);
        let sb_down = s.active_pane().unwrap().lock().unwrap().term.term.scrollbar().unwrap();
        assert_eq!(sb_down.offset, sb_after.offset, "Shift+ArrowDown must scroll 1 line downward");

        ctrl(&mut s, Key::K);
        press(&mut s, Key::C, Mods::CTRL | Mods::SHIFT);
        assert_eq!(toast(&s), "nothing to copy");
    }

    #[test]
    fn locked_mode_swallows_binds_and_only_ctrl_g_unlocks() {
        let mut s = state();
        ctrl(&mut s, Key::G);
        assert_eq!(s.mode, Mode::Locked);
        ctrl(&mut s, Key::P);
        assert_eq!(s.mode, Mode::Locked);
        press(&mut s, Key::T, Mods::CTRL | Mods::SHIFT);
        assert_eq!(s.tabs.len(), 1);
        press(&mut s, Key::D, Mods::CTRL);
        assert_eq!(live_panes(&s), 1);
        press(&mut s, Key::Escape, Mods::empty());
        assert_eq!(s.mode, Mode::Locked);
        ctrl(&mut s, Key::G);
        assert_eq!(s.mode, Mode::Normal);
        assert_eq!(toast(&s), "unlocked");
    }

    #[test]
    fn unimplemented_features_say_so_and_wired_ones_act() {
        let mut s = state();
        ctrl(&mut s, Key::O);
        assert_eq!(s.mode, Mode::Session);
        typed(&mut s, Key::W, 'w');
        assert!(s.session_picker.is_some(), "w opens the session manager");
        assert_eq!(s.mode, Mode::Normal);
        s.session_picker = None;

        assert!(s.palette.is_none());
        press(&mut s, Key::P, Mods::CTRL | Mods::SHIFT);
        assert!(s.palette.is_some(), "Ctrl+Shift+p opens the command palette");

        press(&mut s, Key::F, Mods::CTRL | Mods::SHIFT);
        assert!(s.search.is_some(), "Ctrl+Shift+f opens the search overlay");

        assert!(!s.scratchpad.is_open());
        press(&mut s, Key::F12, Mods::empty());
        assert!(s.scratchpad.is_open(), "F12 opens the scratchpad");
        assert!(s.scratchpad.pane().is_some(), "the scratchpad pane exists");
        press(&mut s, Key::F12, Mods::empty());
        assert!(!s.scratchpad.is_open(), "F12 hides it again");

        ctrl(&mut s, Key::P);
        typed(&mut s, Key::F, 'f');
        assert!(
            !s.scratchpad.floating.is_empty(),
            "the float bind really floats the focused pane"
        );
    }

    #[test]
    fn the_session_actions_reach_the_session_state() {
        let _guard = crate::ui_session::test_lock();
        let mut s = state();
        let before = unique_session("binds");
        s.current_session = before.clone();

        ctrl(&mut s, Key::O);
        assert_eq!(s.mode, Mode::Session);
        typed(&mut s, Key::W, 'w');
        assert!(s.session_picker.is_some(), "w opens the session manager");
        assert_eq!(s.mode, Mode::Normal, "the modal mode is left after the bind");
        assert_eq!(
            s.session_picker.as_ref().unwrap().selected_name(),
            Some(before.as_str()),
            "the picker selects the session the workspace is in"
        );

        pick(&mut s, "d");
        assert_eq!(
            toast(&s),
            "cannot delete the current session",
            "deleting the session in use is refused"
        );
        assert!(s.session_picker.is_some(), "and the picker stays open");

        pick(&mut s, "r");
        assert!(s.session_picker.is_none(), "r closes the picker for the prompt");
        assert_eq!(s.mode, Mode::RenameSession);
        assert_eq!(s.rename_buf, before, "the prompt starts on the session's name");
        type_str(&mut s, "-renamed");
        press(&mut s, Key::Enter, Mods::empty());
        let renamed = format!("{before}-renamed");
        assert_eq!(s.mode, Mode::Normal);
        assert_eq!(s.current_session, renamed, "Enter renamed the session");
        assert!(
            ghostwave::session::load(&renamed).is_some(),
            "and the renamed session is on disk"
        );

        execute(&mut s, Action::SessionManager);
        let entries = s.session_picker.as_ref().unwrap().entries.clone();
        assert!(entries.contains(&renamed), "the manager lists it: {entries:?}");
        s.session_picker = None;

        execute(&mut s, Action::SessionNew);
        let started = s.current_session.clone();
        assert!(started.starts_with("session-"), "a new session is named, got {started}");
        assert_eq!(toast(&s), format!("session: {started}"));
        assert_eq!(s.tabs.len(), 1, "a fresh workspace");
        assert_eq!(live_panes(&s), 1, "with one pane");
        assert!(
            ghostwave::session::load(&renamed).is_some(),
            "the session switched away from was saved"
        );

        execute(&mut s, Action::SessionDelete);
        assert_eq!(s.current_session, "default", "deleting opens the default session");
        assert_eq!(toast(&s), format!("deleted session: {started}"));
        assert!(
            ghostwave::session::load(&started).is_none(),
            "the deleted session is gone"
        );

        execute(&mut s, Action::Detach);
        assert!(s.quit, "detach saves and quits");
        assert_eq!(toast(&s), "session default saved — quitting");
        assert!(
            ghostwave::session::load("default").is_some(),
            "the session was written on the way out"
        );

        ghostwave::session::delete(&renamed);
        ghostwave::session::delete("default");
    }

    #[test]
    fn command_palette_runs_the_command_it_selects() {
        use crate::palette::{PaletteAction, PaletteMessage, PaletteState};

        let mut s = state();
        assert_eq!(s.tabs.len(), 1);

        execute(&mut s, Action::OpenCommandBar);
        assert!(s.palette.is_some(), "OpenCommandBar opens the palette");
        assert!(
            matches!(s.palette.as_ref(), Some(PaletteState { .. })),
            "the palette carries its state"
        );

        let palette = s.palette.as_mut().expect("the palette is open");
        palette.set_query("new tab");
        let index = palette
            .filtered_items()
            .iter()
            .position(|item| item.action == Action::NewTab)
            .expect("New Tab is still listed under the query");
        let action = match palette.update(PaletteMessage::ItemClicked(index)) {
            Some(PaletteAction::Execute(action)) => action,
            Some(PaletteAction::Close) => panic!("an item click must not close the palette"),
            None => panic!("an item click must yield an action"),
        };

        s.palette = None;
        execute(&mut s, action);
        assert_eq!(s.tabs.len(), 2, "New Tab from the palette opens a tab");
        assert!(s.palette.is_none(), "the palette is closed after executing");
    }

    #[test]
    fn the_scratchpad_float_is_visible_to_the_layer() {
        let mut s = state();
        execute(&mut s, Action::ToggleScratchpad);
        assert!(s.scratchpad.is_open(), "F12 opens the scratchpad");

        let floats: Vec<_> = s.scratchpad.visible_floats().collect();
        assert_eq!(floats.len(), 1, "the layer has one float to paint");
        let float = &floats[0];
        assert!(
            s.current().index_of(float.id()).is_some(),
            "the float's pane is in the tab, so the layer can build its canvas"
        );
        let size = float.size();
        assert!(
            size.width > 0.0 && size.height > 0.0,
            "the float has a rect to paint at"
        );

        execute(&mut s, Action::ToggleScratchpad);
        assert_eq!(
            s.scratchpad.visible_floats().count(),
            0,
            "hiding the scratchpad removes it from the layer"
        );
    }

    #[test]
    fn a_drag_leaves_a_selection_the_painter_can_read() {
        let mut s = state();
        let (tab, index) = (s.active_tab, 0);
        let pane = s.pane_mut(tab, index).expect("a pane");
        let (cell, padding) = (s.cell, s.config.padding);
        let rect = ghostwave::layout::Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 200.0,
        };
        let mods = crate::keys::current_mods();
        let left = crate::pane_canvas::vt_button(iced::mouse::Button::Left).expect("a real button");
        {
            let mut guard = pane.lock().unwrap();
            guard.ingest(b"alpha beta gamma\r\ndelta epsilon zeta\r\n");
            guard.mouse_event(
                rect,
                cell,
                padding,
                ghostwave::term::MouseEv {
                    kind: ghostwave::term::MouseEvKind::Press(left),
                    x: 20.0,
                    y: 10.0,
                    any_pressed: false,
                    button_down: None,
                },
                mods,
            );
            for step in 1..=5 {
                let d = step as f32;
                guard.mouse_event(
                    rect,
                    cell,
                    padding,
                    ghostwave::term::MouseEv {
                        kind: ghostwave::term::MouseEvKind::Motion,
                        x: 20.0 + d * 30.0,
                        y: 10.0 + d * 20.0,
                        any_pressed: true,
                        button_down: Some(left),
                    },
                    mods,
                );
            }
            guard.mouse_event(
                rect,
                cell,
                padding,
                ghostwave::term::MouseEv {
                    kind: ghostwave::term::MouseEvKind::Release(left),
                    x: 170.0,
                    y: 110.0,
                    any_pressed: true,
                    button_down: None,
                },
                mods,
            );
        }

        let mut guard = pane.lock().unwrap();
        let crate::pane::Pane { term, scratch, .. } = &mut *guard;
        let crate::pane::RenderScratch {
            render_state,
            row_it,
            ..
        } = scratch;
        let snapshot = render_state.update(&term.term).expect("a snapshot");
        let mut rows = row_it.update(&snapshot).expect("rows");
        let mut ranges = Vec::new();
        while let Some(row) = rows.next() {
            if let Some(sel) = row.selection().ok().flatten() {
                ranges.push((sel.start_x, sel.end_x));
            }
        }
        assert!(
            !ranges.is_empty(),
            "a drag left no row selection range for the painter: {ranges:?}"
        );
    }

    #[test]
    fn the_selection_range_survives_a_clean_pass() {
        let mut s = state();
        let (tab, index) = (s.active_tab, 0);
        let pane = s.pane_mut(tab, index).expect("a pane");
        let (cell, padding) = (s.cell, s.config.padding);
        let rect = ghostwave::layout::Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 200.0,
        };
        let mods = crate::keys::current_mods();
        let left = crate::pane_canvas::vt_button(iced::mouse::Button::Left).expect("a real button");
        {
            let mut guard = pane.lock().unwrap();
            guard.ingest(b"alpha beta gamma\r\ndelta epsilon zeta\r\n");
            guard.mouse_event(
                rect,
                cell,
                padding,
                ghostwave::term::MouseEv {
                    kind: ghostwave::term::MouseEvKind::Press(left),
                    x: 20.0,
                    y: 10.0,
                    any_pressed: false,
                    button_down: None,
                },
                mods,
            );
            guard.mouse_event(
                rect,
                cell,
                padding,
                ghostwave::term::MouseEv {
                    kind: ghostwave::term::MouseEvKind::Motion,
                    x: 180.0,
                    y: 110.0,
                    any_pressed: true,
                    button_down: Some(left),
                },
                mods,
            );
        }

        let mut guard = pane.lock().unwrap();
        let crate::pane::Pane { term, scratch, .. } = &mut *guard;
        let crate::pane::RenderScratch {
            render_state,
            row_it,
            ..
        } = scratch;

        let mut before = Vec::new();
        {
            let snapshot = render_state.update(&term.term).expect("a snapshot");
            let mut rows = row_it.update(&snapshot).expect("rows");
            while let Some(row) = rows.next() {
                if let Some(sel) = row.selection().ok().flatten() {
                    before.push((sel.start_x, sel.end_x));
                }
                let _ = row.set_dirty(false);
            }
            let _ = snapshot.set_dirty(libghostty_vt::render::Dirty::Clean);
        }

        let mut after = Vec::new();
        {
            let snapshot = render_state.update(&term.term).expect("a snapshot");
            let mut rows = row_it.update(&snapshot).expect("rows");
            while let Some(row) = rows.next() {
                if let Some(sel) = row.selection().ok().flatten() {
                    after.push((sel.start_x, sel.end_x));
                }
            }
        }

        assert!(!before.is_empty(), "no range before clearing: {before:?}");
        assert!(
            !after.is_empty(),
            "the selection range must survive the painter's clean pass: {after:?}"
        );
    }

    /// Drives a drag through the real mouse path in the pane at `index` and
    /// returns the selection ranges the painter would see, one per selected row.
    fn drag_and_read_selection(s: &mut AppState, index: usize) -> Vec<(u16, u16)> {
        let pane = s.pane_mut(s.active_tab, index).expect("a pane");
        let (cell, padding) = (s.cell, s.config.padding);
        let rect = ghostwave::layout::Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 200.0,
        };
        let mods = crate::keys::current_mods();
        let left = crate::pane_canvas::vt_button(iced::mouse::Button::Left).expect("a real button");
        {
            let mut guard = pane.lock().unwrap();
            guard.ingest(b"alpha beta gamma\r\ndelta epsilon zeta\r\n");
            guard.mouse_event(
                rect,
                cell,
                padding,
                ghostwave::term::MouseEv {
                    kind: ghostwave::term::MouseEvKind::Press(left),
                    x: 20.0,
                    y: 10.0,
                    any_pressed: false,
                    button_down: None,
                },
                mods,
            );
            for step in 1..=5 {
                let d = step as f32;
                guard.mouse_event(
                    rect,
                    cell,
                    padding,
                    ghostwave::term::MouseEv {
                        kind: ghostwave::term::MouseEvKind::Motion,
                        x: 20.0 + d * 20.0,
                        y: 10.0 + d * 20.0,
                        any_pressed: true,
                        button_down: Some(left),
                    },
                    mods,
                );
            }
            guard.mouse_event(
                rect,
                cell,
                padding,
                ghostwave::term::MouseEv {
                    kind: ghostwave::term::MouseEvKind::Release(left),
                    x: 120.0,
                    y: 110.0,
                    any_pressed: true,
                    button_down: None,
                },
                mods,
            );
        }

        let mut guard = pane.lock().unwrap();
        let crate::pane::Pane { term, scratch, .. } = &mut *guard;
        let crate::pane::RenderScratch {
            render_state,
            row_it,
            ..
        } = scratch;
        let snapshot = render_state.update(&term.term).expect("a snapshot");
        let mut rows = row_it.update(&snapshot).expect("rows");
        let mut ranges = Vec::new();
        while let Some(row) = rows.next() {
            if let Some(sel) = row.selection().ok().flatten() {
                ranges.push((sel.start_x, sel.end_x));
            }
        }
        ranges
    }

    #[test]
    fn an_existing_pane_still_selects_after_a_split() {
        let mut s = state();
        let existing = s.pane_mut(s.active_tab, 0).expect("a pane").lock().unwrap().id;

        let single = drag_and_read_selection(&mut s, 0);
        assert!(!single.is_empty(), "a single pane selects: {single:?}");

        execute(&mut s, Action::NewPaneRight);
        let live = s.current().panes.iter().filter(|p| p.is_some()).count();
        assert_eq!(live, 2, "the split added a second pane");

        let index = s.current().index_of(existing).expect("the pane is still there");
        let after = drag_and_read_selection(&mut s, index);
        assert!(
            !after.is_empty(),
            "the pane that was already there must still select after a split \
             (it is index {index} now): {after:?}"
        );
    }

    /// Drives a drag through the real mouse path in the pane at `index`.
    fn drag_in(s: &mut AppState, index: usize) {
        let pane = s.pane_mut(s.active_tab, index).expect("a pane");
        let (cell, padding) = (s.cell, s.config.padding);
        let rect = ghostwave::layout::Rect {
            x: 0.0,
            y: 0.0,
            w: 400.0,
            h: 200.0,
        };
        let mods = crate::keys::current_mods();
        let left = crate::pane_canvas::vt_button(iced::mouse::Button::Left).expect("a real button");
        let mut guard = pane.lock().unwrap();
        guard.mouse_event(
            rect,
            cell,
            padding,
            ghostwave::term::MouseEv {
                kind: ghostwave::term::MouseEvKind::Press(left),
                x: 20.0,
                y: 10.0,
                any_pressed: false,
                button_down: None,
            },
            mods,
        );
        for step in 1..=5 {
            let d = step as f32;
            guard.mouse_event(
                rect,
                cell,
                padding,
                ghostwave::term::MouseEv {
                    kind: ghostwave::term::MouseEvKind::Motion,
                    x: 20.0 + d * 20.0,
                    y: 10.0 + d * 20.0,
                    any_pressed: true,
                    button_down: Some(left),
                },
                mods,
            );
        }
        guard.mouse_event(
            rect,
            cell,
            padding,
            ghostwave::term::MouseEv {
                kind: ghostwave::term::MouseEvKind::Release(left),
                x: 120.0,
                y: 110.0,
                any_pressed: true,
                button_down: None,
            },
            mods,
        );
    }

    /// The selection ranges the painter would see for the pane at `index`.
    ///
    /// With `mark_clean` this also does what [`pane_canvas::paint_pane`] does at
    /// the end of a pass: mark every row clean and settle the snapshot.
    fn ranges_of(s: &mut AppState, index: usize, mark_clean: bool) -> Vec<(u16, u16)> {
        let pane = s.pane_mut(s.active_tab, index).expect("a pane");
        let mut guard = pane.lock().unwrap();
        let crate::pane::Pane { term, scratch, .. } = &mut *guard;
        let crate::pane::RenderScratch {
            render_state,
            row_it,
            ..
        } = scratch;
        let snapshot = render_state.update(&term.term).expect("a snapshot");
        let mut rows = row_it.update(&snapshot).expect("rows");
        let mut ranges = Vec::new();
        while let Some(row) = rows.next() {
            if let Some(sel) = row.selection().ok().flatten() {
                ranges.push((sel.start_x, sel.end_x));
            }
            if mark_clean {
                let _ = row.set_dirty(false);
            }
        }
        if mark_clean {
            let _ = snapshot.set_dirty(libghostty_vt::render::Dirty::Clean);
        }
        ranges
    }

    #[test]
    fn a_drag_in_a_pane_that_was_already_painted_still_selects() {
        let mut s = state();
        {
            let pane = s.pane_mut(s.active_tab, 0).expect("a pane");
            let mut guard = pane.lock().unwrap();
            guard.ingest(b"alpha beta gamma\r\ndelta epsilon zeta\r\n");
        }

        // A paint pass with nothing selected; the pane is idle from here on, which
        // is exactly the state a pane is in when the owner drags in it.
        let _ = ranges_of(&mut s, 0, true);

        drag_in(&mut s, 0);

        let after = ranges_of(&mut s, 0, false);
        assert!(
            !after.is_empty(),
            "a drag in an already-painted idle pane left the painter no selection range: {after:?}"
        );
    }

    #[test]
    fn each_grid_cell_holds_its_own_pane_after_a_split() {
        let mut s = state();
        let first = s.pane_mut(s.active_tab, 0).expect("a pane").lock().unwrap().id;
        execute(&mut s, Action::NewPaneRight);

        let tab = s.current();
        let mut live: Vec<usize> = tab.panes.iter().flatten().map(|e| e.id().0).collect();
        let mut cells: Vec<usize> = tab
            .grid
            .iter()
            .filter_map(|(_, cell)| cell.active_id())
            .map(|id| id.0)
            .collect();
        live.sort_unstable();
        cells.sort_unstable();
        assert_eq!(
            live, cells,
            "the grid's cells do not hold exactly the live panes"
        );
        let mut deduped = cells.clone();
        deduped.dedup();
        assert_eq!(
            deduped.len(),
            cells.len(),
            "one pane is painted by more than one cell"
        );
        assert!(
            tab.index_of(first).is_some(),
            "the pane that was already there is no longer in the tab"
        );
    }

    #[test]
    fn a_pane_you_drag_in_after_closing_and_splitting_is_the_one_that_gets_the_selection() {
        let mut s = state();
        // The owner's other sequence: close a pane, then split a new one in, then
        // drag in the pane that was still on screen.
        execute(&mut s, Action::NewPaneRight);
        let survivor = s
            .current()
            .panes
            .iter()
            .flatten()
            .map(|e| e.id())
            .next()
            .expect("a pane");
        execute(&mut s, Action::ClosePane);
        execute(&mut s, Action::NewPaneRight);

        let tab = s.current();
        let live: Vec<usize> = tab.panes.iter().flatten().map(|e| e.id().0).collect();
        let mut cells: Vec<usize> = tab
            .grid
            .iter()
            .filter_map(|(_, cell)| cell.active_id())
            .map(|id| id.0)
            .collect();
        cells.sort_unstable();
        let mut expect = live.clone();
        expect.sort_unstable();
        assert_eq!(
            cells, expect,
            "after a close and a split the cells no longer hold the live panes"
        );

        let index = s
            .current()
            .index_of(survivor)
            .expect("the surviving pane is still in the tab");
        let ranges = ranges_of(&mut s, index, false);
        let _ = ranges;
        drag_in(&mut s, index);
        let after = ranges_of(&mut s, index, false);
        assert!(
            !after.is_empty(),
            "the surviving pane (index {index}) got no selection from the drag: {after:?}"
        );
    }

    #[test]
    fn a_search_query_finds_text_the_pane_has_written() {
        let mut s = state();
        let active = s.current().active;
        let pane = s
            .pane_mut(s.active_tab, active)
            .expect("the tab has a focused pane");
        pane.lock().unwrap().ingest(b"hello hi there\r\n");

        open_search(&mut s);
        assert!(s.search.is_some(), "the overlay is open");
        search_push_char(&mut s, 'h');
        search_push_char(&mut s, 'i');

        let (active, matches) = {
            let guard = pane.lock().unwrap();
            (guard.term.search.active(), guard.term.search.matches.len())
        };
        assert!(active, "the query arms the pane's own search");
        assert!(
            matches > 0,
            "the query must reach the pane's match pass, found {matches}"
        );
    }

    #[test]
    fn search_overlay_owns_the_query_and_the_panes_search() {
        use ghostwave::search::Match;

        let mut s = state();
        execute(&mut s, Action::OpenSearch);
        assert!(s.search.is_some(), "OpenSearch opens the overlay");
        assert!(
            s.active_pane().unwrap().lock().unwrap().term.search.active,
            "the pane's search engine is armed"
        );

        search_push_char(&mut s, 'l');
        search_push_char(&mut s, 'i');
        assert_eq!(s.search.as_ref().unwrap().query, "li");
        assert_eq!(
            s.active_pane().unwrap().lock().unwrap().term.search.query,
            "li",
            "the typed query reaches the pane's search engine"
        );

        search_backspace(&mut s);
        assert_eq!(s.search.as_ref().unwrap().query, "l");
        assert_eq!(
            s.active_pane().unwrap().lock().unwrap().term.search.query,
            "l"
        );

        execute(&mut s, Action::SearchToggleCase);
        assert!(s.search.as_ref().unwrap().case_sensitive);
        assert!(
            s.active_pane().unwrap().lock().unwrap().term.search.case_sensitive,
            "the case toggle reaches the pane's search engine"
        );

        {
            let pane = s.active_pane().unwrap();
            let mut guard = pane.lock().unwrap();
            guard.term.search.matches = vec![
                Match {
                    row: 0,
                    col_start: 0,
                    col_end: 1,
                },
                Match {
                    row: 1,
                    col_start: 0,
                    col_end: 1,
                },
            ];
            guard.term.search.current = 0;
        }
        execute(&mut s, Action::SearchNext);
        assert_eq!(s.active_pane().unwrap().lock().unwrap().term.search.current, 1);
        assert_eq!(s.search.as_ref().unwrap().match_index, 1);
        execute(&mut s, Action::SearchPrev);
        assert_eq!(s.active_pane().unwrap().lock().unwrap().term.search.current, 0);
        assert_eq!(s.search.as_ref().unwrap().match_index, 0);

        close_search(&mut s);
        assert!(s.search.is_none(), "closing drops the overlay");
        assert!(
            !s.active_pane().unwrap().lock().unwrap().term.search.active,
            "closing disarms the pane's search engine"
        );
    }

    #[test]
    fn focus_stack_move_and_break_follow_the_grid() {
        let mut s = state();
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::R, 'r');
        let right = s.current().active;
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::D, 'd');
        assert_eq!(live_panes(&s), 3);

        ctrl(&mut s, Key::P);
        typed(&mut s, Key::H, 'h');
        assert_eq!(s.current().active, 0, "h focuses the pane on the left");
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::L, 'l');
        assert_eq!(s.current().active, right, "l focuses it back");

        let before = format!("{:?}", s.current().grid.layout());
        ctrl(&mut s, Key::H);
        assert_eq!(s.mode, Mode::Move);
        typed(&mut s, Key::L, 'l');
        assert_eq!(format!("{:?}", s.current().grid.layout()), before);
        typed(&mut s, Key::N, 'n');
        assert_ne!(format!("{:?}", s.current().grid.layout()), before);

        ctrl(&mut s, Key::P);
        typed(&mut s, Key::W, 'w');
        assert_eq!(toast(&s), "not in a stack (create with pane-mode s)");

        ctrl(&mut s, Key::P);
        typed(&mut s, Key::S, 's');
        let stacked = s.current().active;
        assert_eq!(live_panes(&s), 4);
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::W, 'w');
        assert_ne!(s.current().active, stacked, "w shows the stack's other pane");

        let cell = s.current().focused().expect("a focused cell");
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::X, 'x');
        let _ids: Vec<_> = s
            .current()
            .panes
            .iter()
            .map(|entry| entry.as_ref().map(|entry| entry.id()))
            .collect();
        assert_eq!(live_panes(&s), 3);
        {
            let tab = s.current();
            assert_eq!(tab.grid.len(), 3, "the cell survives its pane");
            let payload = tab.grid.get(cell).expect("the cell is still there");
            assert_eq!(payload.panes.len(), 1, "the cell kept its other pane");
            assert_eq!(
                payload.active_id().and_then(|id| tab.index_of(id)),
                Some(tab.active),
                "the focus follows the cell's remaining pane"
            );
        }

        let moved = s.current().pane(s.current().active).map(|p| p.lock().unwrap().id);
        ctrl(&mut s, Key::T);
        typed(&mut s, Key::B, 'b');
        assert_eq!(s.tabs.len(), 2, "break-pane opens a tab");
        assert_eq!(s.active_tab, 1);
        assert_eq!(live_panes(&s), 1, "the pane is alone in the new tab");
        assert_eq!(
            s.current().pane(s.current().active).map(|p| p.lock().unwrap().id),
            moved,
            "the pane keeps its identity across tabs"
        );
        assert_eq!(s.tabs[0].panes.iter().filter(|p| p.is_some()).count(), 2);

        ctrl(&mut s, Key::T);
        typed(&mut s, Key::B, 'b');
        assert_eq!(s.tabs.len(), 3);
        s.goto_tab(1);
        assert_eq!(live_panes(&s), 0);
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::D, 'd');
        assert_eq!(live_panes(&s), 1, "a split repopulates the emptied tab");
    }

    #[test]
    fn ctrl_q_quits_and_locked_keys_pass_through() {
        let mut s = state();
        ctrl(&mut s, Key::Q);
        assert!(s.quit);
    }


    /// The focused tab's live terminals; content panes are not counted.
    fn terms(state: &AppState) -> usize {
        state
            .current()
            .panes
            .iter()
            .flatten()
            .filter(|entry| entry.is_term())
            .count()
    }

    /// The focused content pane of the active tab.
    fn focused_content(state: &AppState) -> &ContentPane {
        state
            .current()
            .content(state.current().active)
            .expect("a content pane in the focused cell")
    }

    /// A path inside this front-end, for the preview tests.
    fn crate_path(relative: &str) -> String {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(relative)
            .display()
            .to_string()
    }

    /// The preview pane at tab index `index`.
    fn preview_at(state: &AppState, index: usize) -> &PreviewPane {
        state
            .current()
            .content(index)
            .and_then(ContentPane::as_preview)
            .expect("a preview pane")
    }

    /// The AI pane at tab index `index`.
    fn ai_at(state: &AppState, index: usize) -> &AiPane {
        state
            .current()
            .content(index)
            .and_then(ContentPane::as_ai)
            .expect("an AI pane")
    }

    #[test]
    fn new_ai_adds_a_content_cell_and_no_terminal() {
        let mut s = state();
        assert_eq!(terms(&s), 1);

        execute(&mut s, Action::NewAi);

        assert_eq!(live_panes(&s), 2, "the AI pane takes a pane slot");
        assert_eq!(terms(&s), 1, "no terminal was spawned for it");
        let tab = s.current();
        let index = tab.active;
        let content = tab.content(index).expect("the focused pane is the AI pane");
        assert_eq!(content.kind(), "ai");
        assert_eq!(content.label(), "ai");
        assert_eq!(
            tab.grid.len(),
            2,
            "the AI pane has a cell of its own in the grid"
        );
        let cell = tab.focused().expect("a focused cell");
        assert_eq!(
            tab.grid.get(cell).and_then(GridPane::active_id),
            Some(content.id()),
            "the cell's payload carries the content pane's own id"
        );
    }

    #[test]
    fn a_preview_entry_reports_its_kind() {
        let mut s = state();

        execute(&mut s, Action::OpenFile(crate_path("src")));
        let content = focused_content(&s);
        assert_eq!(
            content.as_preview().expect("a preview pane").kind,
            PreviewKind::Dir,
            "a directory previews as a directory"
        );
        assert_eq!(content.kind(), "preview");

        execute(&mut s, Action::OpenFile(crate_path("src/pane.rs")));
        let content = focused_content(&s);
        assert_eq!(
            content.as_preview().expect("a preview pane").kind,
            PreviewKind::Text,
            "a rust source previews as text"
        );
        assert_eq!(content.label(), "preview pane.rs");
    }

    #[test]
    fn ghostwave_adds_a_visualizer_entry() {
        let mut s = state();

        execute(&mut s, Action::Ghostwave);

        let content = focused_content(&s);
        assert!(
            content.as_visualizer().is_some(),
            "the entry is the visualizer pane"
        );
        assert_eq!(content.kind(), "visualizer");
        assert_eq!(content.label(), "ghostwave");
        assert_eq!(terms(&s), 1, "the visualizer spawns no terminal");
        assert_eq!(live_panes(&s), 2);
    }

    #[test]
    fn terminal_only_paths_skip_a_content_pane() {
        let mut s = state();
        execute(&mut s, Action::NewAi);
        let index = s.current().active;
        let id = focused_content(&s).id();

        assert!(
            s.current().pane(index).is_none(),
            "the terminal-only accessor returns None for a content pane"
        );
        assert!(s.active_pane().is_none(), "the focused pane is no terminal");
        assert!(
            s.pane_by_id(id).is_none(),
            "a content pane has no pty for output to route to"
        );
        assert_eq!(
            s.current().term_handles().len(),
            1,
            "the tab's terminal list holds the terminal alone"
        );

        s.refresh_titles();
        s.relayout();
        s.apply_terminal_colors();

        assert!(s.toggle_sync());
        press(&mut s, Key::Escape, Mods::empty());
        assert_eq!(
            ai_at(&s, index).input,
            "",
            "the broadcast reaches no content pane"
        );
    }

    #[test]
    fn closing_a_content_pane_removes_its_cell() {
        let mut s = state();
        execute(&mut s, Action::NewAi);
        let index = s.current().active;
        let id = focused_content(&s).id();
        assert_eq!(s.current().grid.len(), 2);

        execute(&mut s, Action::ClosePane);

        assert_eq!(
            s.current().grid.len(),
            1,
            "the content pane's cell collapses"
        );
        assert_eq!(
            s.current().panes.iter().flatten().count(),
            1,
            "the pane's slot is tombstoned"
        );
        assert!(s.current().index_of(id).is_none(), "the id is gone");
        assert!(s.current().content(index).is_none());
        assert_eq!(terms(&s), 1, "the terminal is untouched");
        assert!(
            s.current().focused().is_some(),
            "the grid still has a focused cell"
        );
        assert!(
            s.current().pane(s.current().active).is_some(),
            "the focus moved to the sibling terminal"
        );
    }

    #[test]
    fn preview_keys_scroll_zoom_and_reset() {
        let mut s = state();
        execute(&mut s, Action::OpenFile(crate_path("src/pane.rs")));
        let index = s.current().active;

        assert_eq!(preview_at(&s, index).scroll, 0);
        typed(&mut s, Key::J, 'j');
        typed(&mut s, Key::J, 'j');
        assert_eq!(
            preview_at(&s, index).scroll,
            2,
            "j scrolls the preview down a line"
        );
        typed(&mut s, Key::K, 'k');
        assert_eq!(preview_at(&s, index).scroll, 1, "k scrolls it back up");
        press(&mut s, Key::PageDown, Mods::empty());
        assert_eq!(
            preview_at(&s, index).scroll,
            1 + PREVIEW_PAGE,
            "the page key scrolls a page"
        );
        press(&mut s, Key::PageUp, Mods::empty());
        assert_eq!(preview_at(&s, index).scroll, 1, "and back up");

        typed(&mut s, Key::Equal, '+');
        let zoomed = preview_at(&s, index).zoom;
        assert!(zoomed > 1.0, "the zoom key magnifies, got {zoomed}");
        typed(&mut s, Key::Minus, '-');
        assert_eq!(
            preview_at(&s, index).zoom,
            1.0,
            "the zoom-out key returns to 1.0"
        );
        typed(&mut s, Key::Equal, '+');
        assert!(preview_at(&s, index).zoom > 1.0);
        typed(&mut s, Key::Digit0, '0');
        assert_eq!(preview_at(&s, index).zoom, 1.0, "0 resets the view");
        assert_eq!(
            preview_at(&s, index).scroll,
            0,
            "the reset clears the scroll too"
        );
    }

    #[test]
    fn typing_while_an_ai_pane_is_focused_appends_to_its_input() {
        let mut s = state();
        execute(&mut s, Action::NewAi);
        let index = s.current().active;

        typed(&mut s, Key::H, 'h');
        typed(&mut s, Key::I, 'i');
        assert_eq!(
            ai_at(&s, index).input,
            "hi",
            "the typed characters land in the prompt"
        );
        assert!(
            s.active_pane().is_none(),
            "the keys went to the content pane, not to a terminal"
        );

        press(&mut s, Key::Backspace, Mods::empty());
        assert_eq!(ai_at(&s, index).input, "h", "backspace edits the prompt");

        press(&mut s, Key::Backspace, Mods::empty());
        assert_eq!(ai_at(&s, index).input, "");
        press(&mut s, Key::Enter, Mods::empty());
        let ai = ai_at(&s, index);
        assert!(!ai.busy, "an empty prompt is not a request");
        assert_eq!(ai.history().len(), 1, "only the system prompt is in it");
    }

    // ---- remote/SSH panes ------------------------------------------------

    /// Host used by the remote tests: a local address whose ssh attempt fails
    /// instantly, so the background kills these tests start die at once.
    const TEST_HOST: &str = "127.0.0.1";

    /// Arms the workspace's ssh seam with a recorder, so the test can assert on
    /// the remote operations it asks for without dialing a host.
    fn record_ssh(s: &mut AppState) -> std::sync::Arc<crate::remote::testing::FakeSsh> {
        let ssh = std::sync::Arc::new(crate::remote::testing::FakeSsh::default());
        s.ssh = ssh.clone();
        ssh
    }

    /// The program the live pane at `(tab, pane)` runs, from its spawn metadata.
    fn pane_program(s: &AppState, tab: usize, pane: usize) -> Option<String> {
        let id = s.tabs[tab].pane(pane)?.lock().unwrap().id;
        crate::pane::with_pane_meta(id, |meta| meta.program.clone()).flatten()
    }

    /// Replaces the focused tab with one DISCONNECTED remote pane running
    /// `program`, and returns its handle.
    fn install_disconnected_remote(s: &mut AppState, program: &str, args: &[&str]) -> SharedPane {
        let pane = crate::pane::Pane::restore_remote(
            80,
            24,
            s.cell,
            s.config.scrollback,
            s.config.max_log_mb * 1024 * 1024,
            Some(program),
            args,
            None,
            "no-such-log",
            RemoteSpec {
                host: TEST_HOST.to_string(),
                session: crate::remote::remote_session(TEST_HOST),
            },
        )
        .expect("a disconnected remote pane is built without connecting");
        let id = pane.id;
        let handle = std::sync::Arc::new(std::sync::Mutex::new(pane));
        let tab = &mut s.tabs[s.active_tab];
        tab.panes = vec![Some(PaneEntry::Term(handle.clone()))];
        tab.grid = iced::widget::pane_grid::State::new(GridPane::single(id)).0;
        tab.active = 0;
        handle
    }

    #[test]
    fn enter_reconnects_a_disconnected_remote_pane() {
        let _guard = crate::ui_session::test_lock();
        let mut s = state();
        let handle = install_disconnected_remote(&mut s, "/bin/sh", &["-c", "true"]);
        assert!(s.remote_pane_disconnected(), "the pane starts disconnected");
        s.binary_ready.insert(TEST_HOST.to_string());

        press(&mut s, Key::Enter, Mods::empty());

        let guard = handle.lock().unwrap();
        assert!(guard.term.exited.is_none(), "Enter connected the pane");
        assert_eq!(guard.term.reconnect, ReconnectState::Idle);
        assert!(guard.term.child.is_some(), "its stored command runs again");
    }

    #[test]
    fn escape_stops_the_auto_reconnect_loop() {
        let _guard = crate::ui_session::test_lock();
        let mut s = state();
        let handle = install_disconnected_remote(&mut s, "/bin/sh", &["-c", "true"]);
        handle.lock().unwrap().term.reconnect = ReconnectState::Waiting {
            attempt: 2,
            next: std::time::Instant::now(),
        };

        press(&mut s, Key::Escape, Mods::empty());

        assert_eq!(
            handle.lock().unwrap().term.reconnect,
            ReconnectState::Manual,
            "Esc leaves the pane for the user to connect"
        );
        assert_eq!(toast(&s), "reconnect stopped — Enter to connect");
    }

    #[test]
    fn k_on_a_disconnected_remote_pane_kills_its_remote_session() {
        let _guard = crate::ui_session::test_lock();
        let mut s = state();
        let ssh = record_ssh(&mut s);
        let handle = install_disconnected_remote(&mut s, "/bin/sh", &["-c", "true"]);

        typed(&mut s, Key::K, 'k');

        assert_eq!(
            toast(&s),
            format!("killing remote session ghostwave-{TEST_HOST} on {TEST_HOST}")
        );
        assert!(
            handle.lock().unwrap().remote_disconnected(),
            "killing the remote session leaves the local pane as it was"
        );
        let calls = ssh.wait_calls(1, std::time::Duration::from_secs(5));
        assert_eq!(calls.len(), 1, "the kill is exactly one ssh command: {calls:?}");
        assert_eq!(calls[0].0, TEST_HOST, "it goes to the pane's host");
        assert_eq!(calls[0].2, 0, "the kill streams nothing on stdin");
        assert!(
            calls[0].1.contains(&format!("kill \"ghostwave-{TEST_HOST}\"")),
            "the daemon is killed by its own pidfile: {:?}",
            calls[0].1
        );
        let session = crate::remote::remote_session(TEST_HOST);
        assert!(
            calls[0]
                .1
                .contains(&format!("pkill -9 -f {}", crate::remote::sweep_pattern(&session))),
            "and the orphan sweep is anchored to that one session: {:?}",
            calls[0].1
        );
    }

    #[test]
    fn every_other_key_is_swallowed_by_a_disconnected_remote_pane() {
        let _guard = crate::ui_session::test_lock();
        let mut s = state();
        install_disconnected_remote(&mut s, "/bin/sh", &["-c", "true"]);
        assert_eq!(s.mode, Mode::Normal);

        ctrl(&mut s, Key::O);

        assert_eq!(
            s.mode,
            Mode::Normal,
            "the session-mode chord is not a bind while the pane is disconnected"
        );
    }

    #[test]
    fn the_durable_ssh_action_opens_its_own_session_and_binds_the_pane() {
        let _guard = crate::ui_session::test_lock();
        let mut s = state();
        let ssh = record_ssh(&mut s);

        execute(&mut s, Action::ConnectDurableSsh(TEST_HOST.to_string()));

        assert_eq!(s.current_session, format!("ssh-{TEST_HOST}"));
        assert_eq!(s.current().name, format!("ssh {TEST_HOST}"));
        let (tab, pane) = (s.active_tab, s.current().active);
        assert_eq!(
            s.remote_spec_of(tab, pane),
            Some((
                TEST_HOST.to_string(),
                crate::remote::remote_session(TEST_HOST)
            )),
            "the pane is bound to the host's ghostd session"
        );
        assert_eq!(toast(&s), format!("durable ssh: {TEST_HOST}"));
        assert_eq!(
            pane_program(&s, tab, pane).as_deref(),
            Some("/bin/sh"),
            "the pane runs the ssh seam's stand-in, so no test dials a host"
        );
        let calls = ssh.wait_calls(1, std::time::Duration::from_secs(5));
        assert_eq!(calls.len(), 1, "the upload checks the architecture once: {calls:?}");
        assert_eq!(calls[0].1, "uname -m");
    }

    #[test]
    fn the_plain_ssh_action_opens_its_session_without_a_ghostd_binding() {
        let _guard = crate::ui_session::test_lock();
        let mut s = state();
        let ssh = record_ssh(&mut s);

        execute(&mut s, Action::ConnectSsh(TEST_HOST.to_string()));

        // A session left behind by another test in this process is restored on
        // launch, so the action either starts this session or opens the pane
        // beside the restored one. The pane it opened is the focused one either
        // way, which is what the assertions below are about.
        let (tab, pane) = (s.active_tab, s.current().active);

        assert_eq!(s.current_session, format!("ssh-{TEST_HOST}"));
        assert_eq!(
            s.remote_spec_of(tab, pane),
            None,
            "a plain ssh pane is not durable"
        );
        assert_eq!(toast(&s), format!("connecting to {TEST_HOST}"));
        assert_eq!(
            pane_program(&s, tab, pane).as_deref(),
            Some("/bin/sh"),
            "the pane runs the ssh seam's stand-in, so no test dials a host"
        );
        assert!(
            ssh.calls().is_empty(),
            "a plain ssh pane never uploads anything: {:?}",
            ssh.calls()
        );
    }

    #[test]
    fn a_host_that_is_not_an_ssh_destination_opens_no_pane() {
        let _guard = crate::ui_session::test_lock();
        let mut s = state();
        let ssh = record_ssh(&mut s);
        let before = live_panes(&s);
        let session_before = s.current_session.clone();

        execute(&mut s, Action::ConnectDurableSsh("prod'; rm -rf ~; '".to_string()));

        assert_eq!(toast(&s), "not an ssh destination: prod'; rm -rf ~; '");
        assert_eq!(live_panes(&s), before, "no pane was opened for it");
        assert_eq!(s.current_session, session_before, "and no session was switched");
        assert!(ssh.calls().is_empty(), "and nothing was run");
    }

    #[test]
    fn killing_the_remote_session_of_a_pane_that_has_none_says_so() {
        let mut s = state();
        let before = live_panes(&s);

        execute(&mut s, Action::KillRemoteSession);

        assert_eq!(toast(&s), "no remote session in active pane");
        assert_eq!(live_panes(&s), before, "no pane was closed");
    }
}
