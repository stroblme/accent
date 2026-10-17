//! The Debug Console: a terminal tab running nothing, whose transcript is fed and whose `>>> `
//! prompt is drawn by the line editor (`line`) from what VTE commits, Python's REPL in look. A
//! line sent is evaluated in the paused call (`evaluate`, context `repl`), one at a time, as
//! Python's REPL waits for each.

use super::line::{self, Did, Line};
use super::*;
use accent_api::debug::{Category, Context};
use std::collections::VecDeque;
use terminal::Term;
use vte4::TerminalExt as _;

const PROMPT: &str = ">>> ";
/// DECTCEM off: with no prompt there is nothing to type at. The reset a session starts with shows
/// it again.
const HIDE_CURSOR: &[u8] = b"\x1b[?25l";

/// The console's state, the window's for its life: a console closed and opened again keeps its
/// history.
#[derive(Default)]
pub(super) struct Console {
    line: RefCell<Line>,
    /// The session the prompt is up for, and its program, from the program's start to its end.
    session: RefCell<Option<(u64, String)>>,
    /// Whether the prompt is on the screen, to be erased before anything is printed above it.
    shown: Cell<bool>,
    /// An answer is out: the prompt waits for it, keys still going into the line, and lines
    /// sent meanwhile wait their turn.
    busy: Cell<bool>,
    queue: RefCell<VecDeque<String>>,
    /// Output after its last newline, printed once its line is whole: a program's `print`
    /// arrives as its text and then its newline.
    tail: RefCell<Option<(Category, String)>>,
}

impl Console {
    fn session(&self) -> Option<u64> {
        self.session.borrow().as_ref().map(|(id, _)| *id)
    }
}

impl App {
    /// The Debug Console's tab, while it is open.
    pub(crate) fn console(&self) -> Option<Rc<Term>> {
        self.docs()
            .into_iter()
            .find_map(|doc| doc.terminal().filter(|term| term.is_console()).cloned())
    }

    /// The console of `session`, debugging `program`: the last session's where it is, else a new
    /// one right after the program's tab and behind it, the program having the keyboard for its
    /// input.
    pub(super) fn open_console(self: &Rc<Self>, session: u64, program: &str) {
        let console = &self.debugger.console;
        console
            .session
            .replace(Some((session, doc::file_name(program).to_string())));
        console.busy.set(false);
        console.queue.borrow_mut().clear();
        console.tail.replace(None);
        match self.console() {
            Some(term) => self.begin_console(&term),
            None => {
                let place = self
                    .docs()
                    .into_iter()
                    .find_map(|doc| doc.terminal().filter(|term| term.is_program()).cloned())
                    .and_then(|program| {
                        let pane = self.pane_of(&program.page)?;
                        let at = pane.tabs.page_position(&program.page) + 1;
                        Some((pane, at))
                    });
                // Behind its pane's tab in front, as a restored tab lands.
                self.new_console(Opened::Restored, place);
            }
        }
    }

    /// Debug Console: bring it to the front with the keyboard, or open it in the active pane.
    pub(crate) fn show_debug_console(self: &Rc<Self>) {
        let Some(term) = self.console() else {
            return self.new_console(Opened::Kept, None);
        };
        self.reveal_page(&term.page);
        if let Some(pane) = self.pane_of(&term.page) {
            self.focus_document(&pane);
        }
    }

    fn new_console(self: &Rc<Self>, how: Opened, at: Option<(Rc<Pane>, i32)>) {
        let key = terminal::console_key();
        self.open_shell_at(key, terminal::Shell::Console, how, at);
        let Some(term) = self.console() else {
            return;
        };
        let weak = Rc::downgrade(self);
        term.view.connect_commit(move |_, typed, _| {
            if let Some(app) = weak.upgrade() {
                app.console_typed(typed);
            }
        });
        self.begin_console(&term);
    }

    /// A clean screen saying what is being debugged, and the prompt; nothing without a session.
    fn begin_console(&self, term: &Term) {
        let console = &self.debugger.console;
        term.view.reset(true, true);
        console.shown.set(false);
        // The rows of a drawing on the screen just cleared.
        console.line.borrow_mut().erase(columns(term));
        let program = console.session.borrow().as_ref().map(|(_, p)| p.clone());
        match program {
            Some(program) => {
                self.console_line(term, Category::Console, &format!("Debugging {program}"))
            }
            None => term.view.feed(HIDE_CURSOR),
        }
    }

    /// What VTE committed: keys typed, a paste. Read only while a session has the prompt up.
    pub(crate) fn console_typed(self: &Rc<Self>, typed: &str) {
        let console = &self.debugger.console;
        let Some(term) = self.console().filter(|_| console.session().is_some()) else {
            return;
        };
        let did = console.line.borrow_mut().key(typed);
        for did in did {
            match did {
                Did::Redraw => self.console_prompt(&term),
                Did::Submit(line) if console.busy.get() => {
                    console.queue.borrow_mut().push_back(line)
                }
                Did::Submit(line) => self.console_submit(&term, line),
                // An answer cannot be called back: what waits for it is given up instead.
                Did::Abandon(_) if console.busy.get() => console.queue.borrow_mut().clear(),
                Did::Abandon(line) => {
                    self.console_echo(&term, &format!("{line}^C"));
                    self.console_prompt(&term);
                }
            }
        }
    }

