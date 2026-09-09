//! Command palette, file switcher and vault switcher: one dialog, four modes.
//!
//! The caller says which mode the palette opens in, so `Ctrl+P` and `Ctrl+Shift+P` both land on an
//! empty entry that is already searching the right thing. A typed leading `>` or `#` still switches
//! mode mid-search, VS Code style. Command mode is also the app's shortcuts reference
//! (DESIGN.md "Keyboard": there is no shortcuts window until the libadwaita floor reaches 1.8), so
//! every command row carries its accelerator.
//!
//! Opening must be instant, so file mode goes up showing the most recently modified notes (one
//! indexed query, no matching at all) and only pulls the full note list the first time the user
//! types something. Keystrokes are debounced, so holding a key down cannot queue up one full match
//! per character.

use crate::start;
use crate::widgets::{Debounce, status_page};
use accent_core::path::{basename, parent_dir};
use adw::prelude::*;
use gtk::glib;
use gtk::{gdk, gio, pango};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The two rows that close the Open Recent list: the ways to a vault that is not in it.
const OPENERS: [&str; 2] = ["app.open-vault", "app.open-remote"];

/// Beyond this the list stops being scannable and nucleo's single-threaded matcher starts to show.
const MAX_RESULTS: usize = 200;
/// Long enough to swallow a burst of keystrokes, short enough to feel immediate.
const DEBOUNCE: Duration = Duration::from_millis(50);
/// How many rows Page Up and Page Down move by: what the 420 px dialog shows at once.
const PAGE: u32 = 10;

/// One thing the palette can offer.
pub enum Item {
    /// A file to open, by vault-relative path. Any file, not only a note: a source file
    /// has to be reachable by name too.
    File(String),
    /// A `GAction` on the window, with the label and accelerator to show.
    Command {
        action: String,
        label: String,
        /// Every accelerator bound to it, first one shown. The whole list, because rebinding
        /// replaces it and a clash has to be found across all of them, not just the first.
        accels: Vec<String>,
        /// Position in the window's recently-run list, if it is in it at all. Lower is newer.
        recent: Option<usize>,
    },
    /// A tag to filter by.
    Tag(String),
    /// A recent vault to switch to, by the key it is stored under: a canonical path, or an
    /// `ssh://` address for one on another machine.
    Vault(String),
}

impl Item {
    /// The text the palette matches against and shows first in the row.
    fn text(&self) -> &str {
        match self {
            Item::File(rel) => rel,
            Item::Command { label, .. } => label,
            Item::Tag(tag) => tag,
            Item::Vault(key) => key,
        }
    }
}

/// Where the three modes get their rows. The two loaders are called at most once per dialog.
pub struct Sources {
    /// Shown in file mode until the first keystroke; never matched against. It is the window's
    /// own list followed by the index's modification-time one, which is a browse page rather than
    /// use order — a file a sync or a checkout touched is not a file the user opened.
    pub recent: Vec<String>,
    /// What this window opened, newest first. This is the recency a typed query is ranked by, so
    /// it holds nothing but the user's own moves.
    pub mru: Vec<String>,
    pub load_files: Box<dyn Fn() -> Vec<String>>,
    pub commands: Vec<Item>,
    pub load_tags: Box<dyn Fn() -> Vec<String>>,
    /// The recent vaults this window can switch to, newest first, the one it is on left out.
    /// Short and already filtered, so it is passed whole rather than behind a loader.
    pub vaults: Vec<String>,
    /// Chords no command of ours holds but that a widget does, each with the name of what it does
    /// there. They are not rows in the palette — nothing can run them — but the rebind dialog has
    /// to refuse them, or a command bound to one would be silently shadowed.
    pub taken: Vec<(String, String)>,
    /// Bind an action to a new set of accelerators, or to its default when given `None`. Returns
    /// what is in force afterwards, so the row can be redrawn without asking again.
    pub on_rebind: Box<Rebind>,
    /// Drop a vault from the recent list, by the key its row carries. Forgets a list entry and
    /// deletes nothing, which is why the row's button asks nothing first.
    pub on_forget: Box<dyn Fn(&str)>,
}

/// Something [`cache`] fills in at most once and hands out by handle for the life of the dialog.
type Cached<T> = Rc<RefCell<Option<Rc<T>>>>;

/// Bind `action` to `accels`, or to its default when they are `None`; yields what is in force.
pub type Rebind = dyn Fn(&str, Option<Vec<String>>) -> Vec<String>;

/// Which of the three lists the palette is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Files,
    Commands,
    Tags,
    Vaults,
}

impl Mode {
    /// Header title. An empty entry says nothing about the mode, so the header has to.
    fn title(self) -> &'static str {
        match self {
            Mode::Files => "Go to File",
            Mode::Commands => "Run Command",
            Mode::Tags => "Filter by Tag",
            Mode::Vaults => "Open Recent",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Mode::Files => "Search files…",
            Mode::Commands => "Run a command…",
            Mode::Tags => "Filter by tag…",
            Mode::Vaults => "Search recent vaults…",
        }
    }
}

