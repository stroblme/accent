//! An org table lined up on Tab and Return as a markdown note's pipe table is
//! ([`crate::markdown::table_key`]): every line opening with `|` a row of it, one opening with
//! `|-` an hline, redrawn to the columns' widths with `+` where they meet.

use crate::markdown::{Rules, Table, TableEdit, TableKey, split};

/// Tab, Shift+Tab or Return at `column` of line `at` in an org table, or `None` off one.
pub fn table_key(lines: &[&str], at: usize, column: usize, key: TableKey) -> Option<TableEdit> {
    let row = |i: usize| lines[i].trim_start().starts_with('|');
    if !row(at) {
        return None;
    }
    let top = (0..=at).rev().take_while(|&i| row(i)).last()?;
    let end = (at..lines.len()).take_while(|&i| row(i)).count() + at;
    let rows = lines[top..end].iter().map(|line| {
        // An hline's `+` marks where two columns meet, as a pipe does in a row.
        let hline = line.trim_start().starts_with("|-");
        let cells = match hline {
            true => split(&line.replace('+', "|"), false),
            false => split(line, false),
        };
        Some((cells?, hline))
    });
    Table {
        lines: top..end,
        rows: rows.collect::<Option<_>>()?,
        rules: Rules::Hline,
    }
    .key(lines, at, column, key)
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
    fn an_hline_is_redrawn_to_the_widths_of_uneven_rows() {
        let table = "| a | bb |\n|-\n| ccc | d | e |";
        assert_eq!(
            press(table, 0, 2, TableKey::Tab).unwrap(),
            "| a   | bb^ |   |\n|-----+----+---|\n| ccc | d  | e |"
        );
        assert_eq!(press("a | b", 0, 0, TableKey::Tab), None);
    }

    /// A row added past the last goes above an hline closing the table, and an empty last row
    /// ends it, as in a markdown note.
    #[test]
    fn return_adds_a_row_above_a_closing_hline() {
        let table = "|---|\n| a |\n|---|";
        assert_eq!(
            press(table, 1, 2, TableKey::Enter).unwrap(),
            "|---|\n| a |\n| ^  |\n|---|"
        );
        assert_eq!(
            press("|---|\n| a |\n|   |\n|---|", 2, 2, TableKey::Enter).unwrap(),
            "|---|\n| a |\n|---|\n^"
        );
    }
}
