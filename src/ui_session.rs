//! Named sessions for the iced front-end: the workspace a session holds, and the
//! bookkeeping of which session the front-end is running inside.
//!
//! The format, the data dir, the pane logs and their pruning belong to the core
//! (`ghostwave::session`), and the layout a session stores is
//! `ghostwave::layout::Node` — the same files the macroquad front-end writes, so
//! a session saved by either front-end opens in the other. This module is only
//! the translation between a saved session and the port's tabs, through the grid
//! conversions in [`crate::grid_session`], plus the actions the session binds
//! perform.
//!
//! Three deliberate differences from the reference, so the parity table can name
//! them: sessions are not *parked* — switching saves the outgoing session and
//! drops its panes, closing their ptys, instead of keeping them alive in the
//! background; a pane saved with a durable remote binding is restored
//! DISCONNECTED (its history replays, no ssh runs until Enter connects it); and
//! the two-second autosave runs as a background subscription
//! ([`crate::autosave`]), so a session file is written periodically as well as on
//! a switch, a delete, a detach and on quit.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ghostwave::keymap::Mode;
use ghostwave::session::{self as core, PaneState, SessionState, TabState};
use iced::widget::pane_grid;

use crate::app::{AppState, GridPane, Result, Tab};
use crate::grid_session;
use crate::pane::{Pane, PaneEntry, PaneId};
use crate::sessions_overlay::{SessionsAction, SessionsMessage, SessionsOverlay};

/// The session the front-end starts inside when nothing has been saved.
pub const DEFAULT: &str = "default";

/// Grid a restored terminal is born with; its own bounds correct it before the
/// first paint, exactly as a fresh pane's do.
const RESTORE_COLS: u16 = 80;
const RESTORE_ROWS: u16 = 24;

/// The sorted names of the saved sessions.
pub fn names() -> Vec<String> {
    core::list_sessions()
}

/// The saved sessions that contain a durable remote pane, which the picker marks
/// with an `ssh` badge.
///
/// Read from the session files, as the reference did: the current session is
/// written every two seconds, so a connection made in this run shows up in the
/// list without the picker having to know about live panes.
pub fn remote_names() -> HashSet<String> {
    names()
        .into_iter()
        .filter(|name| core::has_remote(name))
        .collect()
}

/// Relists an open picker from disk, keeping its selection on where the user is.
pub fn refresh_picker(state: &mut AppState) {
    let names = names();
    let remotes = remote_names();
    if let Some(picker) = state.session_picker.as_mut() {
        picker.relist(names, remotes);
    }
}

/// The name a session started now takes: `session-1`, `session-2`, … .
pub fn next_name() -> String {
    let existing = core::list_sessions();
    let mut n = 1;
    loop {
        let name = format!("session-{n}");
        if !existing.iter().any(|saved| saved == &name) {
            return name;
        }
        n += 1;
    }
}

/// The on-disk state of the workspace: every tab, its terminal panes, and the
/// layout they sit in.
///
/// Content panes (a preview, an AI chat, the visualizer) have no pty and are not
/// saved; the cell one of them occupied keeps its position pointing at the tab's
/// first pane, which is the same fallback the reference's own remap uses. A
/// floating pane has no grid cell either, so it is not saved — sessions carry the
/// tiled layout only.
pub fn snapshot(state: &AppState) -> SessionState {
    SessionState {
        tabs: state.tabs.iter().map(tab_state).collect(),
    }
}

/// One tab's saved state: its terminals in tab order, addressed by their new
/// index, and its layout tree remapped onto them.
fn tab_state(tab: &Tab) -> TabState {
    let gridded: HashSet<PaneId> = tab
        .grid
        .iter()
        .flat_map(|(_, cell)| cell.panes.iter().copied())
        .collect();
    let mut ids: Vec<PaneId> = Vec::new();
    let mut panes: Vec<PaneState> = Vec::new();
    for entry in tab.panes.iter().flatten() {
        if entry.term().is_none() || !gridded.contains(&entry.id()) {
            continue;
        }
        // Spawn metadata comes from the registry the pane filled at construction,
        // never from the pane's own lock: that lock is shared with the pty path,
        // and a session save must not wait on it.
        let Some(state) = crate::pane::with_pane_meta(entry.id(), |m| PaneState {
            program: m.program.clone(),
            args: m.args.clone(),
            cwd: m.cwd.clone(),
            remote: m.remote.clone(),
            log: m.log.clone(),
        }) else {
            continue;
        };
        ids.push(entry.id());
        panes.push(state);
    }
    let position: HashMap<PaneId, usize> = ids
        .iter()
        .enumerate()
        .map(|(at, id)| (*id, at))
        .collect();
    let root = Some(grid_session::node_from_state(&tab.grid, |id| {
        position.get(&id).copied().unwrap_or(0)
    }));
    TabState {
        name: tab.name.clone(),
        panes,
        root,
        floating: Vec::new(),
    }
}