/// Where a key moves the highlight in a list of `n` rows, or `None` where the key is not the
/// list's and the entry should have it. The arrows step, a page is [`PAGE`] rows, and Home and
/// End are the two ends — the keys every list in GNOME answers, on a dialog whose keyboard never
/// leaves the search box.
fn step(key: gdk::Key, at: u32, n: u32) -> Option<u32> {
    if n == 0 {
        return None;
    }
    let last = n - 1;
    match key {
        gdk::Key::Down | gdk::Key::KP_Down => Some((at + 1).min(last)),
        gdk::Key::Up | gdk::Key::KP_Up => Some(at.saturating_sub(1)),
        gdk::Key::Page_Down | gdk::Key::KP_Page_Down => Some((at + PAGE).min(last)),
        gdk::Key::Page_Up | gdk::Key::KP_Page_Up => Some(at.saturating_sub(PAGE)),
        gdk::Key::Home | gdk::Key::KP_Home => Some(0),
        gdk::Key::End | gdk::Key::KP_End => Some(last),
        _ => None,
    }
}

/// Split a raw query into its mode and the text to search for, starting from `opened_in`.
///
/// Only the *leading* character switches modes, so "notes > misc" stays whatever the palette was
/// opened as, and a command search does not fall back to file search once the user deletes the `>`
/// they never had to type.
fn parse_query(raw: &str, opened_in: Mode) -> (Mode, &str) {
    match raw.as_bytes().first() {
        Some(b'>') => (Mode::Commands, &raw[1..]),
        Some(b'#') => (Mode::Tags, &raw[1..]),
        _ => (opened_in, raw),
    }
}

/// Indices of `haystacks` that match `query`, best first, capped at [`MAX_RESULTS`].
///
/// This is `Pattern::match_list` with the index kept instead of the string, so the caller can map a
/// hit back to the [`Item`] it came from. Ties keep corpus order, as nucleo's stable sort does.
///
/// A haystack whose *last segment* matches leads, because a query is nearly always the name of
/// the thing and only rarely the folder it sits in: for "tes", `test.md` comes before `tes/t.md`.
/// The whole path is still matched — it is what lets a folder narrow a search — it just sorts
/// below. A haystack with no delimiter is its own last segment, so command and tag mode are
/// unaffected.
///
/// `recent` is either empty or one entry per haystack, holding how recently it was used. Inside
/// each of the two tiers anything used before leads, in use order, and the rest follow by score: a
/// command run twice is what the user means by that half-typed query, however well something else
/// scores. This is VS Code's quick-open behaviour.
fn rank(
    haystacks: &[String],
    recent: &[Option<usize>],
    query: &str,
    matcher: &mut Matcher,
) -> Vec<usize> {
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    // (index, score against the last segment, score against the whole path).
    let mut hits: Vec<(usize, Option<u32>, u32)> = haystacks
        .iter()
        .enumerate()
        .filter_map(|(i, h)| {
            let path = pattern.score(Utf32Str::new(h, &mut buf), matcher)?;
            let name = basename(h);
            // Scoring the whole path again when it has no folder in it would give the same number.
            let base = match name.len() == h.len() {
                true => Some(path),
                false => pattern.score(Utf32Str::new(name, &mut buf), matcher),
            };
            Some((i, base, path))
        })
        .collect();
    let used = |i: usize| recent.get(i).copied().flatten().unwrap_or(usize::MAX);
    hits.sort_by(|a, b| {
        a.1.is_none()
            .cmp(&b.1.is_none())
            .then(used(a.0).cmp(&used(b.0)))
            .then(b.1.cmp(&a.1))
            .then(b.2.cmp(&a.2))
            .then(a.0.cmp(&b.0))
    });
    hits.into_iter()
        .take(MAX_RESULTS)
        .map(|(i, _, _)| i)
        .collect()
}

/// Corpus for one mode, loaded at most once.
///
/// `load` reaches into the index and must never run while `slot` is borrowed, so this borrows,
/// clones the handle and drops before calling.
fn cache<T>(slot: &Cached<T>, load: &dyn Fn() -> T) -> Rc<T> {
    let cached = slot.borrow().clone();
    cached.unwrap_or_else(|| {
        let loaded = Rc::new(load());
        *slot.borrow_mut() = Some(loaded.clone());
        loaded
    })
}

/// Where each of `corpus` sits in `mru`, for [`rank`]'s recency tiebreak. A map rather than a
/// scan per path: the corpus is every file in the vault and this runs once per dialog.
fn places(corpus: &[String], mru: &[String]) -> Vec<Option<usize>> {
    let at: HashMap<&str, usize> = mru
        .iter()
        .enumerate()
        .map(|(i, rel)| (rel.as_str(), i))
        .collect();
    corpus
        .iter()
        .map(|rel| at.get(rel.as_str()).copied())
        .collect()
}

/// "<Control>p" -> "Ctrl+P", spelled the way this GTK build spells it.
///
/// `gtk::ShortcutLabel` would do the same, but it is deprecated since GTK 4.18.
fn accel_label(accel: &str) -> Option<String> {
    let (key, mods) = gtk::accelerator_parse(accel)?;
    Some(gtk::accelerator_get_label(key, mods).into())
}

