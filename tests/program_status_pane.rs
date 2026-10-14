//! OSC 7501 through a real pane: a program emits, the pane's own path
//! (`TermPane::ingest_bytes`, the same one `poll_pty` uses) scans, and the grid
//! renders exactly the bytes the child wrote.
//!
//! Where the unit tests in `src/program_status.rs` pin the protocol, these pin
//! the plumbing: a real pty, a real child process, and the surviving records.

use std::time::{Duration, Instant};

use ghostwave::program_status::{State, Table, QUERY_REPLY};
use ghostwave::term::{CellDims, TermPane};
use libghostty_vt::render::{CellIterator, RenderState, RowIterator};

/// A pane running `script` under `/bin/sh -c`, 80x24.
fn pane_running(script: &str) -> TermPane {
    let cell = CellDims {
        w: 8.0,
        h: 16.0,
        baseline: 12.0,
    };
    TermPane::new(
        80,
        24,
        cell,
        Some("/bin/sh"),
        &["-c", script],
        None,
        1000,
        0,
        true,
        None,
    )
    .expect("spawn the pane's shell")
}

/// Drains the pane until its child exits or `deadline` passes, returning every
/// distinct `dominant` state the pane showed while it ran.
fn drain(pane: &mut TermPane, deadline: Duration) -> Vec<State> {
    let start = Instant::now();
    let mut seen: Vec<State> = Vec::new();
    while start.elapsed() < deadline {
        pane.poll_pty();
        if let Some(record) = pane.program_status().dominant() {
            if seen.last() != Some(&record.state) {
                seen.push(record.state);
            }
        }
        if pane.exited.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // Whatever arrived with the child's exit still belongs to this run.
    while pane.poll_pty() {}
    seen
}

/// The pane's grid, rows trimmed of trailing blanks.
fn rows(pane: &TermPane) -> Vec<String> {
    let mut state = RenderState::new().unwrap();
    let snapshot = state.update(&pane.term).unwrap();
    let mut row_it = RowIterator::new().unwrap();
    let mut it = row_it.update(&snapshot).unwrap();
    let mut cell_it = CellIterator::new().unwrap();
    let mut out = Vec::new();
    while let Some(row) = it.next() {
        let mut line = String::new();
        let mut cit = cell_it.update(row).unwrap();
        while let Some(cell) = cit.next() {
            let mut graphemes = String::new();
            cell.graphemes_utf8(&mut graphemes).unwrap();
            line.push_str(&graphemes);
        }
        out.push(line.trim_end().to_string());
    }
    out
}

#[test]
fn a_program_status_sequence_renders_nothing() {
    // The spec's own shell function, plus a report with every key, between two
    // visible lines: the grid must show the two lines and nothing else.
    let script = r#"
status() { printf '\033]7501;state=%s:msg=%s\033\\' "$1" "$(printf '%s' "$2" | base64 | tr -d '\n')"; }
printf 'before\n'
status blocked "Apply 3 to add, 1 to change, 0 to destroy?"
printf '\033]7501;state=working:id=us-east:title=VVMgRWFzdA==:progress=40:app=deploy:msg=UHVzaGluZyBpbWFnZQ==\033\\'
printf 'after\n'
"#;
    let mut pane = pane_running(script);
    drain(&mut pane, Duration::from_secs(10));

    let rows = rows(&pane);
    let content: Vec<&String> = rows.iter().filter(|row| !row.is_empty()).collect();
    assert_eq!(
        content.len(),
        2,
        "only the two visible lines: {rows:?}"
    );
    assert_eq!(content[0], "before");
    assert_eq!(content[1], "after");

    // And the reports did reach the record table.
    let table = pane.program_status();
    assert_eq!(table.get("us-east").unwrap().progress, Some(40));
    assert_eq!(table.get("us-east").unwrap().title.as_deref(), Some("US East"));
    assert_eq!(table.get("us-east").unwrap().app.as_deref(), Some("deploy"));
}

#[test]
fn the_spec_shell_function_drives_a_real_state_change() {
    let script = r#"
status() { printf '\033]7501;state=%s:msg=%s\033\\' "$1" "$(printf '%s' "$2" | base64 | tr -d '\n')"; }
status working "Syncing photos"
sleep 0.5
status done "Photos synced"
"#;
    let mut pane = pane_running(script);
    let seen = drain(&mut pane, Duration::from_secs(10));

    assert!(
        seen.contains(&State::Working) && seen.contains(&State::Done),
        "the pane should show working and then done, saw {seen:?}"
    );
    // `done` outlives the process that reported it.
    let record = pane
        .program_status()
        .get(Table::ROOT)
        .expect("root record survives the exit");
    assert_eq!(record.state, State::Done);
    assert_eq!(record.msg.as_deref(), Some("Photos synced"));
}

#[test]
fn a_child_exit_drops_working_but_keeps_done() {
    let script = r#"
status() { printf '\033]7501;state=%s:msg=%s\033\\' "$1" "$(printf '%s' "$2" | base64 | tr -d '\n')"; }
status working:id=photos:progress=40 "Uploading"
status done:id=photos "Uploaded"
status working:id=leftover "Still going"
"#;
    let mut pane = pane_running(script);
    drain(&mut pane, Duration::from_secs(10));

    let table = pane.program_status();
    assert!(
        table.get("leftover").is_none(),
        "the process is gone, so its working record is too"
    );
    assert_eq!(table.get("photos").unwrap().state, State::Done);
    // A report replaces its record whole: `progress` was not in the last one.
    assert_eq!(table.get("photos").unwrap().progress, None);
}

#[test]
fn a_query_is_answered_over_the_pty_with_the_constant_reply() {
    let reply_path = std::env::temp_dir().join(format!(
        "ghostwave-osc7501-reply-{}.bin",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&reply_path);
    // The child takes its tty out of cooked mode so the reply arrives whole,
    // then keeps what it was sent: the pane answers a program, not the screen.
    let script = format!(
        r#"
stty raw -echo 2>/dev/null
printf '\033]7501;?\033\\'
timeout 1 cat > '{path}'
stty -raw echo
"#,
        path = reply_path.display()
    );
    let mut pane = pane_running(&script);
    drain(&mut pane, Duration::from_secs(10));

    let got = std::fs::read(&reply_path).expect("the child wrote what it was sent");
    let _ = std::fs::remove_file(&reply_path);
    assert_eq!(
        got, QUERY_REPLY,
        "the pane's answer is the constant reply, byte for byte"
    );
    let text = String::from_utf8_lossy(&got);
    assert!(!text.contains("state="), "no report content in the reply");
    assert_eq!(pane.program_status().len(), 0, "a query leaves no record");
}

#[test]
fn a_report_split_over_pty_reads_is_still_found() {
    // `dd` hands the sequence over in one-byte reads, so the pane's own reads
    // land wherever the scheduler happens to cut them.
    let script = r#"
printf '\033]7501;state=blocked:kind=permission:app=terraform:msg=QXBwbHkgMyB0byBhZGQsIDEgdG8gY2hhbmdlLCAwIHRvIGRlc3Ryb3k/\033\\' | dd bs=1 2>/dev/null
"#;
    let mut pane = pane_running(script);
    drain(&mut pane, Duration::from_secs(10));

    let record = pane
        .program_status()
        .get(Table::ROOT)
        .expect("the sequence was found however it was cut");
    assert_eq!(record.state, State::Blocked);
    assert_eq!(record.app.as_deref(), Some("terraform"));
    assert_eq!(
        record.msg.as_deref(),
        Some("Apply 3 to add, 1 to change, 0 to destroy?")
    );
}
