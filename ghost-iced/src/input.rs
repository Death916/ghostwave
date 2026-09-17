//! The input path: iced key events -> keymap modes -> `Action`s -> effects.
//!
//! The bind table belongs to the app crate, so there is no second copy here;
//! actions whose feature is not implemented yet raise a toast instead of acting.

use ghostwave::keymap::{self, Action, Dispatch, KeyPress, Mode};
use ghostwave::layout::SplitDir;
use libghostty_vt::key::{self, Mods};

use crate::app::AppState;
use crate::search_overlay::SearchState;

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

/// Hands a key to the terminal: the active pane, or every pane in the tab when
/// synchronised input is on.
fn send(state: &mut AppState, kp: &KeyPress, action: key::Action) {
    if state.current().sync_panes {
        let panes: Vec<_> = state.current().panes.iter().flatten().cloned().collect();
        for pane in panes {
            pane.lock().unwrap().key_press(kp, action);
        }
        return;
    }
    if let Some(pane) = state.active_pane() {
        pane.lock().unwrap().key_press(kp, action);
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

        Action::ScrollUp(n) => scroll(state, |p| p.scroll(-n)),
        Action::ScrollDown(n) => scroll(state, |p| p.scroll(n)),
        Action::PageUp => scroll(state, |p| p.scroll_page(-1)),
        Action::PageDown => scroll(state, |p| p.scroll_page(1)),
        Action::HalfPageUp => scroll(state, |p| p.scroll_half_page(-1)),
        Action::HalfPageDown => scroll(state, |p| p.scroll_half_page(1)),
        Action::ScrollToBottom => scroll(state, |p| p.scroll_to_bottom()),
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
        Action::Ghostwave => state.toast("visualizer pane: not implemented yet"),
        Action::NewAi => state.toast("AI pane: not implemented yet"),
        Action::OpenFile(_) => state.toast("preview panes: not implemented yet"),
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
    if let Some(pane) = pane {
        let mut guard = pane.lock().unwrap();
        guard.term.search.start();
        if !query.is_empty() {
            guard.term.search.set_query(query);
        }
        guard.term.search.case_sensitive = case_sensitive;
        guard.term.search.dirty = true;
        guard.term.force_repaint = true;
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

/// Pushes the overlay's query and case setting into the focused pane's search.
pub(crate) fn sync_search(state: &mut AppState) {
    let pane = state.active_pane();
    let Some(overlay) = state.search.as_ref() else {
        return;
    };
    if let Some(pane) = pane {
        let mut guard = pane.lock().unwrap();
        overlay.sync_to_term(&mut guard.term);
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

/// Flips the focused pane's whole-word search flag and reports its new value.
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

fn scroll(state: &mut AppState, f: impl FnOnce(&mut crate::pane::Pane)) {
    if let Some(pane) = state.active_pane() {
        f(&mut pane.lock().unwrap());
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
        // Pressing the mode's own key again leaves it.
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
        // A vertical split puts the new pane BELOW the focused one.
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
        // A modal action returns to Normal.
        assert_eq!(s.mode, Mode::Normal);

        ctrl(&mut s, Key::P);
        typed(&mut s, Key::R, 'r');
        assert_eq!(live_panes(&s), 3);
        assert_eq!(s.mode, Mode::Normal);

        // `x` closes the focused pane and keeps the mode exit.
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::X, 'x');
        assert_eq!(live_panes(&s), 2);

        // Esc leaves the mode without acting.
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
        press(&mut s, Key::Z, Mods::SHIFT); // shift+z = ToggleFrames
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

        // h/l walk the tabs.
        ctrl(&mut s, Key::T);
        typed(&mut s, Key::H, 'h');
        assert_eq!(s.active_tab, 0);
        ctrl(&mut s, Key::T);
        typed(&mut s, Key::L, 'l');
        assert_eq!(s.active_tab, 1);

        // 1-9 go straight to a tab, 1-based.
        ctrl(&mut s, Key::T);
        typed(&mut s, Key::Digit1, '1');
        assert_eq!(s.active_tab, 0);

        // Rename: the tab name wins over the OSC title once set.
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

        // `x` closes the second tab; one tab is never closed.
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
        // Ctrl+Shift+d is the "down" split, Ctrl+d the "right" one.
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

        // Ctrl+m toggles zoom on the focused pane.
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
        // Resize stays in the mode: its own binds keep it open.
        assert_ne!(format!("{:?}", s.current().grid.layout()), before);

        ctrl(&mut s, Key::H);
        typed(&mut s, Key::L, 'l');
        // Move mode swaps the focused pane with its neighbour: the cells trade
        // their panes, the count does not change.
        assert_eq!(live_panes(&s), 2);
    }

    #[test]
    fn scrolling_and_clear_reach_the_pane() {
        let mut s = state();
        // Scroll mode is sticky: the bind does not drop back to Normal.
        ctrl(&mut s, Key::S);
        assert_eq!(s.mode, Mode::Scroll);
        typed(&mut s, Key::K, 'k');
        typed(&mut s, Key::J, 'j');
        typed(&mut s, Key::D, 'd');
        typed(&mut s, Key::U, 'u');
        assert_eq!(s.mode, Mode::Scroll);
        press(&mut s, Key::Escape, Mods::empty());
        assert_eq!(s.mode, Mode::Normal);

        // Populate scrollback and verify Shift+PageUp / Shift+ArrowUp scroll viewport:
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

        // Ctrl+k clears the terminal; Ctrl+Shift+C has no selection to copy yet.
        ctrl(&mut s, Key::K);
        press(&mut s, Key::C, Mods::CTRL | Mods::SHIFT);
        assert_eq!(toast(&s), "nothing to copy");
    }

    #[test]
    fn locked_mode_swallows_binds_and_only_ctrl_g_unlocks() {
        let mut s = state();
        ctrl(&mut s, Key::G);
        assert_eq!(s.mode, Mode::Locked);
        // Mode entry and global binds are inert while locked.
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

        // Ctrl+Shift+p opens the command palette rather than toasting.
        assert!(s.palette.is_none());
        press(&mut s, Key::P, Mods::CTRL | Mods::SHIFT);
        assert!(s.palette.is_some(), "Ctrl+Shift+p opens the command palette");

        // Ctrl+Shift+f opens the search overlay.
        press(&mut s, Key::F, Mods::CTRL | Mods::SHIFT);
        assert!(s.search.is_some(), "Ctrl+Shift+f opens the search overlay");

        // F12 toggles the scratchpad terminal.
        assert!(!s.scratchpad.is_open());
        press(&mut s, Key::F12, Mods::empty());
        assert!(s.scratchpad.is_open(), "F12 opens the scratchpad");
        assert!(s.scratchpad.pane().is_some(), "the scratchpad pane exists");
        press(&mut s, Key::F12, Mods::empty());
        assert!(!s.scratchpad.is_open(), "F12 hides it again");

        // Floating panes have no key binding of their own yet.
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

        // The query a user types filters the inventory; the click index is an
        // index into the filtered list, as the palette's own view emits it.
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

        // `Message::Palette` clears the palette and then applies the action.
        s.palette = None;
        execute(&mut s, action);
        assert_eq!(s.tabs.len(), 2, "New Tab from the palette opens a tab");
        assert!(s.palette.is_none(), "the palette is closed after executing");
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

        // Typing goes to the overlay's query and from there into the pane.
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

        // The case toggle drives both sides.
        execute(&mut s, Action::SearchToggleCase);
        assert!(s.search.as_ref().unwrap().case_sensitive);
        assert!(
            s.active_pane().unwrap().lock().unwrap().term.search.case_sensitive,
            "the case toggle reaches the pane's search engine"
        );

        // Next/prev step the pane's engine and the overlay's own index together.
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
        // Three panes: the first split right, then the focused pane split down.
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::R, 'r');
        let right = s.current().active;
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::D, 'd');
        assert_eq!(live_panes(&s), 3);

        // Pane mode h/l move the focus between neighbouring cells.
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::H, 'h');
        assert_eq!(s.current().active, 0, "h focuses the pane on the left");
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::L, 'l');
        assert_eq!(s.current().active, right, "l focuses it back");

        // Move mode is sticky, and with nothing to the right it leaves the layout
        // alone; `n` then swaps the focused pane with the next cell in layout
        // order.
        let before = format!("{:?}", s.current().grid.layout());
        ctrl(&mut s, Key::H);
        assert_eq!(s.mode, Mode::Move);
        typed(&mut s, Key::L, 'l');
        assert_eq!(format!("{:?}", s.current().grid.layout()), before);
        typed(&mut s, Key::N, 'n');
        assert_ne!(format!("{:?}", s.current().grid.layout()), before);

        // A plain pane is not a stack.
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::W, 'w');
        assert_eq!(toast(&s), "not in a stack (create with pane-mode s)");

        // Stacking a pane into the focused cell and cycling shows the other member.
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::S, 's');
        let stacked = s.current().active;
        assert_eq!(live_panes(&s), 4);
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::W, 'w');
        assert_ne!(s.current().active, stacked, "w shows the stack's other pane");

        // Closing the shown member leaves the cell to the other one and moves the
        // focus with it.
        let cell = s.current().focused().expect("a focused cell");
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::X, 'x');
        let _ids: Vec<_> = s
            .current()
            .panes
            .iter()
            .map(|p| p.as_ref().map(|p| p.lock().unwrap().id))
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

        // Breaking the focused pane out of the grid moves it, identity and all,
        // into a tab of its own.
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

        // Breaking the only pane out of a tab leaves that tab empty; a split into
        // it starts it over.
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
}
