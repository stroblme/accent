//! Links in a shell's output, and the context menu that offers them.
//!
//! An `http`, `https` or `mailto` URL goes to the desktop. A file path opens in an accent tab, and
//! lights up only while its file is there to open: VTE lights every match of a pattern it holds,
//! so the path pattern is only asked under the pointer ([`candidate`]), and a path found there
//! whose file is there is handed to VTE as a pattern of its own ([`light`]).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use accent_api::Vault;
use gtk::glib::translate::{FromGlibPtrFull, ToGlibPtr};
use gtk::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use vte4::TerminalExt;

use super::{Shell, Term, dir_of};

/// Local action group for pointer-targeted URLs, which have no window shortcut or palette action.
const GROUP: &str = "link";
/// What counts as a link on the screen: an `http`, `https` or `mailto` URL, stopping before the
/// punctuation that ends a sentence rather than swallowing it.
const LINK: &str = r"(?:https?://|mailto:)[^\s<>\x22'`]*[^\s<>\x22'`.,:;!?)\]}]";
/// A file path as a compiler, grep or Claude Code prints one, as plain text: Claude Code 2.1.330's
/// bundle writes its tools' paths and its `path:line` so, and hands only URLs and a markdown
/// link's target to OSC 8 ([`osc8_path`]). It is absolute, from home (`~/`), has a folder in
/// it or is a file name with an extension, and may end in `:line` or `:line:col`. Its parts are
/// anything but spaces, quotes, brackets, the shell's punctuation and the `:` before a line.
/// It starts a word, so neither a URL's path nor another path's tail is one, and a full stop,
/// a comma or a bracket after it ends the sentence, not the name.
const PATH: &str = concat!(
    r#"(?<![^\s'"`(\[{<=])"#,
    r#"(?:~?/[^\s/\\'"`<>()\[\]{}|;,:=*?!$]+(?:/[^\s/\\'"`<>()\[\]{}|;,:=*?!$]+)*"#,
    r#"|[^\s/\\'"`<>()\[\]{}|;,:=*?!$]+(?:/[^\s/\\'"`<>()\[\]{}|;,:=*?!$]+)+"#,
    r#"|[^\s/\\'"`<>()\[\]{}|;,:=*?!$]+\.\w+)(?<!\.)(?::\d+(?::\d+)?)?"#,
    r#"(?![^\s'"`)\]}>,;:.!?])"#,
);
/// What may come before a path, [`PATH`]'s first part: the start of a line, a space, a quote,
/// an opening bracket or `=`.
const START: &str = r#"(?<![^\s'"`(\[{<=])"#;
/// What may follow a path, [`PATH`]'s last part: the end of a line, a space, a closing bracket
/// or quote, or punctuation.
const END: &str = r#"(?![^\s'"`)\]}>,;:.!?])"#;
/// `PCRE2_MULTILINE`. vte4 re-exports no PCRE2 flags, so the value is written out here; a match
/// has to be able to end at a wrapped line rather than only at the end of the buffer.
const PCRE2_MULTILINE: u32 = 0x0000_0400;
/// How long a host's answer about a path stands: a path is often printed as its file is written.
const FRESH: Duration = Duration::from_secs(5);
/// The holder is asked where a shell without OSC 7 is at most this often.
const ASK_AGAIN: Duration = Duration::from_secs(2);

/// A file a shell's output names, which Ctrl+click opens at its line and column.
#[derive(Clone, Debug, PartialEq)]
pub struct FileLink {
    /// Absolute, on this machine or on the remote vault's host, as `host` says. A host's `~/`
    /// path stays as printed until the host has said where it is ([`known`]).
    pub path: PathBuf,
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub host: bool,
}

/// What opens a file a link names, the window's.
type Open = Rc<dyn Fn(FileLink)>;

/// Where a shell's paths are looked up.
enum Disk {
    /// This machine's.
    Local,
    /// The remote vault's host, through the vault, which reads its own files and those a shell
    /// named there.
    Vault(Arc<Vault>),
}

