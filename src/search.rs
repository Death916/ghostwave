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

    #[inline]
    pub fn contains(&self, row: u32, col: u16) -> bool {
        if self.matches.is_empty() {
            return false;
        }
        self.matches.iter().any(|m| {
            m.row == row && col >= m.col_start && col < m.col_end
        })
    }

    #[inline]
    pub fn row_has_match(&self, row: u32) -> bool {
        if self.matches.is_empty() {
            return false;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_word_boundary() {
        // positive: word at end, start, solo, surrounded by non-word chars
        assert!(is_word_boundary("hello world", 6, 11), "world at end");
        assert!(is_word_boundary("foo bar", 0, 3), "foo at start");
        assert!(is_word_boundary("foo", 0, 3), "solo word");
        assert!(is_word_boundary("foo,bar", 0, 3), "followed by comma (non-word)");
        assert!(is_word_boundary("foo-bar", 4, 7), "after hyphen");
        // negative: substring inside a word or joined by word char
        assert!(!is_word_boundary("helloworld", 5, 10), "substring inside word");
        assert!(!is_word_boundary("foo_bar", 0, 3), "underscore is word char, so foo is not a word");
        assert!(!is_word_boundary("afoo", 1, 4), "preceded by word char");
        assert!(!is_word_boundary("foobar", 0, 3), "followed by word char");
        assert!(!is_word_boundary("foo_bar", 4, 7), "bar inside foo_bar");
    }

    #[test]
    fn search_state_lifecycle() {
        let mut s = SearchState::default();
        assert!(!s.active(), "default should be inactive");
        assert!(!s.active);
        s.start();
        assert!(s.active, "start sets flag");
        assert!(s.dirty, "start marks dirty");
        assert!(!s.active(), "active() still false when query is empty — probes the &&");
        s.set_query("hi".into());
        assert_eq!(s.query, "hi");
        assert!(s.active(), "now truly active");
        assert!(s.dirty);
        assert_eq!(s.current, 0);
        s.matches.push(Match { row: 0, col_start: 0, col_end: 2 });
        s.stop();
        assert!(!s.active);
        assert!(s.query.is_empty(), "stop clears query");
        assert!(s.matches.is_empty(), "stop clears matches");
        assert!(!s.dirty);
        assert!(!s.active(), "inactive after stop");
    }

    #[test]
    fn next_prev_wraps() {
        let mut s = SearchState::default();
        s.matches = vec![
            Match { row: 0, col_start: 0, col_end: 2 },
            Match { row: 0, col_start: 5, col_end: 7 },
            Match { row: 1, col_start: 0, col_end: 2 },
        ];
        assert_eq!(s.current, 0);
        s.next_match();
        assert_eq!(s.current, 1);
        s.next_match();
        assert_eq!(s.current, 2);
        s.next_match();
        assert_eq!(s.current, 0, "wraps around");
        s.prev_match();
        assert_eq!(s.current, 2, "prev from 0 wraps to last");
        s.prev_match();
        assert_eq!(s.current, 1);
    }

    #[test]
    fn next_prev_empty_is_noop() {
        let mut s = SearchState::default();
        s.next_match();
        s.prev_match();
        assert_eq!(s.current, 0);
    }

    #[test]
    fn contains_and_row_has_match() {
        let mut s = SearchState::default();
        s.matches = vec![
            Match { row: 2, col_start: 3, col_end: 7 },
            Match { row: 5, col_start: 0, col_end: 4 },
        ];
        assert!(s.contains(2, 3));
        assert!(s.contains(2, 6));
        assert!(!s.contains(2, 7), "end is exclusive");
        assert!(!s.contains(2, 2));
        assert!(!s.contains(3, 4));
        assert!(s.contains(5, 0));
        assert!(s.row_has_match(2));
        assert!(s.row_has_match(5));
        assert!(!s.row_has_match(3));
    }

    #[test]
    fn set_query_resets_current() {
        let mut s = SearchState::default();
        s.matches = vec![Match { row: 0, col_start: 0, col_end: 2 }];
        s.current = 1;
        s.set_query("new".into());
        assert_eq!(s.current, 0);
        assert!(s.dirty);
    }
}
