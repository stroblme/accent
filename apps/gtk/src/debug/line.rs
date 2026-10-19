//! The Debug Console's prompt line, with no GTK: what VTE commits read as a shell's line editor
//! reads keys, and the prompt and the line drawn back over their last drawing. Also the text the
//! console shows, cleared of what the terminal would act on.

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
    /// The last drawing up to the cell the cursor stands on, prompt included. VTE keeps the
    /// cursor on that cell as it rewraps the line on a resize, so its row below the prompt's first
    /// is where this lays out ([`cell`]), whatever the width is now.
    drawn: String,
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
        for key in pieces(typed) {
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

    /// The prompt and the line over the last drawing, on rows of `columns`, the cursor where it
    /// is in the text. Columns go by display width: a wide character takes two.
    pub fn draw(&mut self, prompt: &str, columns: u16) -> String {
        use std::fmt::Write as _;
        let mut out = self.erase(columns);
        let columns = usize::from(columns.max(1));
        // A space after the text is the cell a cursor at its end stands on, rather than past the
        // edge of a row the line fills exactly, where a rewrap would not keep it.
        let under = self.text[self.cursor..].chars().next().unwrap_or(' ');
        let drawn = format!("{prompt}{}{under}", &self.text[..self.cursor]);
        let (row, column) = cell(&drawn, columns);
        let (end, _) = cell(&format!("{prompt}{} ", self.text), columns);
        out.push_str(prompt);
        out.push_str(&self.text);
        out.push(' ');
        // Writing leaves the cursor on the space's row.
        if end > row {
            _ = write!(out, "\x1b[{}A", end - row);
        }
        out.push('\r');
        if column > 0 {
            _ = write!(out, "\x1b[{column}C");
        }
        self.drawn = drawn;
        out
    }

    /// Back to where the prompt starts on rows of `columns`, and everything from there on
    /// cleared.
    pub fn erase(&mut self, columns: u16) -> String {
        let drawn = std::mem::take(&mut self.drawn);
        match cell(&drawn, usize::from(columns.max(1))).0 {
            0 => "\r\x1b[J".to_string(),
            rows => format!("\x1b[{rows}A\r\x1b[J"),
        }
    }
}

/// The row and column the last character of `s` starts on, laid out on rows of `columns` as VTE
/// lays out a line and rewraps it: a wide character that would start in a row's last column
/// starts the next row instead, leaving that column empty.
fn cell(s: &str, columns: usize) -> (usize, usize) {
    use unicode_width::UnicodeWidthChar as _;
    let (mut row, mut column, mut last) = (0, 0, (0, 0));
    for c in s.chars() {
        let width = c.width().unwrap_or(0);
        if column + width > columns {
            (row, column) = (row + 1, 0);
        }
        last = (row, column);
        column += width;
    }
    last
}

/// `text` without what a terminal acts on rather than shows: its escape sequences, and its
/// control characters but newlines and tabs.
pub fn clean(text: &str) -> String {
    let shown = |c: char| !c.is_control() || matches!(c, '\n' | '\t');
    pieces(text)
        .filter(|piece| piece.chars().all(shown))
        .collect()
}

/// `s` as a terminal reads it: character by character, an escape sequence whole.
fn pieces(s: &str) -> impl Iterator<Item = &str> {
    let mut rest = s;
    std::iter::from_fn(move || {
        let first = rest.chars().next()?;
        let (piece, after) = rest.split_at(match first {
            '\x1b' => sequence_len(rest),
            _ => first.len_utf8(),
        });
        rest = after;
        Some(piece)
    })
}

