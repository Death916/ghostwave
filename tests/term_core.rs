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
    let rows = rows_of(&term);
    // 137 chars at 50 cols = 50 + 50 + 37 (three rows).
    assert_eq!(rows[0].len(), 50, "row 0 should be 50 wide, got {}", rows[0].len());
    assert_eq!(rows[1].len(), 50, "row 1 should be 50 wide");
    assert_eq!(rows[2].len(), 37, "row 2 should be 37 wide");
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
    let before = rows_of(&term);
    assert_eq!(before[0].len(), 100);
    assert_eq!(before[1].len(), 100);

    // Shrink to 50 cols: the already-written line must reflow to 4 rows.
    term.resize(50, 10, 10, 26).unwrap();
    let after = rows_of(&term);
    assert_eq!(after[0].len(), 50, "reflow: row 0");
    assert_eq!(after[1].len(), 50, "reflow: row 1");
    assert_eq!(after[2].len(), 50, "reflow: row 2");
    assert_eq!(after[3].len(), 50, "reflow: row 3");
}