/// Every accelerator that more than one command claims. Computed from the rows themselves, so a
/// clash a hand-edited `[shortcuts]` table introduced surfaces the same way one made in the
/// rebind dialog does.
fn conflicts(items: &[Rc<Item>]) -> HashSet<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut twice = HashSet::new();
    for item in items {
        let Item::Command { accels, .. } = &**item else {
            continue;
        };
        for accel in accels {
            if !seen.insert(accel) {
                twice.insert(accel.clone());
            }
        }
    }
    twice
}

/// The accelerator, as a button that opens the rebind dialog: DESIGN.md leaves command mode as the
/// app's only shortcuts reference, so this is also where a shortcut is changed.
fn accel_button(
    action: &str,
    accels: &[String],
    conflicts: &HashSet<String>,
    rebind: &Rc<dyn Fn(&str)>,
) -> gtk::Button {
    let shown = accels.first().and_then(|a| accel_label(a));
    let clashes = accels.iter().any(|a| conflicts.contains(a));
    let button = gtk::Button::builder()
        // An unbound command still lists; the button is what says it can be given a shortcut.
        .label(shown.as_deref().unwrap_or("Set…"))
        .tooltip_text(match clashes {
            true => "Change Shortcut (bound twice)",
            false => "Change Shortcut",
        })
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    button.add_css_class("caption");
    button.add_css_class(if clashes { "error" } else { "dim-label" });
    button.connect_clicked({
        let (rebind, action) = (rebind.clone(), action.to_string());
        move |_| rebind(&action)
    });
    button
}

/// Forgetting a recent vault, the same removal the start screen's rows have. Always visible, not
/// a hover affordance: a button that only appears under the pointer is not there at all for the
/// keyboard or for a touchscreen. No confirmation either — it drops a list entry and deletes
/// nothing on disk.
fn forget_button(key: &str, forget: &Rc<dyn Fn(&str)>) -> gtk::Button {
    let button = gtk::Button::builder()
        .icon_name("user-trash-symbolic")
        .tooltip_text("Remove from Recents")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    button.connect_clicked({
        let (forget, key) = (forget.clone(), key.to_string());
        move |_| forget(&key)
    });
    button
}

/// Row template: name, dimmed directory, and the slot the accelerator button goes in. The
/// directory label expands, so the accelerator sits at the far end even when there is no
/// directory to show.
///
/// No margins: `.navigation-sidebar` gives the row its 36 px height and its padding, the same way
/// the sidebar's file rows get theirs.
///
/// The button is built in `bind`, not in `setup`: it carries the action name of the row it is on,
/// and a widget recycled across rows cannot. Only command rows get one and there are forty
/// commands, so nothing worth saving is allocated here.
fn row_factory(
    rebind: Rc<dyn Fn(&str)>,
    forget: Rc<dyn Fn(&str)>,
    conflicts: Rc<RefCell<HashSet<String>>>,
) -> gtk::SignalListItemFactory {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .build();
        let name = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        let dir = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::Middle)
            .css_classes(["dim-label"])
            .build();
        let slot = gtk::Box::builder().valign(gtk::Align::Center).build();
        row.append(&name);
        row.append(&dir);
        row.append(&slot);
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&row));
    });
    let home = glib::home_dir();
    factory.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let (Some(row), Some(boxed)) = (
            item.child().and_downcast::<gtk::Box>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let (Some(name), Some(dir), Some(slot)) = (
            row.first_child().and_downcast::<gtk::Label>(),
            row.first_child()
                .and_then(|w| w.next_sibling())
                .and_downcast::<gtk::Label>(),
            row.last_child().and_downcast::<gtk::Box>(),
        ) else {
            return;
        };
        while let Some(child) = slot.first_child() {
            slot.remove(&child);
        }
        let entry: Rc<Item> = boxed.borrow::<Rc<Item>>().clone();
        match &*entry {
            // A note row reads as basename first, directory after: a vault full of `index.md`
            // files is unreadable the other way round.
            Item::File(rel) => {
                name.set_text(basename(rel));
                dir.set_text(parent_dir(rel));
            }
            Item::Command {
                action,
                label,
                accels,
                ..
            } => {
                name.set_text(label);
                dir.set_text("");
                slot.append(&accel_button(action, accels, &conflicts.borrow(), &rebind));
            }
            Item::Tag(tag) => {
                name.set_text(tag);
                dir.set_text("");
            }
            // The reading the start screen's recent list gives a vault: a local one named by its
            // folder and placed by its path, a remote one by its host and the path on that host.
            Item::Vault(key) => {
                let (title, subtitle) = start::labels(Path::new(key), Some(home.as_path()));
                name.set_text(&title);
                dir.set_text(&subtitle);
                slot.append(&forget_button(key, &forget));
            }
        }
    });
    factory
}

/// What the never-bind list of DESIGN.md's Keyboard section comes down to for someone standing in
/// front of the dialog. Documented rather than enforced: the desktop and the editor will simply
/// keep the chord, and saying so is more useful than a rule that guesses at the user's setup.
const RESERVED: &str = "Super, Alt+Tab, Ctrl+Alt and F1 belong to the desktop, and Ctrl+Z, Ctrl+A \
                        and Ctrl+X/C/V to the editor.";