    /// The line as sent, under the prompt it was typed at, and its answer asked for: only while
    /// the program is paused, in the call selected.
    fn console_submit(self: &Rc<Self>, term: &Rc<Term>, line: String) {
        self.console_echo(term, &line);
        let console = &self.debugger.console;
        if line.trim().is_empty() {
            return self.console_prompt(term);
        }
        let (Some(session), Some(asked)) =
            (console.session(), self.debug_evaluate(line, Context::Repl))
        else {
            let paused = "Evaluates while the program is paused";
            return self.console_line(term, Category::Console, paused);
        };
        console.busy.set(true);
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let answer = asked.await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            let console = &app.debugger.console;
            if console.session() != Some(session) {
                return;
            }
            console.busy.set(false);
            let Some(term) = app.console() else {
                return console.queue.borrow_mut().clear();
            };
            match answer {
                Ok(value) if value.result.is_empty() => app.console_prompt(&term),
                Ok(value) => app.console_line(&term, Category::Stdout, value.result.trim_end()),
                // A traceback, which ends in a newline of its own.
                Err(e) => app.console_line(&term, Category::Stderr, e.to_string().trim_end()),
            }
            let next = console.queue.borrow_mut().pop_front();
            if let Some(next) = next {
                app.console_submit(&term, next);
            }
        });
    }

    /// The adapter's output, and the program's while the console evaluates: above the prompt,
    /// so what arrives while a line is typed does not break into it.
    pub(super) fn console_output(&self, category: Category, text: &str) {
        let Some(term) = self.console() else {
            return;
        };
        let console = &self.debugger.console;
        let mut out = String::new();
        let mut held = console.tail.take();
        // Lines of two kinds are not joined.
        if let Some((was, part)) = held.take_if(|(was, _)| *was != category) {
            out.push_str(&styled(was, &format!("{part}\n")));
        }
        let mut text = held.map(|(_, part)| part).unwrap_or_default() + text;
        let tail = text.split_off(text.rfind('\n').map_or(0, |at| at + 1));
        if !tail.is_empty() {
            console.tail.replace(Some((category, tail)));
        }
        out.push_str(&styled(category, &text));
        if !out.is_empty() {
            self.console_feed(&term, &out);
        }
    }

    /// The session is over: the console says how, and leaves no prompt.
    pub(super) fn console_end(&self, session: u64, code: Option<i64>) {
        let console = &self.debugger.console;
        if console.session() != Some(session) {
            return;
        }
        console.session.replace(None);
        let Some(term) = self.console() else {
            return;
        };
        let ended = match code {
            Some(code) => format!("Session ended (exit code {code})"),
            None => "Session ended".to_string(),
        };
        self.console_line(&term, Category::Console, &ended);
        term.view.feed(HIDE_CURSOR);
    }

    /// One whole line of the console's own, after any output still waiting for its newline.
    fn console_line(&self, term: &Term, category: Category, line: &str) {
        let mut out = match self.debugger.console.tail.take() {
            Some((was, part)) => styled(was, &format!("{part}\n")),
            None => String::new(),
        };
        out.push_str(&styled(category, &format!("{line}\n")));
        self.console_feed(term, &out);
    }

    /// Whole lines printed above the prompt.
    fn console_feed(&self, term: &Term, lines: &str) {
        self.console_erase(term);
        term.view.feed(lines.as_bytes());
        self.console_prompt(term);
    }

    /// The prompt and the line, unless there is no session or an answer is out.
    fn console_prompt(&self, term: &Term) {
        let console = &self.debugger.console;
        if console.session().is_none() || console.busy.get() {
            return;
        }
        let drawn = console.line.borrow_mut().draw(PROMPT, columns(term));
        term.view.feed(drawn.as_bytes());
        console.shown.set(true);
    }

    /// The prompt line as it was given up or sent, the cursor on the next line.
    fn console_echo(&self, term: &Term, line: &str) {
        self.console_erase(term);
        term.view.feed(format!("{PROMPT}{line}\r\n").as_bytes());
    }

    fn console_erase(&self, term: &Term) {
        let console = &self.debugger.console;
        if console.shown.replace(false) {
            let erased = console.line.borrow_mut().erase(columns(term));
            term.view.feed(erased.as_bytes());
        }
    }
}

/// The terminal's width now, which VTE has rewrapped the prompt to.
fn columns(term: &Term) -> u16 {
    u16::try_from(term.view.column_count()).unwrap_or(u16::MAX)
}

/// `text` as shown, with nothing a terminal would act on (a title, a cursor move, a query it
/// would answer as if typed at the prompt) and its newlines as a terminal takes them, styled for
/// `category`: stderr in the palette's red, what must be seen bold, the adapter's and the
/// console's own words dim.
fn styled(category: Category, text: &str) -> String {
    let text = line::clean(text).replace('\n', "\r\n");
    let sgr = match category {
        _ if text.is_empty() => return text,
        Category::Stdout => return text,
        Category::Stderr => "\x1b[31m".to_string(),
        Category::Important => "\x1b[1m".to_string(),
        Category::Console => dim(),
    };
    format!("{sgr}{text}\x1b[0m")
}

/// The terminal's ink at 0.6 over its page, as a quote's is. Not SGR 2: VTE dims towards black,
/// which on a light page is a darker ink. A colour, not a palette entry, so a line printed before
/// the theme changes keeps its grey, which reads on either page.
fn dim() -> String {
    let dark = adw::StyleManager::default().is_dark();
    let parse = |colour| gdk::RGBA::parse(colour).ok();
    let (Some(ink), Some(page)) = (
        parse(crate::theme::view_fg(dark)),
        parse(crate::theme::view_bg(dark)),
    ) else {
        return String::new();
    };
    let dim = crate::theme::over(crate::theme::at(ink, 0.6), page);
    let byte = |channel: f32| (channel * 255.0).round() as u8;
    let (r, g, b) = (byte(dim.red()), byte(dim.green()), byte(dim.blue()));
    format!("\x1b[38;2;{r};{g};{b}m")
}
