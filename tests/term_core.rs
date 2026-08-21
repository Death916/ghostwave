//! Terminal core behavior tests (no UI): wrap, reflow, scrollback.
//! These pin down libghostty-vt 0.2.1 semantics that the UI depends on.

use libghostty_vt::{
    render::{CellIterator, RenderState, RowIterator},
    Terminal, TerminalOptions,
};

fn rows_of(term: &Terminal<'static, 'static>) -> Vec<String> {
    let mut state = RenderState::new().unwrap();
    let snapshot = state.update(term).unwrap();
    let mut row_it = RowIterator::new().unwrap();
    let mut it = row_it.update(&snapshot).unwrap();
    let mut out = Vec::new();
    let mut cell_it = CellIterator::new().unwrap();
    let mut s = String::new();
    while let Some(row) = it.next() {
        s.clear();
        let mut cit = cell_it.update(row).unwrap();
        while let Some(c) = cit.next() {
            let mut g = String::new();
            c.graphemes_utf8(&mut g).unwrap();
            s.push_str(&g);
        }
        out.push(s.clone());
    }
    out
}

/// libghostty rows expose only written cells (no trailing-space padding).
/// trimmed_rows is the canonical content view.
fn trimmed_rows(term: &Terminal<'static, 'static>) -> Vec<String> {
    rows_of(term).into_iter().map(|s| s.trim_end().to_string()).collect()
}

#[test]
fn long_line_wraps_at_cols() {
    let mut term = Terminal::new(TerminalOptions {
        cols: 50,
        rows: 10,
        max_scrollback: 1000,
    })
    .unwrap();
    let long: String = "A".repeat(137) + "\n";
    term.vt_write(long.as_bytes());
    // libghostty rows expose only written cells (no trailing-space padding),
    // so the last wrapped fragment is short — 50 + 50 + 37 in content.
    let rows = trimmed_rows(&term);
    assert_eq!(rows[0].len(), 50, "row 0 should be 50 wide, got {}", rows[0].len());
    assert_eq!(rows[1].len(), 50, "row 1 should be 50 wide");
    assert_eq!(rows[2].len(), 37, "row 2 should be 37 wide");
    assert_eq!(rows[0], "A".repeat(50));
    assert_eq!(rows[1], "A".repeat(50));
    assert_eq!(rows[2], "A".repeat(37));
    let total_a: usize = rows.iter().take(3).map(|s| s.len()).sum();
    assert_eq!(total_a, 137);
}

#[test]
fn resize_reflows_wrapped_lines() {
    let mut term = Terminal::new(TerminalOptions {
        cols: 100,
        rows: 10,
        max_scrollback: 1000,
    })
    .unwrap();
    let long: String = "B".repeat(200) + "\n";
    term.vt_write(long.as_bytes());
    let before = trimmed_rows(&term);
    assert_eq!(before[0].len(), 100);
    assert_eq!(before[1].len(), 100);
    assert_eq!(before[0], "B".repeat(100));
    assert_eq!(before[1], "B".repeat(100));

    // Shrink to 50 cols: the already-written line must reflow to 4 rows.
    term.resize(50, 10, 10, 26).unwrap();
    let after = trimmed_rows(&term);
    assert_eq!(after[0].len(), 50, "reflow: row 0");
    assert_eq!(after[1].len(), 50, "reflow: row 1");
    assert_eq!(after[2].len(), 50, "reflow: row 2");
    assert_eq!(after[3].len(), 50, "reflow: row 3");
    for r in after.iter().take(4) {
        assert_eq!(r, &"B".repeat(50));
    }
}

#[test]
fn exact_cols_line_does_not_create_extra_row() {
    let mut term = Terminal::new(TerminalOptions { cols: 10, rows: 5, max_scrollback: 10 }).unwrap();
    term.vt_write(b"0123456789"); // exactly cols, no newline
    let rows = trimmed_rows(&term);
    // The 10 chars fill row 0; row 1 is still empty (cursor wrapped but no extra content row).
    assert_eq!(rows[0], "0123456789");
    assert_eq!(rows[1], "", "next row should be empty, got {:?}", rows[1]);
}

#[test]
fn short_line_is_not_padded_in_trimmed_view() {
    let mut term = Terminal::new(TerminalOptions { cols: 20, rows: 5, max_scrollback: 10 }).unwrap();
    term.vt_write(b"hi\n");
    // libghostty only exposes written cells per row — no trailing-space padding
    let rows = rows_of(&term);
    assert_eq!(rows[0], "hi");
    let trimmed = trimmed_rows(&term);
    assert_eq!(trimmed[0], "hi");
}

#[test]
fn scrollback_retains_after_resize() {
    // Fill many lines to exercise scrollback, then resize and check content survives.
    let mut term = Terminal::new(TerminalOptions { cols: 20, rows: 5, max_scrollback: 100 }).unwrap();
    for i in 0..20 {
        let line = format!("line-{i:02}\n");
        term.vt_write(line.as_bytes());
    }
    // Before resize, at least some trimmed rows should contain the written lines.
    let before = trimmed_rows(&term);
    // The viewport shows the last 5 lines; earlier ones are in scrollback but not in viewport rows.
    // At minimum the visible window should have content.
    assert!(before.iter().any(|r| r.contains("line-")), "viewport should show written lines");

    term.resize(10, 5, 10, 26).unwrap();
    let after = trimmed_rows(&term);
    // After narrowing, each "line-XX" (7 chars) still fits in 10 cols, so viewport still has content.
    assert!(after.iter().any(|r| r.contains("line-")), "content should survive resize");
}

#[test]
fn content_preserved_through_widen_and_narrow() {
    let mut term = Terminal::new(TerminalOptions { cols: 10, rows: 10, max_scrollback: 100 }).unwrap();
    let payload = "X".repeat(45); // 10+10+10+10+5
    term.vt_write(payload.as_bytes());
    term.vt_write(b"\n");
    let narrow = trimmed_rows(&term);
    let total_narrow: String = narrow.iter().take(5).cloned().collect();
    assert_eq!(total_narrow, payload, "narrow view should reassemble payload");

    term.resize(20, 10, 10, 26).unwrap();
    let wide = trimmed_rows(&term);
    // At 20 cols: 20+20+5
    assert_eq!(wide[0].len(), 20);
    assert_eq!(wide[1].len(), 20);
    assert_eq!(wide[2].len(), 5);
    let total_wide: String = wide.iter().take(3).cloned().collect();
    assert_eq!(total_wide, payload, "wide view should reassemble same payload");
}