/// Persists the workspace under the session it runs inside, and remembers that
/// session as the last one used.
///
/// A workspace with no terminal pane at all is not written: a deleted or
/// never-populated session must not come back.
pub fn save(state: &AppState) {
    let sess = snapshot(state);
    if sess.tabs.iter().all(|tab| tab.panes.is_empty()) {
        return;
    }
    core::save(&state.current_session, &sess);
    core::set_last_session(&state.current_session);
}

/// Restores the session the front-end starts inside, when anything is saved
/// under that name. Called once at launch, on the state [`AppState::new`] built.
///
/// Nothing is written for the placeholder tab the state was born with: it is
/// replaced, not saved, so a launch cannot resurrect an empty session.
pub fn restore_last(state: &mut AppState) {
    let name = state.current_session.clone();
    if let Some(sess) = core::load(&name) {
        if let Err(e) = restore(state, &sess) {
            eprintln!("ghostwave: session {name} not restored: {e}");
        }
    }
}

/// Switches the workspace to the session saved under `name`, the outgoing one
/// written first. A name with nothing saved under it opens as a fresh session.
pub fn switch(state: &mut AppState, name: &str) {
    if name == state.current_session {
        state.toast(format!("already in session: {name}"));
        return;
    }
    save(state);
    open_saved(state, name);
}

/// Starts a brand-new session under `name`, the outgoing one written first.
pub fn start(state: &mut AppState, name: &str) {
    save(state);
    if let Err(e) = restore_fresh(state) {
        state.toast(format!("failed to start session: {e}"));
        return;
    }
    adopt(state, name);
}

/// Deletes the saved session `name`; the session the workspace runs inside is
/// left alone, so `false` means "switch away from it first".
pub fn delete(state: &mut AppState, name: &str) -> bool {
    if name == state.current_session {
        return false;
    }
    core::delete(name);
    true
}

/// Deletes the session the workspace runs inside and opens `default` in its
/// place, mirroring the reference's own delete action.
pub fn delete_current(state: &mut AppState) {
    let name = state.current_session.clone();
    if name == DEFAULT {
        state.toast("cannot delete the default session");
        return;
    }
    core::delete(&name);
    let saved = core::load(DEFAULT);
    let opened = match &saved {
        Some(sess) => restore(state, sess),
        None => restore_fresh(state),
    };
    if let Err(e) = opened {
        state.toast(format!("failed to open session {DEFAULT}: {e}"));
        return;
    }
    adopt(state, DEFAULT);
    state.toast(format!("deleted session: {name}"));
}

/// Renames the session the picker had selected — or the one the workspace runs
/// inside — preserving its panes and their logs.
pub fn rename(state: &mut AppState, new_name: &str) {
    let old = std::mem::take(&mut state.rename_target);
    let old = if old.is_empty() {
        state.current_session.clone()
    } else {
        old
    };
    if old == new_name || new_name.is_empty() {
        return;
    }
    core::rename(&old, new_name);
    if old == state.current_session {
        state.current_session = new_name.to_string();
        core::set_last_session(new_name);
        // The renamed session is the one this workspace runs inside, so write it
        // now: a rename of a session that was never saved would otherwise leave
        // nothing on disk under the new name.
        save(state);
    }
    state.toast(format!("session renamed: {new_name}"));
}

/// Opens the picker on the saved sessions, selecting the one the workspace runs
/// inside.
pub fn open_picker(state: &mut AppState) {
    let current = state.current_session.clone();
    state.session_picker = Some(SessionsOverlay::new(current, names(), remote_names()));
}