/// A shell's file links between presses.
#[derive(Default)]
pub(super) struct Files {
    /// Where paths are looked up and what opens one, once the window has said ([`on_file_link`]).
    to: RefCell<Option<(Disk, Open)>>,
    pattern: OnceCell<Option<vte4::Regex>>,
    /// The path under the pointer when last looked, so a motion along it asks nothing again.
    hovered: RefCell<Option<String>>,
    /// VTE's tag for the one path lit.
    lit: Cell<Option<i32>>,
    /// A host's answers, by the path asked, and when each came: the file's absolute path there,
    /// or `None` for a folder or nothing.
    known: RefCell<HashMap<PathBuf, (Option<PathBuf>, Instant)>>,
    /// When the holder was last asked where the shell is.
    asked: Cell<Option<Instant>>,
    /// The file the context menu was opened over, which its Open Link opens.
    menu: RefCell<Option<FileLink>>,
}

/// What the pointer was over when the context menu opened.
enum Target {
    Url(String),
    File(FileLink),
}

/// Install terminal menus and safe output-link handling.
/// Only `launchable` schemes reach the desktop; action parameters retain the clicked URL
/// even if later pointer motion changes the hover target.
pub(super) fn install_links(term: &Rc<Term>) {
    let view = &term.view;
    // OSC 8, for the programs that mark their own links instead of printing a bare URL.
    view.set_allow_hyperlink(true);
    match vte4::Regex::for_match(LINK, PCRE2_MULTILINE) {
        Ok(regex) => {
            let tag = view.match_add_regex(&regex, 0);
            view.match_set_cursor_name(tag, "pointer");
        }
        Err(e) => tracing::warn!("terminal links are off, the pattern did not compile: {e}"),
    }
    view.insert_action_group(GROUP, Some(&link_actions(term)));

    let menu = gio::Menu::new();
    fill_menu(&menu, None, term.share_state());
    view.set_context_menu_model(Some(&menu));

    // Capture phase, so the model is rebuilt before VTE reads it to pop the menu up.
    let secondary = gtk::GestureClick::new();
    secondary.set_button(gdk::BUTTON_SECONDARY);
    secondary.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(term);
    secondary.connect_pressed(move |_, _, x, y| {
        let Some(term) = weak.upgrade() else { return };
        let target = target_at(&term, x, y);
        fill_menu(&menu, target.as_ref(), term.share_state());
        if term.share_vault.is_some() {
            let menu = menu.clone();
            term.ask(move |term| fill_menu(&menu, target.as_ref(), term.share_state()));
        }
    });
    view.add_controller(secondary);

    // Ctrl and the primary button, which is what GNOME Terminal asks for and what a PDF link here
    // already asks for. A plain click cannot have it: VTE claims the sequence for its own
    // selection, which cancels any gesture of ours before the release, and a press that both
    // starts a selection and launches a browser is the wrong gesture in any case.
    let primary = gtk::GestureClick::new();
    primary.set_button(gdk::BUTTON_PRIMARY);
    primary.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(term);
    primary.connect_pressed(move |gesture, _, x, y| {
        let ctrl = gesture
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK);
        let Some(term) = weak.upgrade().filter(|_| ctrl) else {
            return;
        };
        match link_at(&term.view, x, y) {
            Some(uri) => launch(&uri),
            None => open_file_at(&term, x, y),
        }
    });
    view.add_controller(primary);

    // On the scroller round the view, in the capture phase, which GTK runs before any handler of
    // the view's, so the path under the pointer is lit before VTE looks for what to light: on the
    // view itself VTE went first, and a path lit only on the motion after.
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(term);
    motion.connect_motion(move |motion, x, y| {
        let Some(term) = weak.upgrade() else { return };
        let at = graphene::Point::new(x as f32, y as f32);
        let at = motion
            .widget()
            .and_then(|w| w.compute_point(&term.view, &at));
        if let Some(at) = at {
            hover(&term, f64::from(at.x()), f64::from(at.y()));
        }
    });
    let weak = Rc::downgrade(term);
    motion.connect_leave(move |_| {
        if let Some(term) = weak.upgrade() {
            term.files.hovered.replace(None);
        }
    });
    if let Some(scroller) = view.parent() {
        scroller.add_controller(motion);
    }
}