/// Ask for one chord for `label`. `taken` is every accelerator already in use with the command
/// holding it, so a clash is refused by name instead of quietly shadowing the other command.
///
/// `on_done` is called with the accelerators to store: one chord, an empty list for "no shortcut",
/// or `None` to go back to the built-in default. Cancelling calls nothing.
fn capture_shortcut(
    parent: &gtk::Widget,
    label: &str,
    taken: Vec<(String, String)>,
    on_done: impl Fn(Option<Vec<String>>) + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(format!("Shortcut for {label}"))
        .body(format!(
            "Press the new shortcut. Backspace clears it, Escape cancels.\n\n{RESERVED}"
        ))
        .close_response("cancel")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("default", "Restore Default");

    let on_done = Rc::new(on_done);
    dialog.connect_response(None, {
        let on_done = on_done.clone();
        move |_, response| {
            if response == "default" {
                on_done(None);
            }
        }
    });

    // Capture phase: the responses are buttons, and one of them would otherwise answer Space or
    // Return before the chord ever reaches this.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let dialog = dialog.downgrade();
        move |_, key, _, state| {
            let Some(dialog) = dialog.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let mods = state & gtk::accelerator_get_default_mod_mask();
            // Escape leaves; the dialog's own close response handles it. Tab, Return and Space
            // reach the responses instead of being captured, or Restore Default would be
            // mouse-only (DESIGN.md pre-flight 9). Unmodified, they are no loss as chords.
            let navigation = matches!(key, gdk::Key::Tab | gdk::Key::ISO_Left_Tab)
                || (mods.is_empty()
                    && matches!(
                        key,
                        gdk::Key::Escape | gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space
                    ));
            if navigation {
                return glib::Propagation::Proceed;
            }
            if key == gdk::Key::BackSpace && mods.is_empty() {
                on_done(Some(Vec::new()));
                dialog.close();
                return glib::Propagation::Stop;
            }
            // Swallows the modifier presses on the way to the chord, so Ctrl alone shows nothing.
            if !gtk::accelerator_valid(key, mods) {
                return glib::Propagation::Stop;
            }
            let accel = gtk::accelerator_name(key, mods).to_string();
            if let Some((_, owner)) = taken.iter().find(|(a, _)| *a == accel) {
                let shown = accel_label(&accel).unwrap_or_else(|| accel.clone());
                dialog.set_body(&format!(
                    "{shown} is already used by {owner}. Press another shortcut.\n\n{RESERVED}"
                ));
                return glib::Propagation::Stop;
            }
            on_done(Some(vec![accel]));
            dialog.close();
            glib::Propagation::Stop
        }
    });
    dialog.add_controller(keys);
    dialog.present(Some(parent));
}