/// Applies a picker message: navigation changes only the picker, and the action a
/// message yields is performed on the workspace here.
pub fn picker_message(state: &mut AppState, message: SessionsMessage) {
    let action = state
        .session_picker
        .as_mut()
        .and_then(|picker| picker.update(message));
    let Some(action) = action else {
        return;
    };
    match action {
        SessionsAction::Close => state.session_picker = None,
        SessionsAction::Switch(name) => {
            state.session_picker = None;
            switch(state, &name);
        }
        SessionsAction::New => {
            state.session_picker = None;
            let name = next_name();
            start(state, &name);
        }
        SessionsAction::Rename(name) => {
            state.session_picker = None;
            state.mode = Mode::RenameSession;
            state.rename_buf = name.clone();
            state.rename_target = name;
        }
        SessionsAction::Delete(name) => {
            if delete(state, &name) {
                state.toast(format!("deleted session: {name}"));
                let refreshed = names();
                let remotes = remote_names();
                if let Some(picker) = state.session_picker.as_mut() {
                    picker.relist(refreshed, remotes);
                }
            } else {
                state.toast("cannot delete the current session");
            }
        }
        SessionsAction::KillRemotes(name) => state.kill_session_remotes(&name),
    }
}

/// Rebuilds the workspace from the session saved under `name`, when there is one,
/// and reports success by adopting it.
fn open_saved(state: &mut AppState, name: &str) {
    let opened = match core::load(name) {
        Some(sess) => restore(state, &sess),
        None => restore_fresh(state),
    };
    match opened {
        Ok(()) => adopt(state, name),
        Err(e) => state.toast(format!("failed to open session {name}: {e}")),
    }
}

/// Rebuilds the tabs of `sess`: a fresh shell per saved pane over its replayed
/// log, laid out by the tree the session stores.
fn restore(state: &mut AppState, sess: &SessionState) -> Result<()> {
    let mut tabs: Vec<Tab> = Vec::new();
    for tab in sess.tabs.iter().filter(|tab| !tab.panes.is_empty()) {
        if let Some(tab) = tab_from_state(state, tab) {
            tabs.push(tab);
        }
    }
    if tabs.is_empty() {
        tabs.push(state.fresh_tab()?);
    }
    state.replace_tabs(tabs);
    Ok(())
}

/// Replaces the workspace with a single fresh shell.
fn restore_fresh(state: &mut AppState) -> Result<()> {
    let tab = state.fresh_tab()?;
    state.replace_tabs(vec![tab]);
    Ok(())
}

/// One tab of a session: its terminals respawned over their replayed history, its
/// remote panes brought back disconnected, and the grid its layout tree converts
/// to. `None` when no pane could be built.
fn tab_from_state(state: &AppState, saved: &TabState) -> Option<Tab> {
    let cap = state.config.max_log_mb * 1024 * 1024;
    let mut ids: Vec<PaneId> = Vec::new();
    let mut entries: Vec<PaneEntry> = Vec::new();
    for pane in &saved.panes {
        let args: Vec<&str> = pane.args.iter().map(String::as_str).collect();
        let restored = match &pane.remote {
            Some(remote) => Pane::restore_remote(
                RESTORE_COLS,
                RESTORE_ROWS,
                state.cell,
                state.config.scrollback,
                cap,
                pane.program.as_deref(),
                &args,
                pane.cwd.as_deref(),
                &pane.log,
                remote.clone(),
            ),
            None => Pane::restore(
                RESTORE_COLS,
                RESTORE_ROWS,
                state.cell,
                state.config.scrollback,
                cap,
                pane.program.as_deref(),
                &args,
                pane.cwd.as_deref(),
                &pane.log,
            ),
        };
        match restored {
            Ok(mut pane) => {
                pane.apply_theme(&state.theme);
                ids.push(pane.id);
                entries.push(PaneEntry::Term(Arc::new(Mutex::new(pane))));
            }
            Err(e) => eprintln!("ghostwave: session pane not restored: {e}"),
        }
    }
    let first = *ids.first()?;
    let grid = match &saved.root {
        Some(root) => grid_session::state_from_node(root, |index| {
            ids.get(index).copied().unwrap_or(first)
        }),
        None => pane_grid::State::new(GridPane::single(first)).0,
    };
    Some(Tab::restored(saved.name.clone(), entries, grid))
}