/// Give a shell's file links their disk and what opens one: this machine's for a shell here, the
/// remote vault's host for a shell held there, and none for a host that is not the vault's,
/// whose files the window has no way to look up.
pub fn on_file_link(
    term: &Rc<Term>,
    vault: Option<&Arc<Vault>>,
    open: impl Fn(FileLink) + 'static,
) {
    let host = vault.filter(|vault| vault.is_remote()).cloned();
    let disk = match &term.shell {
        Shell::Local(_) => Some(Disk::Local),
        Shell::Program(program) if program.link.is_none() => Some(Disk::Local),
        Shell::Program(_) => host.map(Disk::Vault),
        Shell::Remote { .. } => host
            .filter(|vault| {
                term.shell
                    .same_host(vault.remote().map(|remote| remote.url()))
            })
            .map(Disk::Vault),
        Shell::Console => None,
    };
    *term.files.to.borrow_mut() = disk.map(|disk| (disk, Rc::new(open) as Open));
}

/// What the menu holds: the clipboard, then the tab, then the link under the pointer when there
/// is one. No Select All — VTE binds nothing for it — and no Split, which is a tab gesture the
/// tab strip already offers.
fn fill_menu(menu: &gio::Menu, target: Option<&Target>, shared: Option<bool>) {
    menu.remove_all();
    let clipboard = gio::Menu::new();
    for action in ["win.terminal-copy", "win.terminal-paste"] {
        clipboard.append(Some(crate::actions::label_of(action)), Some(action));
    }
    menu.append_section(None, &clipboard);
    let tab = gio::Menu::new();
    tab.append(
        Some(crate::actions::label_of("win.terminal")),
        Some("win.terminal"),
    );
    if let Some(shared) = shared {
        let action = if shared {
            "win.terminal-revoke"
        } else {
            "win.terminal-share"
        };
        tab.append(Some(crate::actions::label_of(action)), Some(action));
    }
    tab.append(
        Some(crate::actions::label_of("win.close-tab")),
        Some("win.close-tab"),
    );
    menu.append_section(None, &tab);
    let (open, address) = match target {
        None => return,
        Some(Target::Url(uri)) => (Some(uri.to_variant()), uri.clone()),
        // A file's Open Link opens the file the menu was opened over, a path not being a URL.
        Some(Target::File(file)) => (None, file.path.to_string_lossy().into_owned()),
    };
    let links = gio::Menu::new();
    let action = format!(
        "{GROUP}.{}",
        if open.is_some() { "open" } else { "open-file" }
    );
    let item = gio::MenuItem::new(Some("Open Link"), None);
    item.set_action_and_target_value(Some(&action), open.as_ref());
    links.append_item(&item);
    let item = gio::MenuItem::new(Some("Copy Link Address"), None);
    item.set_action_and_target_value(Some(&format!("{GROUP}.copy")), Some(&address.to_variant()));
    links.append_item(&item);
    menu.append_section(None, &links);
}

/// The link items' actions, a URL's and the copy taking the URL or the path as their parameter,
/// as the tree's row menu does with a path: a detailed-action string would have to quote it.
fn link_actions(term: &Rc<Term>) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let open = gio::SimpleAction::new("open", Some(glib::VariantTy::STRING));
    open.connect_activate(|_, target| {
        if let Some(uri) = target.and_then(|t| t.str()) {
            launch(uri);
        }
    });
    group.add_action(&open);
    let open_file = gio::SimpleAction::new("open-file", None);
    let weak = Rc::downgrade(term);
    open_file.connect_activate(move |_, _| {
        let Some(term) = weak.upgrade() else { return };
        let file = term.files.menu.borrow().clone();
        if let (Some(file), Some(open)) = (file, opener(&term)) {
            open(file);
        }
    });
    group.add_action(&open_file);
    let copy = gio::SimpleAction::new("copy", Some(glib::VariantTy::STRING));
    copy.connect_activate(glib::clone!(
        #[weak(rename_to = view)]
        term.view,
        move |_, target| {
            if let Some(uri) = target.and_then(|t| t.str()) {
                view.clipboard().set_text(uri);
            }
        }
    ));
    group.add_action(&copy);
    group
}

