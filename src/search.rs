//! Terminal search: viewport-scoped substring search with highlight.
//! (Scrollback-wide search is a TODO — requires page iteration.)

use libghostty_vt::render::RowIteration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Match {
    pub row: u32,
    pub col_start: u16,
    pub col_end: u16,
}

#[derive(Debug)]
pub struct SearchState {
    pub query: String,
    pub active: bool,
    pub dirty: bool,
    pub matches: Vec<Match>,
    /// Index of the currently highlighted match.
    pub current: usize,
    pub case_sensitive: bool,
    pub wrap: bool,
    pub whole_word: bool,
}

impl Default for SearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            active: false,
            dirty: false,
            matches: Vec::new(),
            current: 0,
            case_sensitive: false,
            wrap: true,
            whole_word: false,
        }
    }
}

impl SearchState {
    pub fn active(&self) -> bool {
        self.active && !self.query.is_empty()
    }

    pub fn start(&mut self) {
        self.active = true;
        self.dirty = true;
        self.current = 0;
    }

    pub fn stop(&mut self) {
        self.active = false;
        self.query.clear();
        self.matches.clear();
        self.dirty = false;
    }

    pub fn set_query(&mut self, q: String) {
        self.query = q;
        self.dirty = true;
        self.current = 0;
    }

    pub fn next_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = (self.current + 1) % self.matches.len();
    }

    pub fn prev_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        self.current = if self.current == 0 {
            self.matches.len() - 1
        } else {
            self.current - 1
        };
    }

    pub fn contains(&self, row: u32, col: u16) -> bool {
        self.matches.iter().any(|m| {
            m.row == row && col >= m.col_start && col < m.col_end
        })
    }

    pub fn row_has_match(&self, row: u32) -> bool {
        self.matches.iter().any(|m| m.row == row)
    }

    /// Scan the current viewport rows for the query. Called while a render
    /// snapshot is active (row iterator borrows the snapshot).
    pub fn run_on_rows(&mut self, row_it: &mut RowIteration<'_, '_>) -> crate::Result<()> {
        self.matches.clear();
        if self.query.is_empty() {
            return Ok(());
        }
        let q = if self.case_sensitive {
            self.query.clone()
        } else {
            self.query.to_lowercase()
        };

        let mut row_idx: u32 = 0;
        let mut text = String::with_capacity(64);
        let mut cell_it = libghostty_vt::render::CellIterator::new()?;

        while let Some(row) = row_it.next() {
            let mut col = 0u16;
            text.clear();
            let mut cols_of: Vec<(u16, u16)> = Vec::new(); // (start_col, end_col) per char
            let mut it = cell_it.update(row)?;
            while let Some(cell) = it.next() {
                let start = text.len();
                cell.graphemes_utf8(&mut text)?;
                let end = text.len();
                if end > start {
                    cols_of.push((col, col + 1));
                }
                col += 1;
            }

            let hay = if self.case_sensitive {
                text.clone()
            } else {
                text.to_lowercase()
            };
            let mut from = 0usize;
            while let Some(rel) = hay[from..].find(&q) {
                let abs = from + rel;
                let end_abs = abs + q.len();
                if !self.whole_word || is_word_boundary(&text, abs, end_abs) {
                    let start_col = cols_of.get(abs).map(|(s, _)| *s).unwrap_or(0);
                    let end_col = cols_of
                        .get(end_abs.saturating_sub(1))
                        .map(|(_, e)| *e)
                        .unwrap_or(col);
                    self.matches.push(Match {
                        row: row_idx,
                        col_start: start_col,
                        col_end: end_col.max(start_col + 1),
                    });
                }
                from = end_abs.max(abs + 1);
            }
            row_idx += 1;
        }

        if self.current >= self.matches.len() {
            self.current = 0;
        }
        Ok(())
    }
}

fn is_word_boundary(text: &str, start: usize, end: usize) -> bool {
    let before = text[..start].chars().next_back();
    let after = text[end..].chars().next();
    let word = |c: Option<char>| c.map_or(false, |c| c.is_alphanumeric() || c == '_');
    !word(before) && !word(after)
}
