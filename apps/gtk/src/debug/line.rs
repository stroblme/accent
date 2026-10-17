//! The Debug Console's prompt line, with no GTK: what VTE commits read as a shell's line editor
//! reads keys, and the prompt and the line drawn back over their last drawing.

/// The line being typed at the prompt, and the lines submitted before it.
#[derive(Default)]
pub struct Line {
    text: String,
    /// A byte offset into `text`, on a char boundary.
    cursor: usize,
    history: Vec<String>,
    /// The history entry shown while walking it.
    recalled: Option<usize>,
    /// What was being typed as the walk began, which Down past the newest entry brings back.
    draft: String,
    /// The rows below the prompt's first that the last drawing left the cursor on.
    rows: u16,
}

/// What a key asks of the console.
#[derive(Debug, PartialEq)]
pub enum Did {
    Redraw,
    /// The line, sent; the prompt is empty again.
    Submit(String),
    /// The line, given up with Ctrl+C.
    Abandon(String),
}

impl Line {
    /// Read what VTE committed: typed text, a paste, or the bytes a key sends (`\x7f` for
    /// Backspace, `\x1b[D` for Left, …), as a shell's line editor reads them.
    pub fn key(&mut self, typed: &str) -> Vec<Did> {
        let mut did = Vec::new();
        let mut rest = typed;
        while let Some(first) = rest.chars().next() {
            let (key, after) = rest.split_at(match first {
                '\x1b' => sequence_len(rest),
                _ => first.len_utf8(),
            });
            rest = after;
            match self.one(key) {
                Some(Did::Redraw) if did.last() == Some(&Did::Redraw) => {}
                Some(next) => did.push(next),
                None => {}
            }
        }
        did
    }

    /// One key: a character, a control character or a whole escape sequence.
    fn one(&mut self, key: &str) -> Option<Did> {
        let before = self.text[..self.cursor].chars().next_back();
        let under = self.text[self.cursor..].chars().next();
        match key {
            "\r" => {
                let text = self.take();
                if !text.trim().is_empty() && self.history.last() != Some(&text) {
                    self.history.push(text.clone());
                }
                return Some(Did::Submit(text));
            }
            "\x03" => return Some(Did::Abandon(self.take())),
            "\x7f" | "\x08" => {
                self.cursor -= before?.len_utf8();
                self.text.remove(self.cursor);
            }
            "\x1b[3~" => {
                under?;
                self.text.remove(self.cursor);
            }
            "\x1b[D" => self.cursor -= before?.len_utf8(),
            "\x1b[C" => self.cursor += under?.len_utf8(),
            "\x1b[H" | "\x01" => self.cursor = 0,
            "\x1b[F" | "\x05" => self.cursor = self.text.len(),
            "\x1b[A" => self.recall(true)?,
            "\x1b[B" => self.recall(false)?,
            "\x15" => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            _ if !key.chars().any(char::is_control) => {
                self.text.insert_str(self.cursor, key);
                self.cursor += key.len();
            }
            _ => return None,
        }
        Some(Did::Redraw)
    }

    /// The line, leaving the prompt empty and the history walk over.
    fn take(&mut self) -> String {
        self.cursor = 0;
        self.recalled = None;
        self.draft.clear();
        std::mem::take(&mut self.text)
    }

    /// Up (`back`) to the entry before, or Down to the one after, the draft past the newest.
    fn recall(&mut self, back: bool) -> Option<()> {
        let to = match (self.recalled, back) {
            (None, true) => self.history.len().checked_sub(1)?,
            (Some(at), true) => at.checked_sub(1)?,
            (Some(at), false) => at + 1,
            (None, false) => return None,
        };
        if self.recalled.is_none() {
            self.draft = std::mem::take(&mut self.text);
        }
        self.recalled = (to < self.history.len()).then_some(to);
        self.text = match self.recalled {
            Some(at) => self.history[at].clone(),
            None => std::mem::take(&mut self.draft),
        };
        self.cursor = self.text.len();
        Some(())
    }