/// Return the permitted URL at `(x, y)`, preferring OSC 8 links over regex matches.
/// Filter before building the menu so unsupported schemes never appear.
fn link_at(view: &vte4::Terminal, x: f64, y: f64) -> Option<String> {
    view.check_hyperlink_at(x, y)
        .or_else(|| view.check_match_at(x, y).0)
        .map(|uri| uri.to_string())
        .filter(|uri| launchable(uri))
}

/// What the menu offers at `(x, y)`: a URL, or a file already known to be there, since the menu
/// cannot wait for a host to answer.
fn target_at(term: &Rc<Term>, x: f64, y: f64) -> Option<Target> {
    if let Some(uri) = link_at(&term.view, x, y) {
        return Some(Target::Url(uri));
    }
    let file = file_text_at(term, x, y)
        .and_then(|text| file_link(term, &text))
        .and_then(|file| known(term, file).flatten());
    term.files.menu.replace(file.clone());
    file.map(Target::File)
}

/// Ctrl+click on a file at `(x, y)`: looked up again, as the screen may have changed since it lit.
fn open_file_at(term: &Rc<Term>, x: f64, y: f64) {
    let Some(text) = file_text_at(term, x, y) else {
        return;
    };
    let weak = Rc::downgrade(term);
    find(term, &text, move |file| {
        if let Some(open) = weak.upgrade().and_then(|term| opener(&term)) {
            open(file);
        }
    });
}

/// The pointer over `(x, y)`: the path there lit if its file is there, and nothing else lit.
fn hover(term: &Rc<Term>, x: f64, y: f64) {
    if term.files.to.borrow().is_none() {
        return;
    }
    let text = candidate(term, x, y);
    if *term.files.hovered.borrow() == text {
        return;
    }
    term.files.hovered.replace(text.clone());
    if let Some(tag) = term.files.lit.take() {
        term.view.match_remove(tag);
    }
    let Some(text) = text else {
        return;
    };
    let weak = Rc::downgrade(term);
    let lit = text.clone();
    find(term, &text, move |_| {
        let Some(term) = weak.upgrade() else { return };
        if term.files.hovered.borrow().as_deref() == Some(lit.as_str()) {
            light(&term, &lit);
        }
    });
}

/// Light `text`, the path under the pointer, wherever it stands on the screen between the same
/// edges, VTE lighting a match only where the pointer is.
fn light(term: &Term, text: &str) {
    match vte4::Regex::for_match(&literal(text), PCRE2_MULTILINE) {
        Ok(regex) => {
            if let Some(tag) = term.files.lit.take() {
                term.view.match_remove(tag);
            }
            let tag = term.view.match_add_regex(&regex, 0);
            term.view.match_set_cursor_name(tag, "pointer");
            term.files.lit.set(Some(tag));
        }
        Err(e) => tracing::warn!("cannot light {text}: {e}"),
    }
}

/// The path pattern's match under `(x, y)`, or an OSC 8 `file:` link's path there.
fn file_text_at(term: &Term, x: f64, y: f64) -> Option<String> {
    match term.view.check_hyperlink_at(x, y) {
        Some(uri) => osc8_path(&uri, matches!(term.shell, Shell::Remote { .. })),
        None => candidate(term, x, y),
    }
}

