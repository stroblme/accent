//! CSV column structure: the byte ranges an editor needs to colour a table by column.
//!
//! GtkSourceView's `csv.lang` only knows numbers and strings, so the columns themselves have to
//! come from here. All ranges are byte offsets into the input text, like `markdown`'s spans.

use std::ops::Range;

/// One cell of a row: the field as written (quotes included) and its zero-based column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub range: Range<usize>,
    pub column: usize,
}

/// The candidates, in the order that breaks a tie.
const CANDIDATES: [u8; 3] = [b',', b';', b'\t'];

/// The delimiter in use: whichever of `,`, `;` or tab occurs most on the first line outside
/// quotes. `,` when the line has none of them, and when two of them tie.
pub fn delimiter(text: &str) -> u8 {
    let mut counts = [0usize; 3];
    let mut quoted = false;
    for &b in text.as_bytes() {
        match b {
            // A doubled quote toggles twice and lands back inside, which is all we need here:
            // nothing can sit between the two halves of a `""`.
            b'"' => quoted = !quoted,
            b'\n' if !quoted => break,
            _ if !quoted => {
                if let Some(i) = CANDIDATES.iter().position(|&c| c == b) {
                    counts[i] += 1;
                }
            }
            _ => {}
        }
    }
    let best = counts.iter().copied().max().unwrap_or(0);
    if best == 0 {
        return b',';
    }
    CANDIDATES[counts.iter().position(|&n| n == best).unwrap()]
}

/// Every cell of every row, in document order. Columns restart at each line.
///
/// RFC 4180 quoting: a `"` opens a quoted field, `""` inside it is a literal quote, and a
/// delimiter or newline inside quotes ends neither the cell nor the row. An empty field still
/// yields a cell with an empty range, so column positions stay aligned across rows.
pub fn columns(text: &str) -> Vec<Cell> {
    let delim = delimiter(text);
    let bytes = text.as_bytes();
    let mut cells = Vec::new();
    let mut push = |range: Range<usize>, column: usize| cells.push(Cell { range, column });
    let (mut start, mut column, mut quoted, mut i) = (0, 0, false, 0);
    // Every byte we branch on is ASCII, and a UTF-8 continuation byte is never ASCII, so the
    // ranges this produces always land on character boundaries.
    while i < bytes.len() {
        let b = bytes[i];
        if quoted {
            if b == b'"' {
                if bytes.get(i + 1) == Some(&b'"') {
                    i += 2; // A literal quote, still inside the cell.
                    continue;
                }
                quoted = false;
            }
        } else if b == b'"' {
            quoted = true;
        } else if b == delim {
            push(start..i, column);
            start = i + 1;
            column += 1;
        } else if b == b'\n' {
            // Leave a CRLF's `\r` out of the cell.
            let end = if i > start && bytes[i - 1] == b'\r' {
                i - 1
            } else {
                i
            };
            push(start..end, column);
            start = i + 1;
            column = 0;
        }
        i += 1;
    }
    // The last row only has a trailing cell if the text did not end on a row terminator; after a
    // delimiter it does, even when the field behind it is empty.
    if start < bytes.len() || column > 0 {
        push(start..bytes.len(), column);
    }
    cells
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fields<'a>(text: &'a str, cells: &[Cell]) -> Vec<&'a str> {
        // Slicing by the raw ranges is the char-boundary check: a split character panics here.
        cells.iter().map(|c| &text[c.range.clone()]).collect()
    }

    #[test]
    fn sniffs_semicolon_and_tab_delimiters() {
        assert_eq!(delimiter("a;b;c\n1;2;3\n"), b';');
        assert_eq!(delimiter("a\tb\tc\n"), b'\t');
        assert_eq!(delimiter("one field\n"), b',');
        // A tie and quoted delimiters both fall back to the comma.
        assert_eq!(delimiter("a,b;c\n"), b',');
        assert_eq!(delimiter("\"a;b;c\",d\n"), b',');
    }

    #[test]
    fn quoted_delimiters_and_doubled_quotes_stay_inside_the_cell() {
        let text = "\"a,b\",\"say \"\"hi\"\"\",c\n";
        let cells = columns(text);
        assert_eq!(fields(text, &cells), ["\"a,b\"", "\"say \"\"hi\"\"\"", "c"]);
        assert_eq!(
            cells.iter().map(|c| c.column).collect::<Vec<_>>(),
            [0, 1, 2]
        );

        // A newline inside quotes does not start a row.
        let text = "\"line 1\nline 2\",x\n";
        assert_eq!(fields(text, &columns(text)), ["\"line 1\nline 2\"", "x"]);
    }

    #[test]
    fn columns_restart_on_every_line() {
        let text = "a,,é\r\nb,c,d\n";
        let cells = columns(text);
        assert_eq!(fields(text, &cells), ["a", "", "é", "b", "c", "d"]);
        assert_eq!(
            cells.iter().map(|c| c.column).collect::<Vec<_>>(),
            [0, 1, 2, 0, 1, 2]
        );
        // An unterminated last row, and its empty trailing field, still count.
        let text = "a,b\nc,";
        assert_eq!(fields(text, &columns(text)), ["a", "b", "c", ""]);
    }
}