    /// The prompt and the line over the last drawing, the cursor where it is in the text.
    /// Columns go by display width: a wide character takes two.
    pub fn draw(&mut self, prompt: &str, columns: u16) -> String {
        use std::fmt::Write as _;
        use unicode_width::UnicodeWidthStr as _;
        let columns = usize::from(columns.max(1));
        let end = prompt.width() + self.text.width();
        let at = prompt.width() + self.text[..self.cursor].width();
        let mut out = self.erase();
        out.push_str(prompt);
        out.push_str(&self.text);
        // Writing leaves the cursor on the last row written. A line that fills its last row
        // exactly has not wrapped yet, so a cursor at its end needs the row after made.
        let (end_row, at_row) = (end.saturating_sub(1) / columns, at / columns);
        match at_row.cmp(&end_row) {
            std::cmp::Ordering::Greater => out.push_str("\r\n"),
            std::cmp::Ordering::Less => _ = write!(out, "\x1b[{}A", end_row - at_row),
            std::cmp::Ordering::Equal => {}
        }
        out.push('\r');
        let column = at % columns;
        if column > 0 {
            _ = write!(out, "\x1b[{column}C");
        }
        self.rows = at_row as u16;
        out
    }

    /// Back to where the prompt starts, and everything from there on cleared.
    pub fn erase(&mut self) -> String {
        match std::mem::take(&mut self.rows) {
            0 => "\r\x1b[J".to_string(),
            rows => format!("\x1b[{rows}A\r\x1b[J"),
        }
    }
}

/// The length of the escape sequence `s` starts with: a CSI one (`ESC [`, parameters, a final
/// byte), an SS3 one (`ESC O` and a byte), or `ESC` and the character after it (Alt and a key).
fn sequence_len(s: &str) -> usize {
    let mut chars = s.char_indices().skip(1);
    let end = match chars.next() {
        Some((_, '[')) => chars.find(|(_, c)| ('\x40'..='\x7e').contains(c)),
        Some((_, 'O')) => chars.next(),
        next => next,
    };
    end.map_or(s.len(), |(at, c)| at + c.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_editing_and_history_read_as_a_shell_does() {
        let mut line = Line::default();
        assert_eq!(line.key("x*2"), [Did::Redraw]);
        line.key("\x1b[D\x1b[D ");
        assert_eq!(line.text, "x *2");
        line.key("\x7f");
        assert_eq!(line.key("\r"), [Did::Submit("x*2".into())]);
        // Up recalls it; Down past the newest brings back what was being typed.
        line.key("dra");
        assert_eq!(line.key("\x1b[A"), [Did::Redraw]);
        assert_eq!(line.text, "x*2");
        assert_eq!(line.key("\x1b[A"), [], "nothing older");
        line.key("\x1b[B");
        assert_eq!(line.text, "dra");
        // Ctrl+U clears to the start; a sequence it does not know (Ctrl+Right, and F5 and F10
        // with Forward All Keys) is ignored whole.
        line.key("\x1b[1;5C\x1b[15~\x1b[21~\x15");
        assert_eq!((line.text.as_str(), line.cursor), ("", 0));
        assert_eq!(line.key("y\x03"), [Did::Redraw, Did::Abandon("y".into())]);
        // A paste holding newlines submits each line in turn.
        assert_eq!(
            line.key("1\r2\r"),
            [
                Did::Redraw,
                Did::Submit("1".into()),
                Did::Redraw,
                Did::Submit("2".into())
            ]
        );
    }

    #[test]
    fn a_wide_character_takes_two_columns() {
        let mut line = Line::default();
        line.key("é中");
        assert_eq!(line.draw(">>> ", 80), "\r\x1b[J>>> é中\r\x1b[7C");
        // Seven columns on rows of six: the cursor is on the second row, which the next drawing
        // goes back up from.
        assert_eq!(line.draw(">>> ", 6), "\r\x1b[J>>> é中\r\x1b[1C");
        assert_eq!(line.erase(), "\x1b[1A\r\x1b[J");
    }
}
