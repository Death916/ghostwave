//! Mouse selection and clipboard integration for the iced terminal front-end.
//!
//! Tracks terminal selection gestures (normal character selection, double-click word
//! expansion, triple-click whole-line selection), coordinate translations between
//! pixel space and terminal grid cells, text extraction across rows with wide character
//! and multi-line handling, and native Wayland/X11 clipboard operations via arboard.

use ghostwave::term::CellDims;

/// The selection mode determining how selection endpoints expand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SelectionMode {
    /// Character-by-character linear selection.
    #[default]
    Normal,
    /// Selection expanded to word boundaries (triggered by double-click).
    Word,
    /// Selection expanded to entire rows (triggered by triple-click).
    Line,
}

/// A single cell representation for grid text extraction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridCell {
    /// Character content of the cell (or grapheme cluster).
    pub text: String,
    /// Width of the character in columns (1 for normal, 2 for wide lead, 0 for wide continuation).
    pub width: u8,
}

impl GridCell {
    /// Creates a cell with a single character and specified column width.
    pub fn new(c: char, width: u8) -> Self {
        Self {
            text: c.to_string(),
            width,
        }
    }

    /// Creates an empty cell (single whitespace with width 1).
    pub fn empty() -> Self {
        Self {
            text: " ".to_string(),
            width: 1,
        }
    }

    /// Creates a lead cell for a wide character occupying 2 columns.
    pub fn wide(c: char) -> Self {
        Self {
            text: c.to_string(),
            width: 2,
        }
    }

    /// Creates a continuation cell for a preceding wide character (width 0).
    pub fn wide_continuation() -> Self {
        Self {
            text: String::new(),
            width: 0,
        }
    }
}

/// Tracks mouse selection state, endpoints, and modes within a terminal pane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionModel {
    /// Starting grid coordinate `(col, row)`.
    pub start: (usize, usize),
    /// Current ending grid coordinate `(col, row)`.
    pub end: (usize, usize),
    /// Current selection mode (`Normal`, `Word`, `Line`).
    pub mode: SelectionMode,
    /// Whether an active drag gesture is currently in progress.
    pub is_dragging: bool,
    /// Initial origin coordinate `(col, row)` where the click began before expansion.
    pub origin: (usize, usize),
    /// Whether the selection is active.
    pub active: bool,
}

impl Default for SelectionModel {
    fn default() -> Self {
        Self::new()
    }
}

impl SelectionModel {
    /// Creates a new inactive, empty selection model.
    pub const fn new() -> Self {
        Self {
            start: (0, 0),
            end: (0, 0),
            mode: SelectionMode::Normal,
            is_dragging: false,
            origin: (0, 0),
            active: false,
        }
    }

    /// Returns `true` if no text is currently selected.
    pub fn is_empty(&self) -> bool {
        if !self.active {
            return true;
        }
        match self.mode {
            SelectionMode::Normal => self.start == self.end && !self.is_dragging,
            _ => false,
        }
    }

    /// Clears the selection and resets dragging state.
    pub fn clear(&mut self) {
        self.start = (0, 0);
        self.end = (0, 0);
        self.mode = SelectionMode::Normal;
        self.is_dragging = false;
        self.origin = (0, 0);
        self.active = false;
    }