/// The path pattern's match under `(x, y)`, asked of VTE without being given to it to light.
fn candidate(term: &Term, x: f64, y: f64) -> Option<String> {
    let pattern = term
        .files
        .pattern
        .get_or_init(|| vte4::Regex::for_match(PATH, PCRE2_MULTILINE).ok())
        .as_ref()?;
    let mut regexes = [pattern.to_glib_none().0];
    let mut found = std::ptr::null_mut();
    // SAFETY: one live regex in a one-long array and one slot for VTE to fill with a string it
    // hands over, or leave NULL. vte4's own `check_regex_simple_at` passes its `&[&Regex]` one
    // indirection too deep, and reads a NULL answer as strings.
    let found: Option<glib::GString> = unsafe {
        vte4::ffi::vte_terminal_check_regex_simple_at(
            term.view.to_glib_none().0,
            x,
            y,
            regexes.as_mut_ptr(),
            1,
            0,
            &mut found,
        );
        FromGlibPtrFull::from_glib_full(found)
    };
    found.map(|text| text.to_string())
}

/// The path an OSC 8 link names, as Claude Code may write a markdown link's target: a `file:` URL
/// or a bare path, with a `#L12` naming its line or not. A URL names one on the shell's machine,
/// on a host whatever host it names and here only this one (`dir_of`), as OSC 7 is read.
fn osc8_path(uri: &str, remote: bool) -> Option<String> {
    let (target, line) = match uri.rsplit_once('#') {
        Some((target, fragment)) => {
            let line = fragment
                .strip_prefix('L')
                .and_then(|n| n.parse::<u32>().ok());
            (target, line)
        }
        None => (uri, None),
    };
    let scheme = target
        .split_once(':')
        .map(|(scheme, _)| scheme)
        .filter(|scheme| {
            scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+.-".contains(c))
        });
    let path = match scheme {
        None => target.to_string(),
        Some(scheme) if scheme.eq_ignore_ascii_case("file") => {
            let path = match remote {
                true => glib::filename_from_uri(target).ok().map(|(path, _)| path),
                false => dir_of(target, &glib::host_name()),
            };
            path?.to_str()?.to_string()
        }
        Some(_) => return None,
    };
    Some(match line {
        Some(line) => format!("{path}:{line}"),
        None => path,
    })
}

/// The file `text` names where its shell is, if the window has said where to look.
fn file_link(term: &Term, text: &str) -> Option<FileLink> {
    let host = matches!(term.files.to.borrow().as_ref()?.0, Disk::Vault(_));
    let (path, line, column) = place(text);
    // A host's home is not known here, so a `~/` path there is the host's to resolve.
    let path = match host && path.starts_with("~/") {
        true => PathBuf::from(path),
        false => resolve(path, term.link_dir().as_deref(), &glib::home_dir())?,
    };
    Some(FileLink {
        path,
        line,
        column,
        host,
    })
}

/// `file` where it is there, or `Some(None)` where it is not, if that is known without asking a
/// host; a host's file has the path the host answered with.
fn known(term: &Term, file: FileLink) -> Option<Option<FileLink>> {
    match &term.files.to.borrow().as_ref()?.0 {
        Disk::Local => Some(file.path.is_file().then_some(file)),
        Disk::Vault(_) => {
            let answers = term.files.known.borrow();
            let (there, _) = answers
                .get(&file.path)
                .filter(|(_, when)| when.elapsed() < FRESH)?;
            Some(there.clone().map(|path| FileLink { path, ..file }))
        }
    }
}

/// Look `text` up and call `found` with its file once that is known to be there: at once on this
/// machine, and on a host once the vault's [`shell_file`](Vault::shell_file) has answered, the
/// answer kept a while. A relative path in a shell that reports no folder (no OSC 7) waits for
/// the holder to say where the shell is first.
fn find(term: &Rc<Term>, text: &str, found: impl FnOnce(FileLink) + 'static) {
    let path = place(text).0;
    let relative = !path.starts_with('/') && !path.starts_with("~/");
    let stale = term
        .files
        .asked
        .get()
        .is_none_or(|when| when.elapsed() > ASK_AGAIN);
    if relative
        && stale
        && term.told_dir().is_none()
        && term.shell.held().is_some()
        && !term.lost.get()
    {
        term.files.asked.set(Some(Instant::now()));
        let (text, found) = (text.to_string(), Cell::new(Some(found)));
        return term.ask(move |term| {
            if let Some(found) = found.take() {
                look(term, &text, found);
            }
        });
    }
    look(term, text, found);
}

