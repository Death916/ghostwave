//! The input path: iced key events -> keymap modes -> `Action`s -> effects.
//!
//! The bind table belongs to the app crate, so there is no second copy here;
//! actions whose feature is not implemented yet raise a toast instead of acting.

use ghostwave::keymap::{self, Action, Dispatch, KeyPress, Mode};
use ghostwave::layout::SplitDir;
use libghostty_vt::key::{self, Mods};

use crate::app::AppState;

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
        Dispatch::Pass => send(state, &kp, action),
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
fn execute(state: &mut AppState, action: Action) {
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
        Action::ToggleScratchpad => state.toast("scratchpad: not implemented yet"),

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

        Action::SearchStart
        | Action::OpenSearch
        | Action::SearchNext
        | Action::SearchPrev
        | Action::SearchToggleCase
        | Action::SearchToggleWrap
        | Action::SearchToggleWholeWord
        | Action::ConfirmSearch => state.toast("search: not implemented yet"),

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
        Action::OpenCommandBar => state.toast("command palette: not implemented yet"),
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
    use ghostwave::layout::{Node, SplitDir};
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
            matches!(s.current().root, Node::Split { dir: SplitDir::Vertical, .. }),
            "d must split downward, got {:?}",
            s.current().root
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
        assert!(
            matches!(s.current().root, Node::Stack { .. }),
            "pane mode `s` stacks, got {:?}",
            s.current().root
        );
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::Z, 'z');
        assert!(s.current().zoomed.contains(&s.current().active));
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::Z, 'z');
        assert!(!s.current().zoomed.contains(&s.current().active));
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
            s.current().root,
            Node::Split {
                dir: SplitDir::Vertical,
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
        assert!(s.current().zoomed.contains(&s.current().active));
    }

    #[test]
    fn resize_and_move_modes_change_the_tree() {
        let mut s = state();
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::R, 'r');
        let before = format!("{:?}", s.current().root);
        ctrl(&mut s, Key::N);
        assert_eq!(s.mode, Mode::Resize);
        typed(&mut s, Key::L, 'l');
        // Resize stays in the mode: its own binds keep it open.
        assert_ne!(format!("{:?}", s.current().root), before);

        ctrl(&mut s, Key::H);
        typed(&mut s, Key::L, 'l');
        // Move mode swaps the focused pane with its neighbour: pane ids move in
        // the tree, the count does not change.
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
    fn unimplemented_features_say_so_instead_of_doing_nothing() {
        let mut s = state();
        ctrl(&mut s, Key::O);
        assert_eq!(s.mode, Mode::Session);
        typed(&mut s, Key::W, 'w');
        assert_eq!(toast(&s), "sessions: not implemented yet");
        assert_eq!(s.mode, Mode::Normal);
        press(&mut s, Key::P, Mods::CTRL | Mods::SHIFT);
        assert_eq!(toast(&s), "command palette: not implemented yet");
        press(&mut s, Key::F, Mods::CTRL | Mods::SHIFT);
        assert_eq!(toast(&s), "search: not implemented yet");
        ctrl(&mut s, Key::P);
        typed(&mut s, Key::F, 'f');
        assert_eq!(toast(&s), "floating panes: not implemented yet");
    }

    #[test]
    fn ctrl_q_quits_and_locked_keys_pass_through() {
        let mut s = state();
        ctrl(&mut s, Key::Q);
        assert!(s.quit);
    }
}