    /// Returns the normalized endpoints `(start, end)` in visual reading order,
    /// where `start <= end`.
    pub fn normalized(&self) -> ((usize, usize), (usize, usize)) {
        if self.start.1 < self.end.1 || (self.start.1 == self.end.1 && self.start.0 <= self.end.0) {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    /// Returns `true` if the cell at `(col, row)` falls inside the normalized selection.
    pub fn contains(&self, col: usize, row: usize) -> bool {
        if self.is_empty() {
            return false;
        }

        let ((s_col, s_row), (e_col, e_row)) = self.normalized();

        if row < s_row || row > e_row {
            return false;
        }

        if s_row == e_row {
            col >= s_col && col <= e_col
        } else if row == s_row {
            col >= s_col
        } else if row == e_row {
            col <= e_col
        } else {
            true
        }
    }

    /// Starts a selection on mouse press at `pos` with the specified mode.
    pub fn on_press(&mut self, pos: (usize, usize), mode: SelectionMode, max_cols: usize) {
        self.origin = pos;
        self.mode = mode;
        self.is_dragging = true;
        self.active = true;

        match mode {
            SelectionMode::Normal => {
                self.start = pos;
                self.end = pos;
            }
            SelectionMode::Word => {
                self.start = pos;
                self.end = pos;
            }
            SelectionMode::Line => {
                self.start = (0, pos.1);
                self.end = (max_cols.saturating_sub(1), pos.1);
            }
        }
    }

    /// Starts a word selection on double-click at `pos` using characters from `line`.
    pub fn on_press_word(&mut self, pos: (usize, usize), line: &[char]) {
        self.origin = pos;
        self.mode = SelectionMode::Word;
        self.is_dragging = true;
        self.active = true;

        let (w_start, w_end) = expand_word_range(line, pos.0);
        self.start = (w_start, pos.1);
        self.end = (w_end, pos.1);
    }

    /// Starts a line selection on triple-click at `pos` expanding to row boundaries.
    pub fn on_press_line(&mut self, pos: (usize, usize), max_cols: usize) {
        self.origin = pos;
        self.mode = SelectionMode::Line;
        self.is_dragging = true;
        self.active = true;

        self.start = (0, pos.1);
        self.end = (max_cols.saturating_sub(1), pos.1);
    }

    /// Updates the selection endpoint as the mouse is dragged to `pos`.
    pub fn on_drag(&mut self, pos: (usize, usize), max_cols: usize) {
        if !self.is_dragging {
            return;
        }

        match self.mode {
            SelectionMode::Normal => {
                self.end = pos;
            }
            SelectionMode::Word => {
                self.end = pos;
            }
            SelectionMode::Line => {
                let min_row = self.origin.1.min(pos.1);
                let max_row = self.origin.1.max(pos.1);
                self.start = (0, min_row);
                self.end = (max_cols.saturating_sub(1), max_row);
            }
        }
    }

    /// Updates word selection drag with the word boundary at `pos` on `line`.
    pub fn on_drag_word(&mut self, pos: (usize, usize), line: &[char]) {
        if !self.is_dragging || self.mode != SelectionMode::Word {
            return;
        }

        let (w_start, w_end) = expand_word_range(line, pos.0);

        if pos.1 == self.origin.1 {
            if pos.0 >= self.origin.0 {
                self.end = (w_end, pos.1);
            } else {
                self.start = (w_start, pos.1);
            }
        } else if pos.1 > self.origin.1 {
            self.end = (w_end, pos.1);
        } else {
            self.start = (w_start, pos.1);
        }
    }

    /// Finishes the drag gesture on mouse release.
    pub fn on_release(&mut self) {
        self.is_dragging = false;
        if self.mode == SelectionMode::Normal && self.start == self.end {
            self.active = false;
        }
    }

    /// Extracts selected text across rows from a 2D grid of [`GridCell`]s.
    ///
    /// Respects:
    /// - Normalized endpoints `(start_col, start_row)` to `(end_col, end_row)`.
    /// - Wide characters: appends text once and skips continuation cells with width 0.
    /// - Trailing spaces: trims trailing whitespace on extracted lines.
    /// - Multi-line breaks: separates lines with `\n`.
    /// - Empty selection: returns an empty string.
    pub fn extract_text(&self, grid: &[Vec<GridCell>]) -> String {
        if self.is_empty() || grid.is_empty() {
            return String::new();
        }

        let ((start_col, start_row), (end_col, end_row)) = self.normalized();
        if start_row >= grid.len() {
            return String::new();
        }
        let max_row = end_row.min(grid.len() - 1);

        let mut lines = Vec::with_capacity(max_row - start_row + 1);

        for r in start_row..=max_row {
            let cells = &grid[r];
            if cells.is_empty() {
                lines.push(String::new());
                continue;
            }

            let col_start = if r == start_row {
                start_col.min(cells.len().saturating_sub(1))
            } else {
                0
            };

            let col_end = if r == end_row {
                end_col.min(cells.len().saturating_sub(1))
            } else {
                cells.len().saturating_sub(1)
            };

            if col_start > col_end {
                lines.push(String::new());
                continue;
            }

            let mut line_buf = String::new();
            for col in col_start..=col_end {
                let cell = &cells[col];
                if cell.width == 0 {
                    continue;
                }
                line_buf.push_str(&cell.text);
            }

            let trimmed = line_buf.trim_end().to_string();
            lines.push(trimmed);
        }

        let mut result = lines.join("\n");
        if self.mode == SelectionMode::Line && !result.is_empty() {
            result.push('\n');
        }
        result
    }

    /// Extracts selected text from a slice of row strings.
    pub fn extract_text_from_lines(&self, lines: &[&str]) -> String {
        if self.is_empty() || lines.is_empty() {
            return String::new();
        }

        let ((start_col, start_row), (end_col, end_row)) = self.normalized();
        if start_row >= lines.len() {
            return String::new();
        }
        let max_row = end_row.min(lines.len() - 1);

        let mut extracted_lines = Vec::with_capacity(max_row - start_row + 1);

        for r in start_row..=max_row {
            let line_chars: Vec<char> = lines[r].chars().collect();
            if line_chars.is_empty() {
                extracted_lines.push(String::new());
                continue;
            }

            let col_start = if r == start_row {
                start_col.min(line_chars.len().saturating_sub(1))
            } else {
                0
            };

            let col_end = if r == end_row {
                end_col.min(line_chars.len().saturating_sub(1))
            } else {
                line_chars.len().saturating_sub(1)
            };

            if col_start > col_end {
                extracted_lines.push(String::new());
                continue;
            }

            let slice: String = line_chars[col_start..=col_end].iter().collect();
            extracted_lines.push(slice.trim_end().to_string());
        }

        let mut result = extracted_lines.join("\n");
        if self.mode == SelectionMode::Line && !result.is_empty() {
            result.push('\n');
        }
        result
    }

    /// Extracts the selection text from `grid` and copies it to the clipboard.
    pub fn copy_selection(&self, grid: &[Vec<GridCell>]) -> bool {
        let text = self.extract_text(grid);
        if text.is_empty() {
            return false;
        }
        copy_to_clipboard(&text)
    }
}

/// Returns `true` if `c` is considered a word boundary delimiter.
///
/// Whitespace and punctuation characters are treated as boundaries, while
/// alphanumeric characters and underscores (`_`) form words.
pub fn is_word_boundary(c: char) -> bool {
    !c.is_alphanumeric() && c != '_'
}

/// Returns `true` if `c` is a constituent word character (alphanumeric or underscore).
pub fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Expands a column index to the enclosing word's start and end columns (inclusive).
///
/// If `col` lands on a word character, the range covers contiguous word characters.
/// If `col` lands on whitespace, it expands across contiguous whitespace.
/// Otherwise, it expands across contiguous punctuation characters.
pub fn expand_word_range(chars: &[char], col: usize) -> (usize, usize) {
    if chars.is_empty() {
        return (0, 0);
    }
    let col = col.min(chars.len() - 1);
    let ch = chars[col];

    #[derive(PartialEq, Eq)]
    enum Category {
        Word,
        Whitespace,
        Punctuation,
    }

    let char_cat = |c: char| {
        if c.is_whitespace() {
            Category::Whitespace
        } else if is_word_char(c) {
            Category::Word
        } else {
            Category::Punctuation
        }
    };

    let cat = char_cat(ch);

    let mut start = col;
    while start > 0 && char_cat(chars[start - 1]) == cat {
        start -= 1;
    }

    let mut end = col;
    while end + 1 < chars.len() && char_cat(chars[end + 1]) == cat {
        end += 1;
    }

    (start, end)
}

/// Expands a column index to word boundaries on a string slice.
pub fn expand_word_boundary_str(line: &str, col: usize) -> (usize, usize) {
    let chars: Vec<char> = line.chars().collect();
    expand_word_range(&chars, col)
}

/// Converts pixel coordinates `(x, y)` within pane bounds to terminal cell grid coordinates `(col, row)`.
pub fn pixel_to_cell(
    x: f32,
    y: f32,
    cell: CellDims,
    padding: f32,
    cols: usize,
    rows: usize,
) -> (usize, usize) {
    let cw = (cell.w as f32).max(1.0);
    let ch = (cell.h as f32).max(1.0);
    let max_col = cols.saturating_sub(1);
    let max_row = rows.saturating_sub(1);

    let col = if x < padding {
        0
    } else {
        ((x - padding) / cw).floor().max(0.0) as usize
    };
    let row = if y < padding {
        0
    } else {
        ((y - padding) / ch).floor().max(0.0) as usize
    };

    (col.min(max_col), row.min(max_row))
}

/// Converts a grid coordinate `(col, row)` to top-left pixel coordinates relative to pane bounds.
pub fn cell_to_pixel(
    col: usize,
    row: usize,
    cell: CellDims,
    padding: f32,
) -> (f32, f32) {
    (
        padding + col as f32 * cell.w as f32,
        padding + row as f32 * cell.h as f32,
    )
}

/// Copies the given text to the system clipboard using [`arboard::Clipboard`].
///
/// On Wayland and X11, this updates both the system clipboard and the primary selection buffer,
/// with CLI fallback tools if the in-process clipboard fails.
pub fn copy_to_clipboard(text: &str) -> bool {
    ghostwave::clipboard::write_text(text);
    ghostwave::clipboard::write_primary_text(text)
}

/// Retrieves the current text contents of the system clipboard using [`arboard::Clipboard`].
///
/// Returns `None` if the clipboard cannot be read or is empty.
pub fn paste_from_clipboard() -> Option<String> {
    ghostwave::clipboard::read_text()
}

/// Retrieves the current text contents of the primary selection clipboard on Linux.
pub fn paste_primary_from_clipboard() -> Option<String> {
    ghostwave::clipboard::read_primary_text()
}

/// Convenience function to extract selected text from a cell grid.
pub fn extract_selection_text(model: &SelectionModel, grid: &[Vec<GridCell>]) -> String {
    model.extract_text(grid)
}

/// Convenience function to extract selected text from lines of strings.
pub fn extract_selection_from_lines(model: &SelectionModel, lines: &[&str]) -> String {
    model.extract_text_from_lines(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_coordinate_normalization() {
        let mut model = SelectionModel::new();

        model.start = (2, 0);
        model.end = (5, 0);
        assert_eq!(model.normalized(), ((2, 0), (5, 0)));

        model.start = (5, 0);
        model.end = (2, 0);
        assert_eq!(model.normalized(), ((2, 0), (5, 0)));

        model.start = (10, 1);
        model.end = (2, 3);
        assert_eq!(model.normalized(), ((10, 1), (2, 3)));

        model.start = (2, 3);
        model.end = (10, 1);
        assert_eq!(model.normalized(), ((10, 1), (2, 3)));
    }

    #[test]
    fn test_pixel_to_cell_conversions() {
        let cell = CellDims {
            w: 8,
            h: 16,
            baseline: 12.0,
        };
        let padding = 4.0;
        let cols = 80;
        let rows = 24;

        assert_eq!(pixel_to_cell(2.0, 2.0, cell, padding, cols, rows), (0, 0));
        assert_eq!(pixel_to_cell(0.0, 0.0, cell, padding, cols, rows), (0, 0));

        assert_eq!(pixel_to_cell(20.0, 36.0, cell, padding, cols, rows), (2, 2));

        assert_eq!(
            pixel_to_cell(10000.0, 10000.0, cell, padding, cols, rows),
            (79, 23)
        );

        let (px, py) = cell_to_pixel(5, 3, cell, padding);
        assert_eq!(px, 4.0 + 5.0 * 8.0);
        assert_eq!(py, 4.0 + 3.0 * 16.0);
        assert_eq!(pixel_to_cell(px, py, cell, padding, cols, rows), (5, 3));
    }

    #[test]
    fn test_word_boundary_detection() {
        assert!(is_word_boundary(' '));
        assert!(is_word_boundary('\t'));
        assert!(is_word_boundary('\n'));
        assert!(is_word_boundary('.'));
        assert!(is_word_boundary(','));
        assert!(is_word_boundary(':'));
        assert!(is_word_boundary(';'));
        assert!(is_word_boundary('/'));
        assert!(is_word_boundary('-'));
        assert!(is_word_boundary('('));
        assert!(is_word_boundary(')'));

        assert!(!is_word_boundary('a'));
        assert!(!is_word_boundary('Z'));
        assert!(!is_word_boundary('0'));
        assert!(!is_word_boundary('9'));
        assert!(!is_word_boundary('_'));

        assert!(is_word_char('a'));
        assert!(is_word_char('Z'));
        assert!(is_word_char('5'));
        assert!(is_word_char('_'));
        assert!(!is_word_char(' '));
        assert!(!is_word_char('.'));
    }

    #[test]
    fn test_word_boundary_expansion() {
        let text = "hello foo_bar 123 :: test()";
        let chars: Vec<char> = text.chars().collect();

        assert_eq!(expand_word_range(&chars, 0), (0, 4));
        assert_eq!(expand_word_range(&chars, 2), (0, 4));
        assert_eq!(expand_word_range(&chars, 4), (0, 4));

        assert_eq!(expand_word_range(&chars, 5), (5, 5));

        assert_eq!(expand_word_range(&chars, 6), (6, 12));
        assert_eq!(expand_word_range(&chars, 9), (6, 12));
        assert_eq!(expand_word_range(&chars, 12), (6, 12));

        assert_eq!(expand_word_range(&chars, 18), (18, 19));
        assert_eq!(expand_word_range(&chars, 19), (18, 19));

        assert_eq!(expand_word_range(&chars, 25), (25, 26));
        assert_eq!(expand_word_range(&chars, 26), (25, 26));

        assert_eq!(expand_word_boundary_str(text, 6), (6, 12));
    }

    #[test]
    fn test_line_selection_expansion() {
        let mut model = SelectionModel::new();
        model.on_press_line((15, 2), 80);

        assert_eq!(model.mode, SelectionMode::Line);
        assert_eq!(model.start, (0, 2));
        assert_eq!(model.end, (79, 2));
        assert!(model.active);
        assert!(model.is_dragging);

        model.on_drag((10, 4), 80);
        assert_eq!(model.start, (0, 2));
        assert_eq!(model.end, (79, 4));

        model.on_drag((5, 0), 80);
        assert_eq!(model.start, (0, 0));
        assert_eq!(model.end, (79, 2));
    }

    #[test]
    fn test_multi_line_string_assembly() {
        let lines = vec![
            "first line of text",
            "second line of text",
            "third line of text",
        ];

        let mut model = SelectionModel::new();
        model.active = true;

        model.start = (6, 0);
        model.end = (5, 1);
        model.mode = SelectionMode::Normal;

        let extracted = model.extract_text_from_lines(&lines);
        assert_eq!(extracted, "line of text\nsecond");

        model.start = (0, 0);
        model.end = (20, 2);
        model.mode = SelectionMode::Line;

        let line_extracted = model.extract_text_from_lines(&lines);
        assert_eq!(
            line_extracted,
            "first line of text\nsecond line of text\nthird line of text\n"
        );
    }

    #[test]
    fn test_wide_character_handling() {
        let row: Vec<GridCell> = vec![
            GridCell::new('H', 1),
            GridCell::new('i', 1),
            GridCell::empty(),
            GridCell::wide('你'),
            GridCell::wide_continuation(),
            GridCell::wide('好'),
            GridCell::wide_continuation(),
            GridCell::empty(),
            GridCell::new('!', 1),
        ];
        let grid = vec![row];

        let mut model = SelectionModel::new();
        model.active = true;
        model.start = (0, 0);
        model.end = (8, 0);

        let extracted = model.extract_text(&grid);
        assert_eq!(extracted, "Hi 你好 !");

        model.start = (3, 0);
        model.end = (6, 0);
        let wide_only = model.extract_text(&grid);
        assert_eq!(wide_only, "你好");
    }

    #[test]
    fn test_trailing_space_trimming() {
        let grid = vec![vec![
            GridCell::new('e', 1),
            GridCell::new('c', 1),
            GridCell::new('h', 1),
            GridCell::new('o', 1),
            GridCell::empty(),
            GridCell::empty(),
            GridCell::empty(),
        ]];

        let mut model = SelectionModel::new();
        model.active = true;
        model.start = (0, 0);
        model.end = (6, 0);

        assert_eq!(model.extract_text(&grid), "echo");
    }

    #[test]
    fn test_empty_selection_handling() {
        let model = SelectionModel::new();
        assert!(model.is_empty());

        let lines = vec!["some line"];
        assert_eq!(model.extract_text_from_lines(&lines), "");

        let mut clicked = SelectionModel::new();
        clicked.on_press((3, 0), SelectionMode::Normal, 80);
        assert!(!clicked.is_empty());

        clicked.on_release();
        assert!(clicked.is_empty());
        assert_eq!(clicked.extract_text_from_lines(&lines), "");

        let mut oob = SelectionModel::new();
        oob.active = true;
        oob.start = (0, 100);
        oob.end = (10, 100);
        assert_eq!(oob.extract_text_from_lines(&lines), "");
    }

    #[test]
    fn test_selection_model_contains() {
        let mut model = SelectionModel::new();
        model.active = true;
        model.start = (5, 1);
        model.end = (10, 3);

        assert!(!model.contains(4, 1));
        assert!(!model.contains(10, 0));

        assert!(model.contains(5, 1));
        assert!(model.contains(20, 1));

        assert!(model.contains(0, 2));
        assert!(model.contains(50, 2));

        assert!(model.contains(0, 3));
        assert!(model.contains(10, 3));
        assert!(!model.contains(11, 3));

        assert!(!model.contains(0, 4));

        model.clear();
        assert!(!model.contains(5, 1));
    }
}
