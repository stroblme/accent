//! Capture shell output with `vt100::Parser` and replay history, screen, cursor, and input
//! modes on attach. Scroll margins, character sets, cursor shape, and OSC 7 working
//! directory are not restored; full-screen programs are asked to redraw. Soft wraps
//! return as hard wraps.

/// History lines per shell, matching VTE scrollback. At 120 columns, a full history uses about
/// 38 MB.
pub const SCROLLBACK: usize = 10_000;

/// The window title the shell last set (OSC 0 or 2).
#[derive(Default)]
pub struct Title(pub Vec<u8>);

impl vt100::Callbacks for Title {
    fn set_window_title(&mut self, _: &mut vt100::Screen, title: &[u8]) {
        self.0 = title.to_vec();
    }
}

pub type Parser = vt100::Parser<Title>;

pub fn parser(rows: u16, cols: u16) -> Parser {
    Parser::new_with_callbacks(rows, cols, SCROLLBACK, Title::default())
}

/// On shrink, move rows above the cursor into history and keep the cursor with its text.
/// Resize the hidden shell screen too. `rows` and `cols` must be nonzero.
pub fn resize(p: &mut Parser, rows: u16, cols: u16) {
    // `?47` switches grids without clearing either, which `?1049` would do.
    // ponytail: here and in `replay`, sequences of ours go through the shell's own parser, so one
    // the shell's output left half-written at that moment is garbled (rare: a read ending inside
    // an escape). A fix needs vt100 to expose its grid switch or its parser state.
    let alt = p.screen().alternate_screen();
    if alt {
        p.process(b"\x1b[?47l");
    }
    let (row, _) = p.screen().cursor_position();
    if row >= rows {
        let n = row + 1 - rows;
        p.process(format!("\x1b[{n}S\x1b[{n}A").as_bytes());
    }
    if alt {
        p.process(b"\x1b[?47h");
    }
    p.screen_mut().set_size(rows, cols);
}

/// The bytes that turn an empty terminal of the parser's size into the shell's: the history,
/// then the screen, and with a full-screen program running, the shell's screen hidden behind
/// the program's, so that quitting it shows what was there before.
pub fn replay(p: &mut Parser) -> Vec<u8> {
    let alt = p.screen().alternate_screen();
    let mut out = Vec::new();
    if alt {
        p.process(b"\x1b[?47l");
    }
    history(p, &mut out);
    if alt {
        out.extend(p.screen().contents_formatted());
        out.extend(b"\x1b[?1049h");
        p.process(b"\x1b[?47h");
    }
    out.extend(p.screen().state_formatted());
    let title = &p.callbacks().0;
    if !title.is_empty() {
        out.extend(b"\x1b]2;");
        out.extend(title);
        out.push(b'\x07');
    }
    out
}

/// Replay shell history oldest first with attributes reset per line. Read vt100 scrollback in
/// pages, then add newlines to move it fully into terminal history before painting a blank
/// screen.
fn history(p: &mut Parser, out: &mut Vec<u8>) {
    let screen = p.screen_mut();
    let rows = usize::from(screen.size().0);
    screen.set_scrollback(usize::MAX);
    let mut back = screen.scrollback();
    while back > 0 {
        screen.set_scrollback(back);
        for line in screen.rows_formatted(0, u16::MAX).take(back.min(rows)) {
            out.extend(line);
            out.extend(b"\x1b[m\r\n");
        }
        back = back.saturating_sub(rows);
    }
    screen.set_scrollback(0);
    out.extend(b"\n".repeat(rows - 1));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Return history text, oldest first, and its line count.
    fn history_of(p: &mut Parser) -> (String, usize) {
        p.screen_mut().set_scrollback(usize::MAX);
        let seen = (p.screen().contents(), p.screen().scrollback());
        p.screen_mut().set_scrollback(0);
        seen
    }

    /// Replay `live` into a fresh parser, which stands in for the terminal on the other end.
    fn replayed(live: &mut Parser) -> Parser {
        let (rows, cols) = live.screen().size();
        let mut terminal = parser(rows, cols);
        terminal.process(&replay(live));
        terminal
    }

    #[test]
    fn a_replay_brings_back_the_history_and_the_screen() {
        let mut live = parser(3, 20);
        live.process(
            b"\x1b[1mone\x1b[m\r\n\x1b[31mtwo\x1b[m\r\n  three\r\nfour\r\n\x1b[42mfi\x1b[mve",
        );
        let mut terminal = replayed(&mut live);
        assert_eq!(
            terminal.screen().contents_formatted(),
            live.screen().contents_formatted()
        );
        assert_eq!(
            terminal.screen().cursor_position(),
            live.screen().cursor_position()
        );
        // Once, not twice: the replay must not leave its own copy of the screen in the history.
        assert_eq!(history_of(&mut terminal), history_of(&mut live));
        assert_eq!(history_of(&mut live).1, 2);
    }

    #[test]
    fn a_full_screen_program_returns_over_the_shell_it_hid() {
        let mut live = parser(3, 20);
        live.process(b"a\r\nb\r\nc\r\n$ vim\r\n\x1b[?1049h\x1b[Hbuffer\x1b[2;1H~");
        let mut terminal = replayed(&mut live);
        assert_eq!(terminal.screen().contents(), live.screen().contents());
        assert!(live.screen().alternate_screen());
        assert_eq!(live.screen().scrollback(), 0);

        for p in [&mut live, &mut terminal] {
            p.process(b"\x1b[?1049l");
        }
        assert_eq!(terminal.screen().contents(), live.screen().contents());
        assert_eq!(
            terminal.screen().cursor_position(),
            live.screen().cursor_position()
        );
        assert_eq!(history_of(&mut terminal), history_of(&mut live));
    }

    #[test]
    fn the_title_comes_back() {
        let mut live = parser(3, 20);
        live.process(b"\x1b]2;make test\x07$ ");
        assert_eq!(replayed(&mut live).callbacks().0, b"make test");
    }

    #[test]
    fn shrinking_keeps_the_latest_lines() {
        // On the shell's own screen, and on the one a full-screen program hides.
        for program in ["", "\x1b[?1049hvim"] {
            let mut p = parser(5, 20);
            p.process(format!("1\r\n2\r\n3\r\n4\r\n5{program}").as_bytes());
            resize(&mut p, 3, 20);
            p.process(b"\x1b[?1049l");
            assert_eq!(p.screen().contents(), "3\n4\n5", "{program:?}");
            assert_eq!(history_of(&mut p), ("1\n2\n3".into(), 2), "{program:?}");
        }
    }
}