/// [`find`] once the shell's folder is as known as it will be.
fn look(term: &Rc<Term>, text: &str, found: impl FnOnce(FileLink) + 'static) {
    let Some(file) = file_link(term, text) else {
        return;
    };
    match known(term, file.clone()) {
        Some(Some(file)) => return found(file),
        Some(None) => return,
        None => {}
    }
    let vault = match term.files.to.borrow().as_ref() {
        Some((Disk::Vault(vault), _)) => vault.clone(),
        _ => return,
    };
    let Some(asked) = file.path.to_str().map(str::to_string) else {
        return;
    };
    let weak = Rc::downgrade(term);
    glib::spawn_future_local(async move {
        let root = vault.root().to_path_buf();
        let there = crate::work::off_thread("look for a terminal's file", move || {
            vault.shell_file(&asked)
        })
        .await
        .and_then(Result::ok)
        .flatten()
        .map(PathBuf::from)
        // From outside the vault only text opens, as a language server's dependency does there.
        .filter(|path| {
            path.starts_with(&root)
                || matches!(
                    crate::doc::kind_of(&path.to_string_lossy()),
                    crate::doc::Kind::Note | crate::doc::Kind::Text
                )
        });
        let Some(term) = weak.upgrade() else { return };
        let answer = (there.clone(), Instant::now());
        term.files
            .known
            .borrow_mut()
            .insert(file.path.clone(), answer);
        if let Some(path) = there {
            found(FileLink { path, ..file });
        }
    });
}

/// What opens a file, once the window has said.
fn opener(term: &Term) -> Option<Open> {
    Some(term.files.to.borrow().as_ref()?.1.clone())
}

impl Term {
    /// The folder the shell last reported with OSC 7: this machine's for a shell here, and on a
    /// host the host's, whatever name the host gives itself.
    fn told_dir(&self) -> Option<PathBuf> {
        let uri = self.view.current_directory_uri()?;
        match &self.shell {
            Shell::Remote { .. } => glib::filename_from_uri(&uri).ok().map(|(dir, _)| dir),
            _ => dir_of(&uri, &glib::host_name()),
        }
    }

    /// The folder a relative path in the output is read from: the one OSC 7 last reported, else
    /// the one the holder last saw the shell in, else the one it started in.
    fn link_dir(&self) -> Option<PathBuf> {
        let held = self.holder.borrow().dir.clone().map(PathBuf::from);
        let start = match &self.shell {
            Shell::Local(start) => start.clone(),
            Shell::Remote { at, .. } => at.path.clone(),
            Shell::Program(program) => return Some(PathBuf::from(&program.cwd)),
            Shell::Console => return None,
        };
        Some(self.told_dir().or(held).unwrap_or(start))
    }
}

#[cfg(feature = "bench")]
impl Term {
    /// The lit path under `(x, y)`, for `ACCENT_BENCH_LINKS`.
    pub fn lit_at(&self, x: f64, y: f64) -> Option<String> {
        let (text, tag) = self.view.check_match_at(x, y);
        text.filter(|_| self.files.lit.get() == Some(tag))
            .map(|text| text.to_string())
    }
}

/// `text` without its `:line` or `:line:col`, and the two.
fn place(text: &str) -> (&str, Option<u32>, Option<u32>) {
    let number = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| s.parse().ok())
            .flatten()
    };
    let Some((rest, last)) = text.rsplit_once(':').filter(|(_, n)| number(n).is_some()) else {
        return (text, None, None);
    };
    match rest
        .rsplit_once(':')
        .and_then(|(path, n)| Some((path, number(n)?)))
    {
        Some((path, line)) => (path, Some(line), number(last)),
        None => (rest, number(last), None),
    }
}

