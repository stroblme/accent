//! Command palette and file switcher: one dialog, three modes.
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

use adw::prelude::*;
use gtk::glib;
use gtk::{gdk, gio, pango};
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Beyond this the list stops being scannable and nucleo's single-threaded matcher starts to show.
const MAX_RESULTS: usize = 200;
/// Long enough to swallow a burst of keystrokes, short enough to feel immediate.
const DEBOUNCE: Duration = Duration::from_millis(50);

/// One thing the palette can offer.
pub enum Item {
    /// A note to open, by vault-relative path.
    Note(String),
    /// A `GAction` on the window, with the label and accelerator to show.
    Command {
        action: String,
        label: String,
        accel: Option<String>,
    },
    /// A tag to filter by.
    Tag(String),
}

impl Item {
    /// The text the palette matches against and shows first in the row.
    fn text(&self) -> &str {
        match self {
            Item::Note(rel) => rel,
            Item::Command { label, .. } => label,
            Item::Tag(tag) => tag,
        }
    }
}

/// Where the three modes get their rows. The two loaders are called at most once per dialog.
pub struct Sources {
    /// Shown in file mode until the first keystroke; never matched against.
    pub recent: Vec<String>,
    pub load_notes: Box<dyn Fn() -> Vec<String>>,
    pub commands: Vec<Item>,
    pub load_tags: Box<dyn Fn() -> Vec<String>>,
}

/// Which of the three lists the palette is showing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Files,
    Commands,
    Tags,
}

impl Mode {
    /// Header title. An empty entry says nothing about the mode, so the header has to.
    fn title(self) -> &'static str {
        match self {
            Mode::Files => "Open Note",
            Mode::Commands => "Run Command",
            Mode::Tags => "Filter by Tag",
        }
    }

    fn placeholder(self) -> &'static str {
        match self {
            Mode::Files => "Search notes…",
            Mode::Commands => "Run a command…",
            Mode::Tags => "Filter by tag…",
        }
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

/// A note row reads as basename first, directory after: a vault full of `index.md` files is
/// unreadable the other way round.
fn split_note(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(i) => (&rel[i + 1..], &rel[..i]),
        None => (rel, ""),
    }
}

/// Indices of `haystacks` that match `query`, best first, capped at [`MAX_RESULTS`].
///
/// This is `Pattern::match_list` with the index kept instead of the string, so the caller can map a
/// hit back to the [`Item`] it came from. Ties keep corpus order, as nucleo's stable sort does.
fn rank(haystacks: &[String], query: &str, matcher: &mut Matcher) -> Vec<usize> {
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    let mut buf = Vec::new();
    let mut hits: Vec<(usize, u32)> = haystacks
        .iter()
        .enumerate()
        .filter_map(|(i, h)| {
            pattern
                .score(Utf32Str::new(h, &mut buf), matcher)
                .map(|score| (i, score))
        })
        .collect();
    hits.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    hits.into_iter().take(MAX_RESULTS).map(|(i, _)| i).collect()
}

/// Corpus for one mode, loaded at most once.
///
/// `load` reaches into the index and must never run while `slot` is borrowed, so this borrows,
/// clones the handle and drops before calling.
fn cache(
    slot: &RefCell<Option<Rc<Vec<String>>>>,
    load: &dyn Fn() -> Vec<String>,
) -> Rc<Vec<String>> {
    let cached = slot.borrow().clone();
    cached.unwrap_or_else(|| {
        let loaded = Rc::new(load());
        *slot.borrow_mut() = Some(loaded.clone());
        loaded
    })
}

/// "<Control>p" -> "Ctrl+P", spelled the way this GTK build spells it.
///
/// `gtk::ShortcutLabel` would do the same, but it is deprecated since GTK 4.18.
fn accel_label(accel: &str) -> Option<String> {
    let (key, mods) = gtk::accelerator_parse(accel)?;
    Some(gtk::accelerator_get_label(key, mods).into())
}