/// Adopts `name` as the current session: the marker, the theme and the layout
/// follow, and the switch is reported.
fn adopt(state: &mut AppState, name: &str) {
    state.current_session = name.to_string();
    core::set_last_session(name);
    state.apply_terminal_colors();
    state.relayout();
    state.toast(format!("session: {name}"));
}

/// Serialises the tests that touch the shared data dir and the last-session
/// marker: a crate's tests run in parallel threads, and both are process-wide.
/// Taking it also isolates the data dir ([`isolate_data_dir`]).
#[cfg(test)]
pub(crate) fn test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    isolate_data_dir();
    guard
}

/// Points `XDG_DATA_HOME` at a per-process temp dir, once, before any test reads
/// a session.
#[cfg(test)]
fn isolate_data_dir() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("ghostwave-test-{}", std::process::id()));
        if std::fs::create_dir_all(&dir).is_err() {
            eprintln!("ghostwave: tests could not create {}", dir.display());
            return;
        }
        // SAFETY: set once, before this process performs any session call; every
        // test that touches the session dir takes `test_lock` first.
        unsafe { std::env::set_var("XDG_DATA_HOME", &dir) };
    });
}

#[cfg(test)]
mod tests {
    //! Sessions against the real core module and a real temp data dir (the suite
    //! runs with `XDG_DATA_HOME` pointing into `/tmp`): a workspace is saved,
    //! switched away from, and loaded back.

