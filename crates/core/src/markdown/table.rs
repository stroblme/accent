//! GFM pipe tables as the editor keeps them: Tab and Enter in one lay it out again with its
//! columns lined up, and walk its cells.
//!
//! A table is a header row and a delimiter row of dashes holding as many cells, then the body
//! rows, every one a line holding a pipe. A row splits at each pipe not escaped with a backslash,
//! inside code too, as GFM and the preview split it. A column is as wide as its widest cell, three
//! at least, so the delimiter row has room for its colons. Widths are display columns, a wide
//! character (CJK, most emoji) taking two, so the table lines up in a monospace font; the caret's
//! columns are characters, as the buffer counts them.

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
enum Align {
    None,
    Left,
    Center,
    Right,
}

/// A cell of a row: its text without the spaces around it, the column that text starts at, and
/// the column of the pipe that ends the cell, or of the end of its text where none does.
struct Cell {
    text: String,
    start: usize,
    end: usize,
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

/// Tab, Shift+Tab or Enter at `column` of line `at` of `lines`: the table laid out again with its
/// columns lined up, and where the caret goes. Tab and Shift+Tab go to the next or previous cell,
/// Tab in the last cell adding a row; Enter in the last row adds one, ends the table in an empty
/// one, and elsewhere leaves the caret where it is. `None` where line `at` is in no table.
pub fn table_key(lines: &[&str], at: usize, column: usize, key: TableKey) -> Option<TableEdit> {
    let rows: Vec<Option<Vec<Cell>>> = lines.iter().map(|line| split(line)).collect();
    rows.get(at)?.as_ref()?;
    let top = (0..=at).rev().take_while(|&i| rows[i].is_some()).last()?;
    let end = (at..rows.len()).take_while(|&i| rows[i].is_some()).count() + at;
    let (head, mut aligns) = (top..=at).find_map(|h| {
        let delimiter = rows.get(h + 1)?.as_ref()?;
        Some((h, aligns(rows[h].as_ref()?, delimiter)?))
    })?;
    let indent: String = lines[head]
        .chars()
        .take_while(|c| c.is_whitespace())
        .collect();
    // The header, then the body rows: the rows that hold text, the delimiter row left out.
    let mut content: Vec<Vec<String>> = (head..end)
        .filter(|&i| i != head + 1)
        .map(|i| {
            rows[i]
                .iter()
                .flatten()
                .map(|cell| cell.text.clone())
                .collect()
        })
        .collect();
    let cells = rows[at].as_ref()?;
    let k = cells
        .iter()
        .position(|cell| column <= cell.end)
        .unwrap_or(cells.len() - 1);
    let offset = column
        .saturating_sub(cells[k].start)
        .min(cells[k].text.chars().count());
    // The delimiter row walks as the header does.
    let row = (at - head).saturating_sub(1);
    let last = content.len() - 1;
    let n = content.iter().map(Vec::len).max().unwrap_or(0);
    let target = match key {
        TableKey::Enter if row == last && row > 0 && content[row].iter().all(String::is_empty) => {
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
    match target {
        Target::Left => {
            content.pop();
        }
        Target::Added => content.push(Vec::new()),
        _ => {}
    }

    aligns.resize(n, Align::None);
    let widths: Vec<usize> = (0..n)
        .map(|k| {
            content
                .iter()
                .filter_map(|row| row.get(k))
                .map(|text| text.width())
                .fold(3, usize::max)
        })
        .collect();
    // A row laid out, and the character each of its cells' text starts at.
    let lay = |cells: &[String]| {
        let mut line = format!("{indent}|");
        let mut starts = Vec::new();
        for (k, (&width, &align)) in widths.iter().zip(&aligns).enumerate() {
            let text = cells.get(k).map_or("", String::as_str);
            let space = width - text.width();
            let left = lead(space, align);
            starts.push(line.chars().count() + 1 + left);
            line += &format!(" {}{text}{} |", " ".repeat(left), " ".repeat(space - left));
        }
        (line, starts)
    };
    let rules: Vec<String> = widths
        .iter()
        .zip(&aligns)
        .map(|(&w, &a)| rule(w, a))
        .collect();
    let mut laid: Vec<(String, Vec<usize>)> = content.iter().map(|cells| lay(cells)).collect();
    laid.insert(1, lay(&rules));
    let line_of = |row: usize| if row == 0 { 0 } else { row + 1 };
    let length = |row: usize, k: usize| content[row].get(k).map_or(0, |text| text.chars().count());
    let caret = match target {
        Target::Stay if at == head + 1 => (1, laid[1].1[k] + offset.min(widths[k])),
        Target::Stay => (line_of(row), laid[line_of(row)].1[k] + offset),
        Target::Cell(row, k) => (line_of(row), laid[line_of(row)].1[k] + length(row, k)),
        Target::Added => (laid.len() - 1, laid[laid.len() - 1].1[0]),
        Target::Left => (laid.len(), 0),
    };
    let mut text: Vec<String> = laid.into_iter().map(|(line, _)| line).collect();
    if matches!(target, Target::Left) {
        text.push(String::new());
    }
    Some(TableEdit {
        lines: head..end,
        text,
        caret,
    })
}

/// The cells of a table row, or `None` for a line with no pipe in it, which is no row. The pipes
/// at either end of a row, where it has them, end no cell.
fn split(line: &str) -> Option<Vec<Cell>> {
    let chars: Vec<char> = line.chars().collect();
    let mut bars = Vec::new();
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if c == '|' && !escaped {
            bars.push(i);
        }
        escaped = c == '\\' && !escaped;
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