/// Row template: name, dimmed directory, dimmed accelerator. The directory label expands, so the
/// accelerator sits at the far end even when there is no directory to show.
///
/// No margins: `.navigation-sidebar` gives the row its 36 px height and its padding, the same way
/// the sidebar's file rows get theirs.
fn row_factory() -> gtk::SignalListItemFactory {
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
        let accel = gtk::Label::builder()
            .xalign(1.0)
            .css_classes(["dim-label"])
            .build();
        row.append(&name);
        row.append(&dir);
        row.append(&accel);
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let (Some(row), Some(boxed)) = (
            item.child().and_downcast::<gtk::Box>(),
            item.item().and_downcast::<glib::BoxedAnyObject>(),
        ) else {
            return;
        };
        let (Some(name), Some(dir), Some(accel)) = (
            row.first_child().and_downcast::<gtk::Label>(),
            row.first_child()
                .and_then(|w| w.next_sibling())
                .and_downcast::<gtk::Label>(),
            row.last_child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        let entry: Rc<Item> = boxed.borrow::<Rc<Item>>().clone();
        match &*entry {
            Item::Note(rel) => {
                let (base, parent) = split_note(rel);
                name.set_text(base);
                dir.set_text(parent);
                accel.set_text("");
            }
            Item::Command {
                label, accel: acc, ..
            } => {
                name.set_text(label);
                dir.set_text("");
                accel.set_text(&acc.as_deref().and_then(accel_label).unwrap_or_default());
            }
            Item::Tag(tag) => {
                name.set_text(tag);
                dir.set_text("");
                accel.set_text("");
            }
        }
    });
    factory
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
        load_notes,
        commands,
        load_tags,
    } = sources;
    let recent = Rc::new(recent);
    let commands: Rc<Vec<Rc<Item>>> = Rc::new(commands.into_iter().map(Rc::new).collect());
    let command_text: Rc<Vec<String>> =
        Rc::new(commands.iter().map(|c| c.text().to_string()).collect());
    // Filled on first use, then reused for the life of the dialog.
    let notes: Rc<RefCell<Option<Rc<Vec<String>>>>> = Rc::new(RefCell::new(None));
    let tags: Rc<RefCell<Option<Rc<Vec<String>>>>> = Rc::new(RefCell::new(None));
    let matcher = Rc::new(RefCell::new(Matcher::new(Config::DEFAULT)));

    let model = gio::ListStore::new::<glib::BoxedAnyObject>();
    let selection = gtk::SingleSelection::new(Some(model.clone()));
    let list = gtk::ListView::new(Some(selection.clone()), Some(row_factory()));
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
    let empty = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title("No Results")
        .description("Try a different search.")
        .css_classes(["compact"])
        .build();
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
        let (commands, command_text, matcher) =
            (commands.clone(), command_text.clone(), matcher.clone());
        move |raw: &str| {
            let t0 = Instant::now();
            let (mode, query) = parse_query(raw, mode);
            let empty_query = query.trim().is_empty();
            let hits: Vec<Rc<Item>> = match mode {
                // No corpus and no matching until the user actually types: the dialog is up in the
                // time one indexed query takes, not the 11 s a full vault walk took.
                Mode::Files if empty_query => recent
                    .iter()
                    .take(MAX_RESULTS)
                    .map(|rel| Rc::new(Item::Note(rel.clone())))
                    .collect(),
                Mode::Files => {
                    let corpus = cache(&notes, &load_notes);
                    let mut m = matcher.borrow_mut();
                    m.config = Config::DEFAULT.match_paths();
                    rank(&corpus, query, &mut m)
                        .into_iter()
                        .map(|i| Rc::new(Item::Note(corpus[i].clone())))
                        .collect()
                }
                // Labels and tags are not paths, so they score better under the plain config.
                Mode::Commands => {
                    let mut m = matcher.borrow_mut();
                    m.config = Config::DEFAULT;
                    rank(&command_text, query, &mut m)
                        .into_iter()
                        .map(|i| commands[i].clone())
                        .collect()
                }
                Mode::Tags => {
                    let corpus = cache(&tags, &load_tags);
                    let mut m = matcher.borrow_mut();
                    m.config = Config::DEFAULT;
                    rank(&corpus, query, &mut m)
                        .into_iter()
                        .map(|i| Rc::new(Item::Tag(corpus[i].clone())))
                        .collect()
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

    refresh("");

    // Debounce: one pending source at a time, replaced on every keystroke and dropped with the
    // dialog so a late timeout cannot touch a closed window.
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    // What the list is already showing, so a `search-changed` that carries no new text cannot put
    // the selection back on row 0 under the user's fingers.
    let shown = Rc::new(RefCell::new(String::new()));
    entry.connect_search_changed({
        let (refresh, pending, shown) = (refresh.clone(), pending.clone(), shown.clone());
        let weak_dialog = dialog.downgrade();
        move |e| {
            let query = e.text().to_string();
            if *shown.borrow() == query {
                return;
            }
            *shown.borrow_mut() = query.clone();
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            // The chrome tracks a typed `>` or `#` immediately; only the matching waits.
            let typed = parse_query(&query, mode).0;
            e.set_placeholder_text(Some(typed.placeholder()));
            if let Some(dialog) = weak_dialog.upgrade() {
                dialog.set_title(typed.title());
            }
            let id = glib::timeout_add_local_once(DEBOUNCE, {
                let (refresh, pending) = (refresh.clone(), pending.clone());
                move || {
                    *pending.borrow_mut() = None;
                    refresh(&query);
                }
            });
            *pending.borrow_mut() = Some(id);
        }
    });
    dialog.connect_closed({
        let pending = pending.clone();
        move |_| {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
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

    // Arrow keys move the list selection while the entry keeps focus.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        let n = selection.n_items();
        let cur = selection.selected();
        match key {
            gdk::Key::Down if n > 0 => selection.set_selected((cur + 1).min(n - 1)),
            gdk::Key::Up if n > 0 => selection.set_selected(cur.saturating_sub(1)),
            _ => return glib::Propagation::Proceed,
        }
        list.scroll_to(selection.selected(), gtk::ListScrollFlags::NONE, None);
        glib::Propagation::Stop
    });
    entry.add_controller(keys);

    dialog.present(Some(parent));
    entry.grab_focus();
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn split_note_puts_the_basename_first() {
        assert_eq!(
            split_note("areas/work/index.md"),
            ("index.md", "areas/work")
        );
        assert_eq!(split_note("index.md"), ("index.md", ""));
        assert_eq!(split_note("a/b.md"), ("b.md", "a"));
    }

    #[test]
    fn rank_keeps_matches_and_drops_the_rest() {
        let corpus = vec![
            "archive/2020/notes.md".to_string(),
            "daily/2026-09-03.md".to_string(),
            "projects/deep-work.md".to_string(),
        ];
        let mut m = Matcher::new(Config::DEFAULT.match_paths());
        assert_eq!(rank(&corpus, "deep", &mut m), vec![2]);
        assert_eq!(rank(&corpus, "daily", &mut m), vec![1]);
        assert!(rank(&corpus, "zzzz", &mut m).is_empty());
        // An empty pattern matches everything, in corpus order.
        assert_eq!(rank(&corpus, "", &mut m), vec![0, 1, 2]);
    }

    #[test]
    fn rank_prefers_a_contiguous_match() {
        let corpus = vec!["d-e-e-p.md".to_string(), "deep-work.md".to_string()];
        let mut m = Matcher::new(Config::DEFAULT.match_paths());
        assert_eq!(rank(&corpus, "deep", &mut m), vec![1, 0]);
    }
}