    use super::*;
    use crate::app::AppState;
    use ghostwave::config::Config;
    use ghostwave::layout::{Node, SplitDir};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A session name no other test (or run) shares.
    fn unique(prefix: &str) -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        format!("iced-{prefix}-{}-{n}", std::process::id())
    }

    fn state() -> AppState {
        let config = Config::load();
        let cell = crate::pane::cell_dims(crate::FONT_BYTES, config.font_size, config.row_gap);
        AppState::new(config, cell, iced::Size::new(1200.0, 800.0)).expect("spawn a pane")
    }

    fn live_panes(state: &AppState) -> usize {
        state.current().panes.iter().filter(|p| p.is_some()).count()
    }

    /// The tab's layout as the session format sees it, which is what a restore
    /// has to reproduce.
    fn saved_layout(state: &AppState) -> String {
        format!("{:?}", snapshot(state).tabs[0].root)
    }

    #[test]
    fn a_split_workspace_round_trips_through_a_saved_session() {
        let _guard = test_lock();
        let mut s = state();
        let name = unique("roundtrip");
        s.current_session = name.clone();
        s.split(SplitDir::Vertical).expect("split the focused pane");
        assert_eq!(live_panes(&s), 2, "the workspace has two panes");
        assert_eq!(s.current().grid.len(), 2, "in two grid cells");
        let tab_name = s.current().name.clone();
        let layout = saved_layout(&s);

        save(&s);
        let saved = core::load(&name).expect("the session file was written");
        assert_eq!(saved.tabs.len(), 1, "one tab");
        assert_eq!(saved.tabs[0].panes.len(), 2, "with both panes in it");
        assert_eq!(saved.tabs[0].name, tab_name);
        assert!(
            matches!(&saved.tabs[0].root, Some(Node::Split { .. })),
            "the split was saved, got {:?}",
            saved.tabs[0].root
        );
        assert!(
            saved.tabs[0].panes.iter().all(|pane| !pane.log.is_empty()),
            "every saved pane names its log"
        );

        let mut opened = state();
        let scratch = unique("scratch");
        opened.current_session = scratch.clone();
        switch(&mut opened, &name);

        assert_eq!(opened.current_session, name, "the switch adopted the session");
        assert_eq!(opened.tabs.len(), 1, "the workspace is the saved one");
        assert_eq!(opened.current().name, tab_name);
        assert_eq!(live_panes(&opened), 2, "both panes came back");
        assert_eq!(opened.current().grid.len(), 2, "as two cells");
        assert_eq!(
            saved_layout(&opened),
            layout,
            "the layout came back through the grid conversions"
        );
        for index in 0..2 {
            assert!(
                opened.current().pane(index).is_some(),
                "pane {index} is a live terminal"
            );
        }

        core::delete(&name);
        core::delete(&scratch);
    }

    #[test]
    fn the_last_session_is_restored_on_launch() {
        let _guard = test_lock();
        let mut s = state();
        let name = unique("launch");
        s.current_session = name.clone();
        s.split(SplitDir::Horizontal).expect("split the focused pane");
        save(&s);
        assert_eq!(core::last_session(), name, "the marker names the last session");
        let layout = saved_layout(&s);

        let mut launched = state();
        assert_eq!(
            launched.current_session, name,
            "a launch starts inside the last session"
        );
        restore_last(&mut launched);
        assert_eq!(launched.tabs.len(), 1);
        assert_eq!(live_panes(&launched), 2, "the saved panes came back");
        assert_eq!(saved_layout(&launched), layout);

        core::delete(&name);
    }

    #[test]
    fn a_switch_writes_the_outgoing_session_and_refuses_the_current_one() {
        let _guard = test_lock();
        let mut s = state();
        let outgoing = unique("outgoing");
        let incoming = unique("incoming");
        s.current_session = outgoing.clone();
        s.split(SplitDir::Vertical).expect("split");
        start(&mut s, &incoming);

        let written = core::load(&outgoing).expect("the outgoing session was saved on the way out");
        assert_eq!(written.tabs.len(), 1);
        assert_eq!(written.tabs[0].panes.len(), 2, "its panes were saved");

        switch(&mut s, &incoming);
        let expected = format!("already in session: {incoming}");
        assert_eq!(
            s.toast.as_ref().map(|(m, _)| m.as_str()),
            Some(expected.as_str()),
            "switching to the session already open is refused"
        );

        core::delete(&outgoing);
        core::delete(&incoming);
    }

    #[test]
    fn a_remote_pane_comes_back_disconnected_and_its_session_is_badged_ssh() {
        let _guard = test_lock();
        let mut s = state();
        let name = unique("remote");
        s.current_session = name.clone();
        let saved = SessionState {
            tabs: vec![TabState {
                name: "ssh prod".to_string(),
                panes: vec![PaneState {
                    program: Some("/bin/sh".to_string()),
                    args: vec!["-c".to_string(), "true".to_string()],
                    cwd: None,
                    remote: Some(ghostwave::term::RemoteSpec {
                        host: "prod".to_string(),
                        session: "ghostwave-prod".to_string(),
                    }),
                    log: "no-such-log".to_string(),
                }],
                root: None,
                floating: vec![],
            }],
        };
        core::save(&name, &saved);

        assert!(
            remote_names().contains(&name),
            "a session holding a durable pane is badged ssh"
        );
        assert!(names().contains(&name) || remote_names().contains(&name));

        let mut opened = state();
        let scratch = unique("scratch");
        opened.current_session = scratch.clone();
        switch(&mut opened, &name);

        assert_eq!(opened.current_session, name);
        let handle = opened
            .current()
            .pane(0)
            .cloned()
            .expect("the remote pane came back");
        let guard = handle.lock().unwrap();
        assert!(guard.remote_disconnected(), "it restores disconnected");
        assert_eq!(
            guard.term.reconnect,
            ghostwave::term::ReconnectState::Manual,
            "it waits for Enter rather than connecting itself"
        );
        assert!(guard.term.child.is_none(), "no ssh is run at restore");
        assert_eq!(
            guard.remote_spec(),
            Some(("prod".to_string(), "ghostwave-prod".to_string()))
        );
        drop(guard);

        core::delete(&name);
        core::delete(&scratch);
    }

    #[test]
    fn delete_refuses_the_current_session_and_removes_a_saved_one() {
        let _guard = test_lock();
        let mut s = state();
        let current = unique("current");
        let other = unique("other");
        s.current_session = current.clone();
        save(&s);
        core::save(&other, &snapshot(&s));
        assert!(names().contains(&other));

        assert!(!delete(&mut s, &current), "the current session is not deleted");
        assert!(core::load(&current).is_some(), "and its file stays");

        assert!(delete(&mut s, &other), "a saved session is deleted");
        assert!(core::load(&other).is_none(), "and its file is gone");

        core::delete(&current);
    }
}
