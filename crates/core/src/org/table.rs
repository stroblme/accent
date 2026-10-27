//! An org table lined up on Tab and Return as a markdown note's pipe table is
//! ([`crate::markdown::table_key`]): every line opening with `|` a row of it, one opening with
//! `|-` an hline, redrawn to the columns' widths with `+` where they meet, and a column of numbers
//! right aligned, as org's own Tab lines a table up (`org-table-align`).

use crate::markdown::{Align, Cell, Rules, Table, TableEdit, TableKey, split};
use regex::Regex;
use std::sync::LazyLock;

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
    let rows: Vec<(Vec<Cell>, bool)> = rows.collect::<Option<_>>()?;
    let aligns = aligns(&rows);
    Table {
        lines: top..end,
        rows,
        rules: Rules::Hline(aligns),
    }
    .key(lines, at, column, key)
}

/// Each column's alignment: right where at least half of its filled cells, a header's too, are
/// numbers (`org-table-number-fraction`), left otherwise.
fn aligns(rows: &[(Vec<Cell>, bool)]) -> Vec<Align> {
    let rows: Vec<&[Cell]> = rows
        .iter()
        .filter(|(_, hline)| !hline)
        .map(|(c, _)| &c[..])
        .collect();
    let n = rows.iter().map(|cells| cells.len()).max().unwrap_or(0);
    (0..n)
        .map(|k| {
            let filled = rows
                .iter()
                .filter_map(|cells| cells.get(k))
                .filter(|c| !c.text.is_empty());
            let (numbers, all) = filled.fold((0, 0), |(n, all), c| {
                (n + usize::from(number(&c.text)), all + 1)
            });
            match 2 * numbers >= all {
                true => Align::Right,
                false => Align::None,
            }
        })
        .collect()
}

/// Whether org reads a cell as a number: `org-table-number-regexp`'s default, which takes
/// `12`, `2.4e-08`, `4.034+-0.02`, `>3.5` or `0x1F`, matched as Emacs does, ignoring case.
fn number(cell: &str) -> bool {
    static NUMBER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(concat!(
            r"(?i)^(?:[<>]?[-+^.0-9]*[0-9][-+^.0-9eEdDx()%:]*|[<>]?[-+]?0[xX][[:xdigit:].]+",
            r"|[<>]?[-+]?[0-9]+#[0-9a-zA-Z.]+|nan|[-+u]?inf)$",
        ))
        .expect("a valid regex")
    });
    NUMBER.is_match(cell)
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

    /// A column at least half of whose filled cells are numbers is right aligned, as org's own Tab
    /// aligns it, the header counting as one of them.
    #[test]
    fn a_column_of_numbers_is_right_aligned() {
        let table = "| Item | Qty | |\n|-\n| tea | 12 | 3 |\n| cake | 2.5e3 | xy |";
        assert_eq!(
            press(table, 2, 3, TableKey::Enter).unwrap(),
            "| Item |   Qty |    |\n|------+-------+----|\n| t^ea  |    12 |  3 |\n| cake | 2.5e3 | xy |"
        );
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
