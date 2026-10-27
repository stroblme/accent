//! Edit GFM pipe tables with Tab and Enter, realigning columns after each change, and org's
//! through [`Table::key`] (`crate::org::table_key`).
//!
//! A pipe table has a header, a delimiter row and optional body rows. Unescaped pipes split cells,
//! including inside code. Columns use display widths (minimum three), while caret positions use
//! character offsets.

use std::ops::Range;
use unicode_width::UnicodeWidthStr;

/// A key the table helper answers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableKey {
    Tab,
    BackTab,
    Enter,
}

/// What a key in a table does to it: the lines it spans are replaced by `text`, and the caret
/// goes to a line of `text` and a column in characters.
#[derive(Debug, PartialEq, Eq)]
pub struct TableEdit {
    pub lines: Range<usize>,
    pub text: Vec<String>,
    pub caret: (usize, usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Align {
    None,
    Left,
    Center,
    Right,
}

/// A cell of a row: its text without the spaces around it, the column that text starts at, and
/// the column of the pipe that ends the cell, or of the end of its text where none does.
pub(crate) struct Cell {
    pub text: String,
    start: usize,
    end: usize,
}

/// A table as a syntax reads it off its text, for a key to lay out.
pub(crate) struct Table {
    /// The lines of the text it spans.
    pub lines: Range<usize>,
    /// Each of those lines' cells, and whether it is a rule across the table, a delimiter row or
    /// an org hline, rather than a row of text.
    pub rows: Vec<(Vec<Cell>, bool)>,
    pub rules: Rules,
    /// How wide a cell's text shows, in columns: as written in a pipe table.
    pub width: fn(&str) -> usize,
}

/// How a table's rules are drawn.
pub(crate) enum Rules {
    /// A delimiter row keeping each column's colons, a column three wide at least, as GFM asks.
    Delimiter(Vec<Align>),
    /// Org's hline, dashes with a `+` where two columns meet, a column one wide at least, as org
    /// lays a table out, and each column's alignment as org sets it.
    Hline(Vec<Align>),
}

/// Where the key leaves the caret.
enum Target {
    /// Where it was: in the same cell, at the same place in its text.
    Stay,
    /// At the end of the text of cell `.1` of content row `.0`.
    Cell(usize, usize),
    /// In the first cell of a new row at the end.
    Added,
    /// On the line the empty last row was, now the first line past the table.
    Left,
}

/// Apply Tab, Shift+Tab or Enter at `column` of line `at`, returning the realigned table and caret.
/// Tab moves between cells and adds a row after the last. Enter adds a row after the last filled
/// row or exits from an empty final row. Returns `None` outside a table.
pub fn table_key(lines: &[&str], at: usize, column: usize, key: TableKey) -> Option<TableEdit> {
    let rows: Vec<Option<Vec<Cell>>> = lines.iter().map(|line| split(line, true)).collect();
    rows.get(at)?.as_ref()?;
    let top = (0..=at).rev().take_while(|&i| rows[i].is_some()).last()?;
    let end = (at..rows.len()).take_while(|&i| rows[i].is_some()).count() + at;
    let (head, aligns) = (top..=at).find_map(|h| {
        let delimiter = rows.get(h + 1)?.as_ref()?;
        Some((h, aligns(rows[h].as_ref()?, delimiter)?))
    })?;
    let rows = rows.into_iter().take(end).skip(head).flatten();
    Table {
        lines: head..end,
        rows: rows.enumerate().map(|(i, cells)| (cells, i == 1)).collect(),
        rules: Rules::Delimiter(aligns),
        width: UnicodeWidthStr::width,
    }
    .key(lines, at, column, key)
}

impl Table {
    /// [`table_key`] in this table, read off `lines`, `at` being one of its lines.
    pub(crate) fn key(
        self,
        lines: &[&str],
        at: usize,
        column: usize,
        key: TableKey,
    ) -> Option<TableEdit> {
        let indent: String = lines[self.lines.start]
            .chars()
            .take_while(|c| c.is_whitespace())
            .collect();
        // The rows that hold text, the rules left out, and each line's row among them, `None` for
        // a rule.
        let mut order: Vec<Option<usize>> = Vec::new();
        let mut content: Vec<Vec<String>> = Vec::new();
        for (cells, rule) in &self.rows {
            order.push((!rule).then_some(content.len()));
            if !rule {
                content.push(cells.iter().map(|cell| cell.text.clone()).collect());
            }
        }
        let at = at - self.lines.start;
        let (cells, on_rule) = &self.rows[at];
        let k = cells
            .iter()
            .position(|cell| column <= cell.end)
            .unwrap_or(cells.len() - 1);
        let offset = column
            .saturating_sub(cells[k].start)
            .min(cells[k].text.chars().count());
        // A rule walks as the row above it does, or the one below where it opens the table.
        let row = order[..=at].iter().flatten().count().saturating_sub(1);
        let last = content.len().checked_sub(1)?;
        let n = content.iter().map(Vec::len).max().unwrap_or(0);
        let k = k.min(n - 1);
        let target = match key {
            TableKey::Enter
                if row == last && row > 0 && content[row].iter().all(String::is_empty) =>
            {
                Target::Left
            }
            TableKey::Enter if row == last => Target::Added,
            TableKey::Enter => Target::Stay,
            TableKey::Tab if k + 1 < n => Target::Cell(row, k + 1),
            TableKey::Tab if row < last => Target::Cell(row + 1, 0),
            TableKey::Tab => Target::Added,
            TableKey::BackTab if k > 0 => Target::Cell(row, k - 1),
            TableKey::BackTab if row > 0 => Target::Cell(row - 1, n - 1),
            TableKey::BackTab => Target::Cell(row, 0),
        };
        let line_of = |order: &[Option<usize>], row: usize| {
            order.iter().position(|r| *r == Some(row)).unwrap_or(0)
        };
        match target {
            Target::Left => {
                content.pop();
                order.retain(|r| *r != Some(last));
            }
            // At the end of a pipe table, past its delimiter row under a lone header; under the
            // last row of an org table, above an hline closing it, as org's own Tab puts it.
            Target::Added => {
                content.push(Vec::new());
                let below = match self.rules {
                    Rules::Delimiter(_) => order.len(),
                    Rules::Hline(_) => line_of(&order, last) + 1,
                };
                order.insert(below, Some(last + 1));
            }
            _ => {}
        }

        let (mut aligns, least) = match &self.rules {
            Rules::Delimiter(aligns) => (aligns.clone(), 3),
            Rules::Hline(aligns) => (aligns.clone(), 1),
        };
        aligns.resize(n, Align::None);
        let widths: Vec<usize> = (0..n)
            .map(|k| {
                content
                    .iter()
                    .filter_map(|row| row.get(k))
                    .map(|text| (self.width)(text))
                    .fold(least, usize::max)
            })
            .collect();
        // A row laid out, and the character each of its cells' text starts at.
        let lay = |cells: &[String]| {
            let mut line = format!("{indent}|");
            let mut starts = Vec::new();
            for (k, (&width, &align)) in widths.iter().zip(&aligns).enumerate() {
                let text = cells.get(k).map_or("", String::as_str);
                let space = width - (self.width)(text);
                let left = lead(space, align);
                starts.push(line.chars().count() + 1 + left);
                line += &format!(" {}{text}{} |", " ".repeat(left), " ".repeat(space - left));
            }
            (line, starts)
        };
        let drawn = match self.rules {
            Rules::Delimiter(_) => {
                let rules: Vec<String> = widths
                    .iter()
                    .zip(&aligns)
                    .map(|(&w, &a)| rule(w, a))
                    .collect();
                lay(&rules)
            }
            Rules::Hline(_) => {
                let dashes: Vec<String> = widths.iter().map(|w| "-".repeat(w + 2)).collect();
                (format!("{indent}|{}|", dashes.join("+")), lay(&[]).1)
            }
        };
        let laid: Vec<(String, Vec<usize>)> = order
            .iter()
            .map(|row| match row {
                Some(row) => lay(&content[*row]),
                None => drawn.clone(),
            })
            .collect();
        let length =
            |row: usize, k: usize| content[row].get(k).map_or(0, |text| text.chars().count());
        let caret = match target {
            Target::Stay if *on_rule => (at, laid[at].1[k] + offset.min(widths[k])),
            Target::Stay => (at, laid[at].1[k] + offset),
            Target::Cell(row, k) => {
                let line = line_of(&order, row);
                (line, laid[line].1[k] + length(row, k))
            }
            Target::Added => {
                let line = line_of(&order, last + 1);
                (line, laid[line].1[0])
            }
            Target::Left => (laid.len(), 0),
        };
        let mut text: Vec<String> = laid.into_iter().map(|(line, _)| line).collect();
        if matches!(target, Target::Left) {
            text.push(String::new());
        }
        Some(TableEdit {
            lines: self.lines,
            text,
            caret,
        })
    }
}

/// The cells of a table row, or `None` for a line with no pipe in it, which is no row. The pipes
/// at either end of a row, where it has them, end no cell, and with `escapes` neither does one
/// after a backslash, as in GFM; org has no escaped pipe.
pub(crate) fn split(line: &str, escapes: bool) -> Option<Vec<Cell>> {
    let chars: Vec<char> = line.chars().collect();
    let mut bars = Vec::new();
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if c == '|' && !escaped {
            bars.push(i);
        }
        escaped = escapes && c == '\\' && !escaped;
    }
    let first = chars.iter().position(|c| !c.is_whitespace())?;
    let last = chars.iter().rposition(|c| !c.is_whitespace())?;
    let start = first + usize::from(*bars.first()? == first);
    let to = match bars.last() == Some(&last) && last >= start {
        true => last,
        false => last + 1,
    };
    let mut cells = Vec::new();
    let mut from = start;
    for &bar in bars.iter().filter(|&&bar| bar >= start && bar < to) {
        cells.push(cell(&chars, from..bar));
        from = bar + 1;
    }
    cells.push(cell(&chars, from..to));
    Some(cells)
}

fn cell(chars: &[char], range: Range<usize>) -> Cell {
    let raw = &chars[range.clone()];
    let lead = raw.iter().take_while(|c| c.is_whitespace()).count();
    let text: String = raw[lead..].iter().collect();
    Cell {
        text: text.trim_end().to_string(),
        start: range.start + lead,
        end: range.end,
    }
}

/// The columns' alignments where `row` is a delimiter row under `header`: as many cells, each
/// dashes with an optional colon at either end.
fn aligns(header: &[Cell], row: &[Cell]) -> Option<Vec<Align>> {
    if row.len() != header.len() {
        return None;
    }
    row.iter()
        .map(|cell| {
            let text = cell.text.as_str();
            let (left, text) = text.strip_prefix(':').map_or((false, text), |t| (true, t));
            let (right, text) = text.strip_suffix(':').map_or((false, text), |t| (true, t));
            (!text.is_empty() && text.chars().all(|c| c == '-')).then_some(match (left, right) {
                (true, true) => Align::Center,
                (true, false) => Align::Left,
                (false, true) => Align::Right,
                (false, false) => Align::None,
            })
        })
        .collect()
}

/// How much of a cell's `space`, the columns its text leaves, goes before the text.
fn lead(space: usize, align: Align) -> usize {
    match align {
        Align::Right => space,
        Align::Center => space / 2,
        _ => 0,
    }
}

/// A delimiter cell `width` characters wide, keeping its column's colons.
fn rule(width: usize, align: Align) -> String {
    match align {
        Align::None => "-".repeat(width),
        Align::Left => format!(":{}", "-".repeat(width - 1)),
        Align::Right => format!("{}:", "-".repeat(width - 1)),
        Align::Center => format!(":{}:", "-".repeat(width - 2)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The table `text` laid out by `key` pressed at `column` of line `at`, with `^` marking where
    /// the caret goes.
    fn press(text: &str, at: usize, column: usize, key: TableKey) -> Option<String> {
        let lines: Vec<&str> = text.lines().collect();
        let edit = table_key(&lines, at, column, key)?;
        let mut out = edit.text;
        let line = &mut out[edit.caret.0];
        let byte = line
            .char_indices()
            .nth(edit.caret.1)
            .map_or(line.len(), |(b, _)| b);
        line.insert(byte, '^');
        Some(out.join("\n"))
    }

    #[test]
    fn columns_line_up_by_their_widest_cell() {
        let table = "| a | bb |\n|---|-|\n| ccc | d |\n| eeee |";
        assert_eq!(
            press(table, 2, 4, TableKey::Enter).unwrap(),
            "| a    | bb  |\n| ---- | --- |\n| cc^c  | d   |\n| eeee |     |"
        );
    }

    #[test]
    fn the_delimiter_row_keeps_each_columns_alignment() {
        let table = "|a|b|c|d|\n|:-|:-:|-:|-|\n|long|long|long|long|";
        assert_eq!(
            press(table, 0, 1, TableKey::Enter).unwrap(),
            "| ^a    |  b   |    c | d    |\n\
             | :--- | :--: | ---: | ---- |\n\
             | long | long | long | long |"
        );
    }

    /// The caret keeps its cell and its place in the cell's text, wherever the padding moved it.
    #[test]
    fn enter_away_from_the_last_row_only_lines_the_table_up() {
        let table = "| a | b |\n|-|-|\n|  wide cell |   xy   |\n| c | d |";
        assert_eq!(
            press(table, 2, 18, TableKey::Enter).unwrap(),
            "| a         | b   |\n| --------- | --- |\n| wide cell | x^y  |\n| c         | d   |"
        );
    }

    /// A pipe splits a cell even inside code, as GFM and the preview read it, unless escaped.
    #[test]
    fn only_an_escaped_pipe_stays_in_its_cell() {
        let table = "| a | b |\n|---|---|\n| x\\|y | `p\\|q` |\n| `r|s` |";
        assert_eq!(
            press(table, 0, 2, TableKey::Enter).unwrap(),
            "| ^a    | b      |\n| ---- | ------ |\n| x\\|y | `p\\|q` |\n| `r   | s`     |"
        );
    }

    /// A wide character takes two columns, so the table lines up in a monospace font, and the
    /// caret still lands by characters.
    #[test]
    fn a_wide_character_takes_two_columns() {
        let table = "| 漢字 | x |\n|---|---|\n| a | 🙂 |";
        assert_eq!(
            press(table, 0, 2, TableKey::Tab).unwrap(),
            "| 漢字 | x^   |\n| ---- | --- |\n| a    | 🙂  |"
        );
        assert_eq!(
            press(table, 2, 6, TableKey::BackTab).unwrap(),
            "| 漢字 | x   |\n| ---- | --- |\n| a^    | 🙂  |"
        );
    }

    #[test]
    fn a_table_without_outer_pipes_gets_them() {
        let table = "a | b\n--|--\nc | d";
        assert_eq!(
            press(table, 2, 0, TableKey::Enter).unwrap(),
            "| a   | b   |\n| --- | --- |\n| c   | d   |\n| ^    |     |"
        );
    }

    /// Tab walks the cells to the end of each one's text and adds a row past the last; Shift+Tab
    /// walks back and stops at the first.
    #[test]
    fn tab_walks_the_cells() {
        let table = "| a | b |\n|---|---|\n| c | d |";
        let tab = |at, column| press(table, at, column, TableKey::Tab).unwrap();
        let back = |at, column| press(table, at, column, TableKey::BackTab).unwrap();
        assert_eq!(tab(0, 2), "| a   | b^   |\n| --- | --- |\n| c   | d   |");
        assert_eq!(tab(0, 6), "| a   | b   |\n| --- | --- |\n| c^   | d   |");
        assert_eq!(
            tab(2, 6),
            "| a   | b   |\n| --- | --- |\n| c   | d   |\n| ^    |     |"
        );
        assert_eq!(back(2, 2), "| a   | b^   |\n| --- | --- |\n| c   | d   |");
        assert_eq!(back(0, 2), "| a^   | b   |\n| --- | --- |\n| c   | d   |");
    }

    /// Enter in an empty last row takes it away and leaves the table, as an empty item ends a
    /// list; a header with no row under it gets one.
    #[test]
    fn enter_in_an_empty_last_row_leaves_the_table() {
        let table = "| a | b |\n|---|---|\n| c | d |\n|  |  |";
        assert_eq!(
            press(table, 3, 2, TableKey::Enter).unwrap(),
            "| a   | b   |\n| --- | --- |\n| c   | d   |\n^"
        );
        assert_eq!(
            press("| a |\n|---|", 0, 2, TableKey::Enter).unwrap(),
            "| a   |\n| --- |\n| ^    |"
        );
    }

    /// A table is a header and a delimiter row with as many cells, found from any of its rows; a
    /// line before the header that holds a pipe is not part of it.
    #[test]
    fn only_a_header_over_a_delimiter_row_is_a_table() {
        assert_eq!(press("| a | b |\n| c | d |", 0, 2, TableKey::Tab), None);
        assert_eq!(press("| a | b |\n|---|", 0, 2, TableKey::Tab), None);
        assert_eq!(press("| a |\n| --- |\nplain", 2, 2, TableKey::Tab), None);
        let lines = ["a | b in prose", "| x |", "|---|", "| y |"];
        let edit = table_key(&lines, 3, 2, TableKey::Enter).unwrap();
        assert_eq!(edit.lines, 1..4);
    }

    /// An indented table, a list item's, keeps its indent.
    #[test]
    fn an_indented_table_keeps_its_indent() {
        assert_eq!(
            press("  | a |\n  |-|", 0, 4, TableKey::Tab).unwrap(),
            "  | a   |\n  | --- |\n  | ^    |"
        );
    }
}