/// The length of the escape sequence `s` starts with: a CSI one (`ESC [`, parameters, a final
/// byte); an SS3 one (`ESC O` and a byte); a string one (`ESC ]` OSC, `ESC P` DCS, SOS, PM, APC)
/// up to its BEL, or up to the `ESC` starting its ST (`ESC \`), read as a sequence of its own;
/// `ESC`, intermediate bytes and a final one (`ESC ( B`); or `ESC` and the character after it
/// (Alt and a key).
fn sequence_len(s: &str) -> usize {
    let mut chars = s.char_indices().skip(1);
    let end = match chars.next() {
        Some((_, '[')) => chars.find(|(_, c)| ('\x40'..='\x7e').contains(c)),
        Some((_, 'O')) => chars.next(),
        Some((_, ']' | 'P' | 'X' | '^' | '_')) => {
            match chars.find(|(_, c)| "\x07\x1b".contains(*c)) {
                Some((at, '\x1b')) => return at,
                bel => bel,
            }
        }
        Some((_, ' '..='/')) => chars.find(|(_, c)| !(' '..='/').contains(c)),
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
    fn a_reply_the_terminal_sends_is_not_typed() {
        let mut line = Line::default();
        // VTE's answers to a DSR, an OSC 11 asked with BEL, an OSC 10 asked with ST, and XTVERSION.
        let replies = concat!(
            "\x1b[2;1R",
            "\x1b]11;rgb:0000/0000/0000\x07",
            "\x1b]10;rgb:c000/c000/c000\x1b\\",
            "\x1bP>|VTE(8401)\x1b\\",
        );
        assert_eq!(line.key(replies), []);
        assert_eq!(line.text, "");
    }

    #[test]
    fn text_shown_keeps_nothing_a_terminal_acts_on() {
        // A title, two queries, a cursor move and a DCS string, C0 and C1 controls, a charset.
        let program = concat!(
            "\x1b]0;pwned\x07\x1b[6n\x1b]11;?\x1b\\",
            "a\tb\x1b[2A\x1bP$qm\x1b\\\x08中\r\n",
            "\x1b(B\x1b[31mx\x07\u{9b}",
        );
        assert_eq!(clean(program), "a\tb中\nx");
    }

    #[test]
    fn a_wide_character_takes_two_columns() {
        let mut line = Line::default();
        line.key("é中");
        assert_eq!(line.draw(">>> ", 80), "\r\x1b[J>>> é中 \r\x1b[7C");
        // Rows of five now: the seven columns of the drawing at 80 rewrapped onto two, the cursor
        // on the second.
        assert_eq!(line.draw(">>> ", 5), "\x1b[1A\r\x1b[J>>> é中 \r\x1b[2C");
        assert_eq!(line.erase(5), "\x1b[1A\r\x1b[J");
        // On rows of six `中` would start in the last column, so VTE starts the next row with it
        // and leaves that column empty (measured with VTE 0.84, as its rewrap does too).
        assert_eq!(line.draw(">>> ", 6), "\r\x1b[J>>> é中 \r\x1b[2C");
        // The cursor on the last `中` of `>>> a中中中`, on the second row of six, the space after
        // the text on the third.
        let mut line = Line::default();
        line.key("a中中中\x1b[D");
        let drawn = "\r\x1b[J>>> a中中中 \x1b[1A\r\x1b[4C";
        assert_eq!(line.draw(">>> ", 6), drawn);
        // Rewrapped onto rows of five, that `中` would start in the second row's last column.
        assert_eq!(line.erase(5), "\x1b[2A\r\x1b[J");
    }

    #[test]
    fn a_resize_finds_the_prompt_where_vte_rewrapped_it() {
        let mut line = Line::default();
        line.key(&"a".repeat(36));
        // Forty columns fill two rows of twenty: the cursor stands on the space after them.
        assert_eq!(
            line.draw(">>> ", 20),
            format!("\r\x1b[J>>> {} \r", "a".repeat(36))
        );
        // Rewrapped onto rows of twelve, that space is on the fourth row; on rows of 41, the first.
        assert_eq!(line.erase(12), "\x1b[3A\r\x1b[J");
        line.draw(">>> ", 20);
        assert_eq!(line.erase(41), "\r\x1b[J");
        // Home, drawn on rows of twelve: up from the space's row to the prompt's.
        line.draw(">>> ", 20);
        line.key("\x01");
        let home = format!("\x1b[3A\r\x1b[J>>> {} \x1b[3A\r\x1b[4C", "a".repeat(36));
        assert_eq!(line.draw(">>> ", 12), home);
    }
}
