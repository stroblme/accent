//! What a shell's end does to its tab.

use std::rc::Rc;

use vte4::TerminalExt;

use super::{Shell, Term};

/// Notify the caller of shell exit so it can close the tab.
///
/// Remote disconnects retain the tab for [`Term::reopen`]; local exit 255 is ordinary exit.
/// Failed `accent-cli attach` tabs also remain so their error can be read. Ctrl+W closes either.
/// A command's or an editor's tab goes as [`ended`] says, kept saying how it ended otherwise, as
/// a debugged program's is.
pub fn on_exit(
    term: &Rc<Term>,
    done: impl Fn(&Rc<Term>, Ended) + 'static,
    lost: impl Fn(&Rc<Term>) + 'static,
) {
    let weak = Rc::downgrade(term);
    term.view.connect_child_exited(move |view, status| {
        let Some(term) = weak.upgrade() else {
            return;
        };
        match &term.shell {
            // Kept, saying how it ended: its output is what was being debugged.
            Shell::Program(program) => {
                term.ended.set(true);
                say_exit(view, &exit_line(status, program.link.is_some()));
            }
            Shell::Remote { at, .. } if link_lost(status) => {
                term.lost.set(true);
                let line = format!(
                    "\r\n[Lost the connection to {}. Press a key to reconnect.]\r\n",
                    at.host
                );
                view.feed(line.as_bytes());
                lost(&term);
            }
            _ if let Some(run) = &term.run => {
                term.ended.set(true);
                match ended(status, run.resumed.get(), run.edits.is_some()) {
                    Ended::Keep => say_exit(view, &exit_line(status, false)),
                    how => done(&term, how),
                }
            }
            _ => {
                term.ended.set(true);
                if !exited_with(status, ATTACH_FAILED) {
                    done(&term, Ended::Close);
                }
            }
        }
    });
}

/// What the end of a command, or of a terminal editor, does to its tab (DESIGN.md, Terminal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    /// It closes.
    Close,
    /// It stays, its output and a line saying how it ended kept until its ×.
    Keep,
    /// It closes, and the file opens in accent's own editor: the shell found no such editor.
    Missing,
}

/// How a command's or an editor's end, from the wait status, leaves its tab: exit 0 closes it,
/// as does a command taken up again that had ended meanwhile, an editor's 127 is a program the
/// shell did not find, and anything else keeps it.
fn ended(status: i32, resumed: bool, editing: bool) -> Ended {
    if exited_with(status, 0) || (resumed && exited_with(status, ATTACH_FAILED)) {
        return Ended::Close;
    }
    match editing && exited_with(status, NOT_FOUND) {
        true => Ended::Missing,
        false => Ended::Keep,
    }
}

/// What a shell exits with when it finds no program of the name it was given.
const NOT_FOUND: i32 = 127;

/// Feed how a program ended on a line of its own, after a blank one: a program that ended mid-line
/// gets both.
fn say_exit(view: &vte4::Terminal, line: &str) {
    let lead = match view.cursor_position().0 {
        0 => "\r\n",
        _ => "\r\n\r\n",
    };
    view.feed(format!("{lead}{line}\r\n").as_bytes());
}

/// Attach failure exit code for an unreachable holder or shell startup failure.
/// A shell explicitly exiting 254 also retains its tab until Ctrl+W.
const ATTACH_FAILED: i32 = 254;

/// Whether SSH exited with 255, indicating connection failure rather than normal remote status.
/// A remote shell's own exit 255 is indistinguishable and may start a new held shell on reconnect.
fn link_lost(status: i32) -> bool {
    exited_with(status, 255)
}

/// How a debugged program ended, from its wait status: on a host ssh's own 255 is the link going,
/// not the program.
fn exit_line(status: i32, remote: bool) -> String {
    match (status & 0x7f, (status >> 8) & 0xff) {
        (0, 255) if remote => "[Lost the connection to host]".to_string(),
        (0, code) => format!("[Exited with code {code}]"),
        (signal, _) => format!("[Ended by signal {signal}]"),
    }
}

/// Whether a child exited with `code`. `status` is the wait status VTE passes on, as `waitpid`
/// gave it: an exit is a zero signal byte, and its code is the byte above.
fn exited_with(status: i32, code: i32) -> bool {
    status & 0x7f == 0 && (status >> 8) & 0xff == code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// VTE hands over the wait status, so ssh's 255 arrives as 255 << 8.
    #[test]
    fn only_ssh_ending_itself_is_a_lost_link() {
        assert!(link_lost(255 << 8));
        // The remote shell's own exit, clean or not.
        assert!(!link_lost(0));
        assert!(!link_lost(1 << 8));
        // Killed by a signal, whatever the byte above says.
        assert!(!link_lost((255 << 8) | 9));
    }

    /// An editor's tab goes with exit 0, stays on any other end and hands its file to accent's
    /// own editor when the shell found no such program; a command's 127 is its own to say.
    #[test]
    fn an_editor_closes_on_0_stays_on_1_and_falls_back_on_127() {
        assert_eq!(ended(0, false, true), Ended::Close);
        assert_eq!(ended(1 << 8, false, true), Ended::Keep);
        assert_eq!(ended(15, false, true), Ended::Keep);
        assert_eq!(ended(127 << 8, false, true), Ended::Missing);
        assert_eq!(ended(127 << 8, false, false), Ended::Keep);
        assert_eq!(ended(ATTACH_FAILED << 8, true, false), Ended::Close);
        assert_eq!(ended(ATTACH_FAILED << 8, false, false), Ended::Keep);
    }

    /// A debugged program's tab says how it ended, from the wait status VTE hands over.
    #[test]
    fn a_program_says_how_it_ended() {
        assert_eq!(exit_line(0, false), "[Exited with code 0]");
        assert_eq!(exit_line(3 << 8, false), "[Exited with code 3]");
        assert_eq!(exit_line(15, false), "[Ended by signal 15]");
        // On a host, ssh's own 255 is the link, not the program.
        assert_eq!(exit_line(255 << 8, true), "[Lost the connection to host]");
        assert_eq!(exit_line(255 << 8, false), "[Exited with code 255]");
    }
}
