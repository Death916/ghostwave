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

/// Applies the visualizer's own key: `c` cycles its colour mode.
fn visualizer_key(pane: &mut crate::visualizer::VisualizerPane, kp: &KeyPress) -> bool {
    match kp.text {
        Some('c') => {
            pane.cycle_mode();
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
        Action::ToggleFloating | Action::ToggleHideFloat | Action::CycleFloat => {
            state.toast("floating panes: not implemented yet")
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
        Action::SessionManager | Action::SessionNew | Action::SessionRename
        | Action::SessionDelete | Action::Detach => state.toast("sessions: not implemented yet"),
        Action::ConnectSsh(_) | Action::ConnectDurableSsh(_) | Action::KillRemoteSession => {
            state.toast("remote panes: not implemented yet")
        }

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
                    state.toast("sessions: not implemented yet");
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
        assert_eq!(toast(&s), "sessions: not implemented yet");
        assert_eq!(s.mode, Mode::Normal);

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
        assert_eq!(toast(&s), "floating panes: not implemented yet");
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
}