/// Opens in `mode` with an empty entry: the mode is chrome (title and placeholder), never a
/// character the user has to type around or delete.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    mode: Mode,
    sources: Sources,
    on_pick: impl Fn(&Item) + 'static,
) {
    let Sources {
        recent,
        mru,
        load_files,
        commands,
        load_tags,
        vaults,
        taken,
        on_rebind,
        on_forget,
    } = sources;
    let recent = Rc::new(recent);
    let mru = Rc::new(mru);
    // Behind a cell because the trash button on a row rewrites the list without closing the
    // dialog. Ranked against itself, so a typed query still breaks ties by how recently a vault
    // was open.
    let vault_places = Rc::new(RefCell::new(places(&vaults, &vaults)));
    let vaults = Rc::new(RefCell::new(vaults));
    // Behind a cell because a rebind rewrites one row's accelerators without closing the dialog.
    let commands: Rc<RefCell<Vec<Rc<Item>>>> =
        Rc::new(RefCell::new(commands.into_iter().map(Rc::new).collect()));
    let command_text: Rc<Vec<String>> = Rc::new(
        commands
            .borrow()
            .iter()
            .map(|c| c.text().to_string())
            .collect(),
    );
    let command_recent: Rc<Vec<Option<usize>>> = Rc::new(
        commands
            .borrow()
            .iter()
            .map(|c| match &**c {
                Item::Command { recent, .. } => *recent,
                _ => None,
            })
            .collect(),
    );
    let clashes = Rc::new(RefCell::new(conflicts(&commands.borrow())));
    // Filled on first use, then reused for the life of the dialog.
    let notes: Cached<Vec<String>> = Rc::new(RefCell::new(None));
    let tags: Cached<Vec<String>> = Rc::new(RefCell::new(None));
    // Where each file sits in the window's most-recent list. Cached with the corpus it indexes:
    // it is one pass over every path in the vault, and the corpus does not change while the
    // dialog is up.
    let note_recent: Cached<Vec<Option<usize>>> = Rc::new(RefCell::new(None));
    let matcher = Rc::new(RefCell::new(Matcher::new(Config::DEFAULT)));

    let model = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(model.clone()));
    // The factory is set further down: its rows open the rebind dialog, which needs the dialog
    // this function has not built yet.
    let list = gtk::ListView::new(Some(selection.clone()), None::<gtk::SignalListItemFactory>);
    list.set_single_click_activate(true);
    // The class the sidebar's file rows use: inset rounded pills, 6 px apart from the list edge.
    list.add_css_class("navigation-sidebar");
    // 6 more, so a pill sits 12 px in and lines up with the entry above it.
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .margin_start(6)
        .margin_end(6)
        .margin_bottom(6)
        .child(&list)
        .build();

    // DESIGN.md: an empty result set is an AdwStatusPage, not a blank list. `.compact` keeps it
    // inside a 560x420 dialog.
    let empty = status_page(
        "system-search-symbolic",
        "No Results",
        "Try a different search.",
    );
    let stack = gtk::Stack::builder().vexpand(true).build();
    stack.add_named(&scroller, Some("list"));
    stack.add_named(&empty, Some("empty"));

    let entry = gtk::SearchEntry::builder()
        .placeholder_text(mode.placeholder())
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&entry);
    content.append(&stack);

    // A dialog's close affordance belongs in a header bar, and the header is also where the mode
    // name goes: `AdwHeaderBar` inside an `AdwDialog` shows the dialog's own title.
    let close = gtk::Button::builder()
        .icon_name("window-close-symbolic")
        .tooltip_text("Close")
        .css_classes(["flat"])
        .build();
    let header = adw::HeaderBar::builder()
        .show_end_title_buttons(false)
        .build();
    header.pack_end(&close);
    let toolbar = adw::ToolbarView::builder().content(&content).build();
    toolbar.add_top_bar(&header);

    let dialog = adw::Dialog::builder()
        .title(mode.title())
        .content_width(560)
        .content_height(420)
        .child(&toolbar)
        .build();

    let refresh = Rc::new({
        let (model, selection, stack) = (model.clone(), selection.clone(), stack.clone());
        let (recent, notes, tags) = (recent.clone(), notes.clone(), tags.clone());
        let (mru, note_recent) = (mru.clone(), note_recent.clone());
        let (vaults, vault_places) = (vaults.clone(), vault_places.clone());
        let (commands, command_text, command_recent, matcher) = (
            commands.clone(),
            command_text.clone(),
            command_recent.clone(),
            matcher.clone(),
        );
        move |raw: &str| {
            let t0 = Instant::now();
            let (mode, query) = parse_query(raw, mode);
            let empty_query = query.trim().is_empty();
            let hits: Vec<Rc<Item>> =
                match mode {
                    // No corpus and no matching until the user actually types: the dialog is up in the
                    // time one indexed query takes, not the 11 s a full vault walk took.
                    Mode::Files if empty_query => recent
                        .iter()
                        .take(MAX_RESULTS)
                        .map(|rel| Rc::new(Item::File(rel.clone())))
                        .collect(),
                    Mode::Files => {
                        let corpus = cache(&notes, &load_files);
                        let used = cache(&note_recent, &|| places(&corpus, &mru));
                        let mut m = matcher.borrow_mut();
                        m.config = Config::DEFAULT.match_paths();
                        rank(&corpus, &used, query, &mut m)
                            .into_iter()
                            .map(|i| Rc::new(Item::File(corpus[i].clone())))
                            .collect()
                    }
                    // Labels and tags are not paths, so they score better under the plain config.
                    Mode::Commands => {
                        let mut m = matcher.borrow_mut();
                        m.config = Config::DEFAULT;
                        rank(&command_text, &command_recent, query, &mut m)
                            .into_iter()
                            .map(|i| commands.borrow()[i].clone())
                            .collect()
                    }
                    Mode::Tags => {
                        let corpus = cache(&tags, &load_tags);
                        let mut m = matcher.borrow_mut();
                        m.config = Config::DEFAULT;
                        rank(&corpus, &[], query, &mut m)
                            .into_iter()
                            .map(|i| Rc::new(Item::Tag(corpus[i].clone())))
                            .collect()
                    }
                    // The two ways of opening a vault that is *not* in the list end the rows, and are
                    // never filtered out: one surface reaches every way of changing vault, and the
                    // picker is never the empty status page even in a window on the only vault known.
                    Mode::Vaults => {
                        let vaults = vaults.borrow();
                        let mut hits: Vec<Rc<Item>> = if empty_query {
                            vaults
                                .iter()
                                .take(MAX_RESULTS)
                                .map(|key| Rc::new(Item::Vault(key.clone())))
                                .collect()
                        } else {
                            let mut m = matcher.borrow_mut();
                            m.config = Config::DEFAULT.match_paths();
                            rank(&vaults, &vault_places.borrow(), query, &mut m)
                                .into_iter()
                                .map(|i| Rc::new(Item::Vault(vaults[i].clone())))
                                .collect()
                        };
                        // Read on every refresh rather than captured, so a rebind made from one of
                        // these rows redraws with the accelerator it was just given.
                        for action in OPENERS {
                            let found = commands.borrow().iter().find(|c| {
                            matches!(&***c, Item::Command { action: a, .. } if a == action)
                        }).cloned();
                            hits.extend(found);
                        }
                        hits
                    }
                };

            let objects: Vec<glib::BoxedAnyObject> =
                hits.into_iter().map(glib::BoxedAnyObject::new).collect();
            model.splice(0, model.n_items(), &objects);
            if !objects.is_empty() {
                selection.set_selected(0);
            }
            stack.set_visible_child_name(if objects.is_empty() { "empty" } else { "list" });
            tracing::debug!(
                query = raw,
                hits = objects.len(),
                ms = t0.elapsed().as_secs_f64() * 1e3,
                "palette query"
            );
        }
    });

    // Clicking an accelerator asks for the new chord, applies it and redraws the list in place.
    // The selection is put back afterwards, so rebinding several commands in a row keeps its place.
    let rebind: Rc<dyn Fn(&str)> = Rc::new({
        let (commands, clashes, refresh) = (commands.clone(), clashes.clone(), refresh.clone());
        let (entry, selection) = (entry.clone(), selection.clone());
        let dialog = dialog.downgrade();
        let on_rebind = Rc::new(on_rebind);
        let widget_taken = Rc::new(taken);
        move |action: &str| {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            let Some(index) = commands.borrow().iter().position(|c| match &**c {
                Item::Command { action: a, .. } => a == action,
                _ => false,
            }) else {
                return;
            };
            let label = commands.borrow()[index].text().to_string();
            let mut taken: Vec<(String, String)> = commands
                .borrow()
                .iter()
                .filter_map(|c| match &**c {
                    Item::Command { action: a, .. } if a == action => None,
                    Item::Command { label, accels, .. } => {
                        Some(accels.iter().map(|a| (a.clone(), label.clone())))
                    }
                    _ => None,
                })
                .flatten()
                .collect();
            taken.extend(widget_taken.iter().cloned());
            capture_shortcut(dialog.upcast_ref::<gtk::Widget>(), &label, taken, {
                let (commands, clashes, refresh) =
                    (commands.clone(), clashes.clone(), refresh.clone());
                let (entry, selection, on_rebind) =
                    (entry.clone(), selection.clone(), on_rebind.clone());
                let action = action.to_string();
                move |chosen| {
                    let accels = on_rebind(&action, chosen);
                    let (label, recent) = match &*commands.borrow()[index] {
                        Item::Command { label, recent, .. } => (label.clone(), *recent),
                        _ => return,
                    };
                    commands.borrow_mut()[index] = Rc::new(Item::Command {
                        action: action.clone(),
                        label,
                        accels,
                        recent,
                    });
                    *clashes.borrow_mut() = conflicts(&commands.borrow());
                    let selected = selection.selected();
                    refresh(&entry.text());
                    if selected < selection.n_items() {
                        selection.set_selected(selected);
                    }
                }
            });
        }
    });
    // Forgetting a vault redraws the list in place: the entry goes, the rows below it move up and
    // the highlight stays where it was, the same way a rebind redraws its row. With the last vault
    // gone the two openers are what is left, which is the list's empty state — see `Mode::Vaults`
    // above, where they are appended on every refresh and never filtered out.
    let forget: Rc<dyn Fn(&str)> = Rc::new({
        let (vaults, vault_places, refresh) =
            (vaults.clone(), vault_places.clone(), refresh.clone());
        let (entry, selection) = (entry.clone(), selection.clone());
        move |key: &str| {
            on_forget(key);
            {
                let mut vaults = vaults.borrow_mut();
                vaults.retain(|vault| vault != key);
                *vault_places.borrow_mut() = places(&vaults, &vaults);
            }
            let selected = selection.selected();
            refresh(&entry.text());
            if selected < selection.n_items() {
                selection.set_selected(selected);
            }
        }
    });
    // Delete on a highlighted Open Recent row does what its trash button does — but only where
    // the key is free: in a text entry Delete takes the character after the caret, so it is the
    // list's only when there is none to take. Nothing is deleted from disk either way.
    let forget_row: Rc<dyn Fn(&gtk::SearchEntry) -> glib::Propagation> = Rc::new({
        let (forget, selection) = (forget.clone(), selection.clone());
        move |entry: &gtk::SearchEntry| {
            let text = entry.text();
            if (entry.position() as usize) < text.chars().count() {
                return glib::Propagation::Proceed;
            }
            let Some(boxed) = selection
                .selected_item()
                .and_downcast::<glib::BoxedAnyObject>()
            else {
                return glib::Propagation::Proceed;
            };
            let item: Rc<Item> = boxed.borrow::<Rc<Item>>().clone();
            let Item::Vault(key) = &*item else {
                return glib::Propagation::Proceed;
            };
            forget(key);
            glib::Propagation::Stop
        }
    });
    list.set_factory(Some(&row_factory(rebind, forget, clashes.clone())));

    refresh("");

    // Dropped with the dialog, so a late timeout cannot touch a closed window.
    let debounce = Rc::new(Debounce::new(DEBOUNCE));
    // What the list is already showing, so a `search-changed` that carries no new text cannot put
    // the selection back on row 0 under the user's fingers.
    let shown = Rc::new(RefCell::new(String::new()));
    entry.connect_search_changed({
        let (refresh, debounce, shown) = (refresh.clone(), debounce.clone(), shown.clone());
        let weak_dialog = dialog.downgrade();
        move |e| {
            let query = e.text().to_string();
            if *shown.borrow() == query {
                return;
            }
            *shown.borrow_mut() = query.clone();
            // The chrome tracks a typed `>` or `#` immediately; only the matching waits.
            let typed = parse_query(&query, mode).0;
            e.set_placeholder_text(Some(typed.placeholder()));
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.set_title(typed.title());
            }
            debounce.call({
                let refresh = refresh.clone();
                move || refresh(&query)
            });
        }
    });
    // libadwaita does not close a floating dialog when the click lands outside it: its dimming
    // widget is targetable but carries no gesture, so `sheet.close`, Escape and an explicit
    // `close()` are the only ways out (adw-floating-sheet.c). One capture-phase gesture on the
    // window adds the behaviour every other palette has. Capture, because the widget under the
    // pointer would otherwise consume the press on its way back up.
    let outside = parent.as_ref().root().map(|root| {
        let gesture = gtk::GestureClick::new();
        gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
        gesture.connect_pressed({
            let (root, dialog) = (root.clone(), dialog.downgrade());
            move |_, _, x, y| {
                let Some(dialog) = dialog.upgrade() else {
                    return;
                };
                let inside = root.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|hit| {
                    let dialog = dialog.upcast_ref::<gtk::Widget>();
                    &hit == dialog
                        || hit.is_ancestor(dialog)
                        // The rebind prompt is a dialog of its own, stacked on this one: a click
                        // in it is not a click outside the palette.
                        || hit.ancestor(adw::Dialog::static_type()).is_some()
                });
                if !inside {
                    dialog.close();
                }
            }
        });
        root.add_controller(gesture.clone());
        (root, gesture)
    });

    dialog.connect_closed({
        let debounce = debounce.clone();
        move |_| {
            debounce.cancel();
            if let Some((root, gesture)) = &outside {
                root.remove_controller(gesture);
            }
        }
    });

    let on_pick = Rc::new(on_pick);
    let pick = {
        // Weak dialog: this closure hangs off widgets inside it, and a strong capture is the cycle
        // that kept every palette ever opened alive, each with its own copy of the note list.
        let (dialog, selection, on_pick) = (dialog.downgrade(), selection.clone(), on_pick.clone());
        move || {
            let Some(dialog) = dialog.upgrade() else {
                return;
            };
            if let Some(boxed) = selection
                .selected_item()
                .and_downcast::<glib::BoxedAnyObject>()
            {
                let item: Rc<Item> = boxed.borrow::<Rc<Item>>().clone();
                dialog.close();
                on_pick(&item);
            }
        }
    };
    close.connect_clicked({
        let dialog = dialog.downgrade();
        move |_| {
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
        }
    });
    entry.connect_activate({
        let pick = pick.clone();
        move |_| pick()
    });
    list.connect_activate({
        let pick = pick.clone();
        move |_, _| pick()
    });

    // `GtkSearchEntry` eats the first Escape to clear its own text, so Escape has to be caught on
    // the way down to it instead of on the way back up.
    let escape = gtk::EventControllerKey::new();
    escape.set_propagation_phase(gtk::PropagationPhase::Capture);
    escape.connect_key_pressed({
        let dialog = dialog.downgrade();
        move |_, key, _, _| {
            if key != gdk::Key::Escape {
                return glib::Propagation::Proceed;
            }
            if let Some(dialog) = dialog.upgrade() {
                dialog.close();
            }
            glib::Propagation::Stop
        }
    });
    dialog.add_controller(escape);

    // The list is driven from the entry, which keeps the focus: the arrows, a page and the two
    // ends move the highlight, and Delete forgets the recent vault under it.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed({
        let (entry, forget) = (entry.clone(), forget_row.clone());
        move |_, key, _, _| {
            if key == gdk::Key::Delete || key == gdk::Key::KP_Delete {
                return forget(&entry);
            }
            let Some(to) = step(key, selection.selected(), selection.n_items()) else {
                return glib::Propagation::Proceed;
            };
            selection.set_selected(to);
            list.scroll_to(to, gtk::ListScrollFlags::NONE, None);
            glib::Propagation::Stop
        }
    });
    entry.add_controller(keys);

    dialog.present(Some(parent));
    entry.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_keys_step_a_row_a_page_and_to_the_ends() {
        assert_eq!(step(gdk::Key::Down, 0, 25), Some(1));
        assert_eq!(step(gdk::Key::Up, 0, 25), Some(0));
        assert_eq!(step(gdk::Key::Page_Down, 0, 25), Some(10));
        assert_eq!(step(gdk::Key::Page_Down, 20, 25), Some(24));
        assert_eq!(step(gdk::Key::Page_Up, 3, 25), Some(0));
        assert_eq!(step(gdk::Key::End, 0, 25), Some(24));
        assert_eq!(step(gdk::Key::KP_Home, 24, 25), Some(0));
        // Every other key belongs to the entry, and an empty list has nothing to move.
        assert_eq!(step(gdk::Key::a, 0, 25), None);
        assert_eq!(step(gdk::Key::Down, 0, 0), None);
    }

    #[test]
    fn parse_query_reads_only_the_leading_character() {
        let files = |raw| parse_query(raw, Mode::Files);
        assert_eq!(files(""), (Mode::Files, ""));
        assert_eq!(files("deep"), (Mode::Files, "deep"));
        assert_eq!(files(">"), (Mode::Commands, ""));
        assert_eq!(files(">save"), (Mode::Commands, "save"));
        assert_eq!(files("#"), (Mode::Tags, ""));
        assert_eq!(files("#area"), (Mode::Tags, "area"));
        // A `>` or `#` further in is part of the file search, not a mode switch.
        assert_eq!(files("notes > misc"), (Mode::Files, "notes > misc"));
        assert_eq!(files("a#b"), (Mode::Files, "a#b"));
    }

    #[test]
    fn parse_query_stays_in_the_mode_it_opened_in() {
        // Nothing is seeded into the entry, so an unprefixed query keeps the caller's mode.
        assert_eq!(parse_query("", Mode::Commands), (Mode::Commands, ""));
        assert_eq!(
            parse_query("save", Mode::Commands),
            (Mode::Commands, "save")
        );
        assert_eq!(parse_query("area", Mode::Tags), (Mode::Tags, "area"));
        // A prefix still switches, whichever mode it started in.
        assert_eq!(parse_query("#area", Mode::Commands), (Mode::Tags, "area"));
        assert_eq!(parse_query(">save", Mode::Tags), (Mode::Commands, "save"));
    }

    #[test]
    fn rank_keeps_matches_and_drops_the_rest() {
        let corpus = vec![
            "archive/2020/notes.md".to_string(),
            "daily/2026-09-03.md".to_string(),
            "projects/deep-work.md".to_string(),
        ];
        let mut m = Matcher::new(Config::DEFAULT.match_paths());
        assert_eq!(rank(&corpus, &[], "deep", &mut m), vec![2]);
        assert_eq!(rank(&corpus, &[], "daily", &mut m), vec![1]);
        assert!(rank(&corpus, &[], "zzzz", &mut m).is_empty());
        // An empty pattern matches everything, in corpus order.
        assert_eq!(rank(&corpus, &[], "", &mut m), vec![0, 1, 2]);
    }

    #[test]
    fn rank_prefers_a_contiguous_match() {
        let corpus = vec!["d-e-e-p.md".to_string(), "deep-work.md".to_string()];
        let mut m = Matcher::new(Config::DEFAULT.match_paths());
        assert_eq!(rank(&corpus, &[], "deep", &mut m), vec![1, 0]);
    }

    #[test]
    fn rank_leads_with_what_was_used_before() {
        let corpus = vec!["d-e-e-p.md".to_string(), "deep-work.md".to_string()];
        let mut m = Matcher::new(Config::DEFAULT.match_paths());
        // The worse match was used last, so it leads; score decides everything below.
        assert_eq!(rank(&corpus, &[Some(0), None], "deep", &mut m), vec![0, 1]);
        // Two recent hits keep their use order, not their score order.
        assert_eq!(
            rank(&corpus, &[Some(1), Some(0)], "deep", &mut m),
            vec![1, 0]
        );
        // Recency never rescues a non-match.
        assert!(rank(&corpus, &[Some(0), Some(1)], "zzzz", &mut m).is_empty());
    }

    #[test]
    fn rank_puts_a_filename_match_above_a_path_match() {
        let corpus = vec!["tes/t.md".to_string(), "test.md".to_string()];
        let mut m = Matcher::new(Config::DEFAULT.match_paths());
        assert_eq!(rank(&corpus, &[], "tes", &mut m), vec![1, 0]);
    }

    #[test]
    fn rank_prefers_a_filename_over_a_recent_folder() {
        let corpus = vec!["tes/t.md".to_string(), "test.md".to_string()];
        let mut m = Matcher::new(Config::DEFAULT.match_paths());
        // Recency orders the files whose name matches; it does not promote one whose folder does.
        assert_eq!(rank(&corpus, &[Some(0), None], "tes", &mut m), vec![1, 0]);
    }

    #[test]
    fn rank_leads_with_the_recent_file_when_both_names_match() {
        let corpus = vec!["a/test.md".to_string(), "b/test.md".to_string()];
        let mut m = Matcher::new(Config::DEFAULT.match_paths());
        assert_eq!(rank(&corpus, &[None, Some(0)], "test", &mut m), vec![1, 0]);
    }

    fn command(action: &str, accels: &[&str]) -> Rc<Item> {
        Rc::new(Item::Command {
            action: action.to_string(),
            label: action.to_string(),
            accels: accels.iter().map(|a| a.to_string()).collect(),
            recent: None,
        })
    }

    #[test]
    fn conflicts_finds_accelerators_bound_twice() {
        let items = vec![
            command("win.save", &["<Control>s"]),
            command("win.find", &["<Control>f", "<Control>s"]),
            command("win.about", &[]),
            Rc::new(Item::File("a.md".to_string())),
        ];
        let twice = conflicts(&items);
        assert_eq!(twice.len(), 1);
        assert!(twice.contains("<Control>s"));
        // A chord only one command claims is not a conflict, and neither is being unbound.
        assert!(!twice.contains("<Control>f"));
    }
}