/// The absolute path `path` names from `dir` and `home`, its `.` and `..` read as written: the
/// files may be on a host, which is not this machine's to ask.
fn resolve(path: &str, dir: Option<&Path>, home: &Path) -> Option<PathBuf> {
    let joined = match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None if path.starts_with('/') => PathBuf::from(path),
        None => dir?.join(path),
    };
    let mut clean = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::ParentDir => {
                clean.pop();
            }
            Component::CurDir => {}
            part => clean.push(part),
        }
    }
    clean.is_absolute().then_some(clean)
}

/// The pattern that lights `text` between the edges a path has, every ASCII mark escaped.
fn literal(text: &str) -> String {
    let mut pattern = START.to_string();
    for c in text.chars() {
        if c.is_ascii_punctuation() {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern + END
}

/// Hand a URL to the desktop.
fn launch(uri: &str) {
    if let Err(e) = gio::AppInfo::launch_default_for_uri(uri, gio::AppLaunchContext::NONE) {
        tracing::warn!("cannot open {uri}: {e}");
    }
}

/// Allow only approved URL schemes from untrusted terminal output.
/// Reject file, JavaScript and other registered helper schemes regardless of regex or OSC 8 claims.
fn launchable(uri: &str) -> bool {
    let Some((scheme, rest)) = uri.split_once(':') else {
        return false;
    };
    !rest.is_empty()
        && matches!(
            scheme.to_ascii_lowercase().as_str(),
            "http" | "https" | "mailto"
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_menu_offers_only_the_current_sharing_action() {
        let menu = gio::Menu::new();
        let actions = |menu: &gio::Menu| {
            let tab = menu.item_link(1, "section").unwrap();
            (0..tab.n_items())
                .filter_map(|i| tab.item_attribute_value(i, "action", None))
                .filter_map(|action| action.str().map(str::to_string))
                .collect::<Vec<_>>()
        };
        fill_menu(&menu, None, None);
        assert_eq!(actions(&menu), ["win.terminal", "win.close-tab"]);
        fill_menu(&menu, None, Some(false));
        assert_eq!(
            actions(&menu),
            ["win.terminal", "win.terminal-share", "win.close-tab"]
        );
        fill_menu(&menu, None, Some(true));
        assert_eq!(
            actions(&menu),
            ["win.terminal", "win.terminal-revoke", "win.close-tab"]
        );
    }

    /// The shell's output is untrusted, so this is the one that has to be a list.
    #[test]
    fn only_a_browser_or_a_mail_client_is_ever_launched() {
        assert!(launchable("https://example.org/x"));
        assert!(launchable("http://example.org"));
        assert!(launchable("mailto:someone@example.org"));
        // Case is the URL's, not ours.
        assert!(launchable("HTTPS://example.org"));
        // Anything a shell could print to reach the filesystem or run something.
        assert!(!launchable("file:///etc/passwd"));
        assert!(!launchable("javascript:alert(1)"));
        assert!(!launchable("ssh://box/x"));
        assert!(!launchable("smb://share"));
        // Not a URL at all.
        assert!(!launchable("https:"));
        assert!(!launchable("example.org"));
    }

    /// Each path the pattern finds in `line`, as VTE would light it, bracketed.
    fn paths(line: &str) -> String {
        const GLOBAL: u32 = 0x0000_0100;
        let regex = vte4::Regex::for_match(PATH, PCRE2_MULTILINE).unwrap();
        regex.substitute(line, "[$0]", GLOBAL).unwrap().to_string()
    }

    /// The shapes Claude Code, compilers and grep print a path in, and the punctuation that ends a
    /// sentence after one.
    #[test]
    fn paths_are_found_as_tools_print_them() {
        assert_eq!(
            paths("⏺ Read(apps/gtk/src/a.rs)"),
            "⏺ Read([apps/gtk/src/a.rs])"
        );
        assert_eq!(paths("Update(/home/me/x.md)"), "Update([/home/me/x.md])");
        assert_eq!(paths("see a/b.rs:12:5."), "see [a/b.rs:12:5].");
        assert_eq!(paths("in `links.rs:42`, then"), "in `[links.rs:42]`, then");
        assert_eq!(paths("--> src/main.rs:10:5"), "--> [src/main.rs:10:5]");
        assert_eq!(paths("src/a.rs:3:    let x"), "[src/a.rs:3]:    let x");
        assert_eq!(
            paths("~/notes/a.md and ../b/c"),
            "[~/notes/a.md] and [../b/c]"
        );
        assert_eq!(paths("PATH=/usr/bin"), "PATH=[/usr/bin]");
        // A sentence's full stop and a URL's path are not a path's.
        assert_eq!(paths("Cargo.toml."), "[Cargo.toml].");
        assert_eq!(
            paths("https://example.org/a/b.rs"),
            "https://example.org/a/b.rs"
        );
        assert_eq!(paths("a word, no path"), "a word, no path");
    }

    #[test]
    fn a_line_and_column_come_off_the_end() {
        assert_eq!(place("a/b.rs:12:5"), ("a/b.rs", Some(12), Some(5)));
        assert_eq!(place("a/b.rs:12"), ("a/b.rs", Some(12), None));
        assert_eq!(place("a/b.rs"), ("a/b.rs", None, None));
        assert_eq!(place("a/b.rs:"), ("a/b.rs:", None, None));
    }

    /// A relative path is read from the shell's folder and `~/` from home; `..` is taken as
    /// written.
    #[test]
    fn paths_resolve_from_the_shell_folder() {
        let (dir, home) = (Path::new("/v/src"), Path::new("/home/me"));
        let at = |path| resolve(path, Some(dir), home);
        assert_eq!(at("a.rs"), Some(PathBuf::from("/v/src/a.rs")));
        assert_eq!(at("./a/../b.rs"), Some(PathBuf::from("/v/src/b.rs")));
        assert_eq!(at("../x.md"), Some(PathBuf::from("/v/x.md")));
        assert_eq!(at("~/n.md"), Some(PathBuf::from("/home/me/n.md")));
        assert_eq!(at("/etc/hosts"), Some(PathBuf::from("/etc/hosts")));
        assert_eq!(resolve("a.rs", None, home), None);
    }

    /// What lights a path matches it alone, between the same edges.
    #[test]
    fn a_lit_path_is_matched_literally() {
        assert!(PATH.starts_with(START) && PATH.ends_with(END));
        const GLOBAL: u32 = 0x0000_0100;
        let regex = vte4::Regex::for_match(&literal("a/b.rs:1"), PCRE2_MULTILINE).unwrap();
        let lit = |line| regex.substitute(line, "[$0]", GLOBAL).unwrap().to_string();
        assert_eq!(
            lit("x a/b.rs:1, a/b.rs:12 xa/b.rs:1"),
            "x [a/b.rs:1], a/b.rs:12 xa/b.rs:1"
        );
        assert_eq!(lit("a/bxrs:1"), "a/bxrs:1");
    }

    #[test]
    fn an_osc8_file_link_names_its_path() {
        assert_eq!(
            osc8_path("file:///v/a%20b.rs", false).as_deref(),
            Some("/v/a b.rs")
        );
        assert_eq!(osc8_path("file://elsewhere/v/a.rs", false), None);
        assert_eq!(
            osc8_path("file://elsewhere/v/a.rs", true).as_deref(),
            Some("/v/a.rs")
        );
        assert_eq!(osc8_path("https://example.org/a.rs", true), None);
        // A markdown link's target as Claude Code writes it, the line after `#L`.
        assert_eq!(
            osc8_path("file:///v/a.rs#L12", false).as_deref(),
            Some("/v/a.rs:12")
        );
        assert_eq!(
            osc8_path("src/a.rs#L3", false).as_deref(),
            Some("src/a.rs:3")
        );
        assert_eq!(osc8_path("src/a.md#top", true).as_deref(), Some("src/a.md"));
        assert_eq!(osc8_path("javascript:alert(1)", false), None);
    }
}
