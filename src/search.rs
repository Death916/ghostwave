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
        if self.current + 1 < self.matches.len() {
            self.current += 1;
        } else if self.wrap {
            self.current = 0;
        }
    }

    pub fn prev_match(&mut self) {
        if self.matches.is_empty() {
            return;
        }
        if self.current > 0 {
            self.current -= 1;
        } else if self.wrap {
            self.current = self.matches.len() - 1;
        }
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
        let q_chars: Vec<char> = self.query.chars().collect();
        if q_chars.is_empty() {
            return Ok(());
        }

        let mut row_idx: u32 = 0;
        let mut cell_text = String::with_capacity(16);
        let mut cell_it = libghostty_vt::render::CellIterator::new()?;

        while let Some(row) = row_it.next() {
            let mut col = 0u16;
            let mut chars: Vec<(char, u16)> = Vec::with_capacity(128);
            let mut it = cell_it.update(row)?;
            while let Some(cell) = it.next() {
                cell_text.clear();
                cell.graphemes_utf8(&mut cell_text)?;
                for c in cell_text.chars() {
                    chars.push((c, col));
                }
                col += 1;
            }

            if chars.len() >= q_chars.len() {
                let mut i = 0;
                while i + q_chars.len() <= chars.len() {
                    let matches = chars[i..i + q_chars.len()]
                        .iter()
                        .zip(&q_chars)
                        .all(|(&(c, _), &qc)| {
                            if self.case_sensitive {
                                c == qc
                            } else {
                                c == qc || c.to_lowercase().eq(qc.to_lowercase())
                            }
                        });

                    if matches {
                        let is_word = if self.whole_word {
                            let before = if i > 0 { Some(chars[i - 1].0) } else { None };
                            let after = if i + q_chars.len() < chars.len() {
                                Some(chars[i + q_chars.len()].0)
                            } else {
                                None
                            };
                            let word = |c: Option<char>| c.map_or(false, |ch| ch.is_alphanumeric() || ch == '_');
                            !word(before) && !word(after)
                        } else {
                            true
                        };

                        if is_word {
                            let start_col = chars[i].1;
                            let end_col = chars[i + q_chars.len() - 1].1 + 1;
                            self.matches.push(Match {
                                row: row_idx,
                                col_start: start_col,
                                col_end: end_col.max(start_col + 1),
                            });
                            i += q_chars.len();
                            continue;
                        }
                    }
                    i += 1;
                }
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
    fn next_prev_stop_at_the_ends_without_wrap() {
        let mut s = SearchState::default();
        s.wrap = false;
        s.matches = vec![
            Match {
                row: 0,
                col_start: 0,
                col_end: 2,
            },
            Match {
                row: 0,
                col_start: 5,
                col_end: 7,
            },
        ];
        s.next_match();
        assert_eq!(s.current, 1);
        s.next_match();
        assert_eq!(s.current, 1, "the last match is the end without wrap");
        s.prev_match();
        assert_eq!(s.current, 0);
        s.prev_match();
        assert_eq!(s.current, 0, "the first match is the end without wrap");
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
