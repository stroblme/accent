//! The left sidebar: Files / Search / Tags / Backlinks in a view switcher.
//!
//! The pane knows nothing about the vault. The file tree arrives as a finished widget and every
//! query goes through a closure in [`Data`], so this module never touches app state and the
//! integration step only has to hand it three closures.

use accent_core::index::SearchHit;
use adw::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;
use std::time::Duration;

/// Same value as the palette and the switcher (DESIGN.md, Motion): long enough to swallow a burst
/// of keystrokes, short enough to feel immediate.
const DEBOUNCE: Duration = Duration::from_millis(50);
/// The file list under a selected tag shares the pane with the tag list, so it gets a fixed slice
/// of it (DESIGN.md, Spacing).
const LIST_HEIGHT: i32 = 120;
/// Notes pointing back at the open one, as an arrow returning to where it came from. Adwaita's one
/// link-named glyph, `insert-link-symbolic`, is a text-insertion mark (two rules over a caret): it
/// reads as "paste a link here" rather than "what links here", and it is the only icon of the four
/// whose artwork is off centre, sitting a pixel low in its 16 px box.
const BACKLINK_ICON: &str = "mail-reply-sender-symbolic";

/// Everything the sidebar needs from the index, as closures so it never sees a vault handle.
// Boxed closures returning a `Vec` are the whole point of this struct; a type alias per field would
// only hide the signature the caller has to write.
#[allow(clippy::type_complexity)]
pub struct Data {
    pub search: Box<dyn Fn(&str) -> Vec<SearchHit>>,
    pub tags: Box<dyn Fn() -> Vec<(String, i64)>>,
    pub files_with_tag: Box<dyn Fn(&str) -> Vec<String>>,
}

pub struct Sidebar {
    root: gtk::Widget,
    switcher: gtk::Widget,
    stack: adw::ViewStack,
    search_entry: gtk::SearchEntry,
    backlinks: gtk::StringList,
    backlinks_stack: gtk::Stack,
    tags_dirty: Rc<Cell<bool>>,
    select_tag: Rc<dyn Fn(&str)>,
}

impl Sidebar {
    /// `files` is the existing vault tree widget, dropped into the Files pane unchanged.
    /// `on_open` is called with a vault-relative path when the user activates a result, a tagged
    /// file or a backlink.
    pub fn new(files: gtk::Widget, data: Data, on_open: impl Fn(&str) + 'static) -> Sidebar {
        let data = Rc::new(data);
        let on_open: Rc<dyn Fn(&str)> = Rc::new(on_open);

        let stack = adw::ViewStack::builder().vexpand(true).build();
        stack.add_titled_with_icon(&files, Some("files"), "Files", "folder-symbolic");

        let search = search_pane(&data, &on_open);
        stack.add_titled_with_icon(
            &search.widget,
            Some("search"),
            "Search",
            "system-search-symbolic",
        );

        let tags = tags_pane(&data, &on_open);
        stack.add_titled_with_icon(
            &tags.widget,
            Some("tags"),
            "Tags",
            "user-bookmarks-symbolic",
        );

        let backlinks = gtk::StringList::new(&[]);
        let backlinks_stack = backlinks_body(&backlinks, on_open.clone());
        stack.add_titled_with_icon(
            &backlinks_stack,
            Some("backlinks"),
            "Backlinks",
            BACKLINK_ICON,
        );

        // Lazy fill: a background reindex only flips the flag, so it costs no query while the user
        // is looking at Files or Search.
        stack.connect_visible_child_notify({
            let (dirty, refill) = (tags.dirty.clone(), tags.refill.clone());
            move |stack| {
                if stack.visible_child_name().as_deref() == Some("tags") && dirty.replace(false) {
                    refill();
                }
            }
        });

        // Icons, because four labels do not fit a 200 px sidebar without truncating. The switcher
        // gives every toggle the page title as its tooltip, so icon-only stays discoverable. No
        // vertical alignment of its own: as a header title widget it takes the header's full
        // content height, which is what puts its toggles on the same line as the buttons opposite.
        // Centred rather than filling: a toggle that stretches to the header's height comes out
        // taller than it is wide, and these are square icon buttons.
        let switcher = adw::InlineViewSwitcher::builder()
            .stack(&stack)
            .display_mode(adw::InlineViewSwitcherDisplayMode::Icons)
            .margin_start(6)
            .margin_end(6)
            .valign(gtk::Align::Center)
            .build();
        switcher.add_css_class("flat");

        Sidebar {
            root: stack.clone().upcast(),
            switcher: switcher.upcast(),
            stack,
            search_entry: search.entry,
            backlinks,
            backlinks_stack,
            tags_dirty: tags.dirty,
            select_tag: tags.select,
        }
    }

    /// The pane switcher, the title widget of the sidebar header so that it shares the header band
    /// with the main header instead of taking a band of its own. `AdwHeaderBar` centres a title
    /// widget, so this hands out the switcher itself with no wrapper to do the centring.
    pub fn switcher(&self) -> &gtk::Widget {
        &self.switcher
    }

    /// The panes themselves, for the sidebar column's content.
    pub fn widget(&self) -> &gtk::Widget {
        &self.root
    }

    /// Replace the backlinks list (called when the active tab changes).
    pub fn set_backlinks(&self, notes: &[String]) {
        let refs: Vec<&str> = notes.iter().map(String::as_str).collect();
        self.backlinks
            .splice(0, self.backlinks.n_items(), refs.as_slice());
        self.backlinks_stack
            .set_visible_child_name(if refs.is_empty() { "empty" } else { "list" });
    }

    /// The tag list is out of date; refill it the next time the Tags pane is shown.
    pub fn mark_tags_dirty(&self) {
        self.tags_dirty.set(true);
    }

    /// Show a pane by name: "files", "search", "tags" or "backlinks", focusing its entry where
    /// there is one.
    pub fn show_pane(&self, name: &str) {
        self.stack.set_visible_child_name(name);
        if name == "search" {
            self.search_entry.grab_focus();
        }
    }

    /// Show the Tags pane with `tag` already selected.
    pub fn show_tag(&self, tag: &str) {
        // Ordering matters: this refills the tag list if it is dirty, and the refill clears the
        // selection, so the tag has to be picked afterwards.
        self.show_pane("tags");
        (self.select_tag)(tag);
    }

    /// The visible pane's name, for session state. A stack always has a visible child once it has
    /// pages, so the fallback only covers the impossible case.
    pub fn pane(&self) -> String {
        self.stack
            .visible_child_name()
            .map_or_else(|| "files".to_string(), Into::into)
    }
}

// --- pure helpers, the only part of this module the tests can reach ------------------------------

/// FTS5 wraps matched terms in `«` and `»` (see `Index::search`). Escape first, so a note holding a
/// literal `<` or `&` cannot corrupt the markup, and only then turn the markers into bold. A note
/// can contain those guillemets itself, so nesting is counted rather than substituted blindly and
/// an unclosed run is closed at the end; the result always parses.
fn snippet_markup(snippet: &str) -> String {
    let escaped = glib::markup_escape_text(snippet);
    let mut out = String::with_capacity(escaped.len() + 7);
    let mut depth = 0usize;
    for c in escaped.chars() {
        match c {
            '«' => {
                if depth == 0 {
                    out.push_str("<b>");
                }
                depth += 1;
            }
            '»' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    out.push_str("</b>");
                }
            }
            _ => out.push(c),
        }
    }
    if depth > 0 {
        out.push_str("</b>");
    }
    out
}

/// A note without frontmatter or a heading has no title, so the path is the only name it has.
fn row_title(hit: &SearchHit) -> &str {
    match hit.title.as_deref() {
        Some(t) if !t.trim().is_empty() => t,
        _ => &hit.rel_path,
    }
}

// --- widgets ------------------------------------------------------------------------------------

/// A `GtkListView` of plain strings — backlinks and the files carrying a tag are the same row.
fn path_list(model: &gtk::StringList, on_open: Rc<dyn Fn(&str)>) -> gtk::ListView {
    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::Middle)
            .build();
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&label));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        if let (Some(label), Some(s)) = (
            item.child().and_downcast::<gtk::Label>(),
            item.item().and_downcast::<gtk::StringObject>(),
        ) {
            label.set_text(&s.string());
        }
    });

    let view = gtk::ListView::new(
        Some(gtk::SingleSelection::new(Some(model.clone()))),
        Some(factory),
    );
    view.add_css_class("navigation-sidebar");
    view.connect_activate(move |view, pos| {
        if let Some(s) = view
            .model()
            .and_then(|m| m.item(pos))
            .and_downcast::<gtk::StringObject>()
        {
            on_open(&s.string());
        }
    });
    view
}

/// The shared empty state of every pane. A full-size `AdwStatusPage` is drawn for a window, not
/// for a 200 px column: `.compact` takes the icon from 128 to 96 px, drops the title a step and
/// halves the margins from 36 to 24.
///
/// ponytail: libadwaita has no smaller variant than `.compact`, so if it still crowds a narrow
/// sidebar the next dial is an app CSS rule shrinking the icon inside `statuspage.compact`.
fn status_page(icon: &str, title: &str, description: &str) -> adw::StatusPage {
    let page = adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(description)
        .vexpand(true)
        .build();
    page.add_css_class("compact");
    page
}

fn scroller(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    gtk::ScrolledWindow::builder()
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(child)
        .build()
}

struct SearchPane {
    widget: gtk::Widget,
    entry: gtk::SearchEntry,
}

fn search_pane(data: &Rc<Data>, on_open: &Rc<dyn Fn(&str)>) -> SearchPane {
    // ponytail: rows are `glib::BoxedAnyObject`s wrapping a `SearchHit` instead of a GObject with
    // typed properties, the same trade `tree.rs` documents. Define a real item type if the row
    // ever needs bindable state.
    let results = gio::ListStore::new::<glib::BoxedAnyObject>();

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::Middle)
            .build();
        title.add_css_class("heading");
        let snippet = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .lines(2)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        snippet.add_css_class("dim-label");
        let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
        row.append(&title);
        row.append(&snippet);
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let (Some(title), Some(snippet)) = (
            row.first_child().and_downcast::<gtk::Label>(),
            row.last_child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        let Some(boxed) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let hit: Ref<SearchHit> = boxed.borrow();
        title.set_text(row_title(&hit));
        snippet.set_markup(&snippet_markup(&hit.snippet));
    });

    let view = gtk::ListView::new(
        Some(gtk::SingleSelection::new(Some(results.clone()))),
        Some(factory),
    );
    view.add_css_class("navigation-sidebar");
    view.connect_activate({
        let on_open = on_open.clone();
        move |view, pos| {
            if let Some(boxed) = view
                .model()
                .and_then(|m| m.item(pos))
                .and_downcast::<glib::BoxedAnyObject>()
            {
                let rel = boxed.borrow::<SearchHit>().rel_path.clone();
                on_open(&rel);
            }
        }
    });

    let body = gtk::Stack::builder().vexpand(true).build();
    body.add_named(
        &status_page(
            "system-search-symbolic",
            "Search Notes",
            "Type to search every note in this vault.",
        ),
        Some("prompt"),
    );
    body.add_named(
        &status_page(
            "system-search-symbolic",
            "No Results",
            "No note matches this search.",
        ),
        Some("empty"),
    );
    body.add_named(&scroller(&view), Some("results"));
    body.set_visible_child_name("prompt");

    let refresh: Rc<dyn Fn(&str)> = Rc::new({
        let (results, body, data) = (results.clone(), body.clone(), data.clone());
        move |query: &str| {
            if query.trim().is_empty() {
                results.remove_all();
                body.set_visible_child_name("prompt");
                return;
            }
            let hits: Vec<glib::BoxedAnyObject> = (data.search)(query)
                .into_iter()
                .map(glib::BoxedAnyObject::new)
                .collect();
            body.set_visible_child_name(if hits.is_empty() { "empty" } else { "results" });
            results.splice(0, results.n_items(), &hits);
        }
    });

    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Search notes…")
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();

    // Debounce: one pending source at a time, replaced on every keystroke. Clearing the entry is
    // free, so it cancels the pending query and repaints immediately.
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    entry.connect_search_changed(move |entry| {
        if let Some(id) = pending.borrow_mut().take() {
            id.remove();
        }
        let query = entry.text().to_string();
        if query.trim().is_empty() {
            refresh("");
            return;
        }
        let id = glib::timeout_add_local_once(DEBOUNCE, {
            let (refresh, pending) = (refresh.clone(), pending.clone());
            move || {
                *pending.borrow_mut() = None;
                refresh(&query);
            }
        });
        *pending.borrow_mut() = Some(id);
    });

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&entry);
    column.append(&body);
    SearchPane {
        widget: column.upcast(),
        entry,
    }
}

struct TagsPane {
    widget: gtk::Widget,
    /// Set by `mark_tags_dirty`, cleared by the refill the next time the pane is shown.
    dirty: Rc<Cell<bool>>,
    select: Rc<dyn Fn(&str)>,
    refill: Rc<dyn Fn()>,
}

fn tags_pane(data: &Rc<Data>, on_open: &Rc<dyn Fn(&str)>) -> TagsPane {
    let tags = gio::ListStore::new::<glib::BoxedAnyObject>();

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let name = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        name.set_ellipsize(pango::EllipsizeMode::End);
        let count = gtk::Label::builder().xalign(1.0).build();
        count.add_css_class("dim-label");
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&name);
        row.append(&count);
        item.downcast_ref::<gtk::ListItem>()
            .expect("list item")
            .set_child(Some(&row));
    });
    factory.connect_bind(|_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().expect("list item");
        let Some(row) = item.child().and_downcast::<gtk::Box>() else {
            return;
        };
        let (Some(name), Some(count)) = (
            row.first_child().and_downcast::<gtk::Label>(),
            row.last_child().and_downcast::<gtk::Label>(),
        ) else {
            return;
        };
        let Some(boxed) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
            return;
        };
        let tag: Ref<(String, i64)> = boxed.borrow();
        name.set_text(&tag.0);
        count.set_text(&tag.1.to_string());
    });

    // No autoselect: the file list stays hidden until the user actually picks a tag.
    let selection = gtk::SingleSelection::new(Some(tags.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    selection.set_selected(gtk::INVALID_LIST_POSITION);

    let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
    view.add_css_class("navigation-sidebar");

    let files = gtk::StringList::new(&[]);
    let heading = gtk::Label::builder()
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::End)
        .margin_start(12)
        .margin_end(12)
        .margin_top(6)
        .margin_bottom(6)
        .build();
    heading.add_css_class("heading");

    let files_scroller = gtk::ScrolledWindow::builder()
        .height_request(LIST_HEIGHT)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .child(&path_list(&files, on_open.clone()))
        .build();
    let files_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    files_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    files_box.append(&heading);
    files_box.append(&files_scroller);
    files_box.set_visible(false);

    // Selection drives the filter, so a single click picks a tag and the refill's "no selection"
    // hides the list through the same path.
    selection.connect_selected_item_notify({
        let (files, files_box, heading, data) = (
            files.clone(),
            files_box.clone(),
            heading.clone(),
            data.clone(),
        );
        move |selection| {
            let Some(boxed) = selection
                .selected_item()
                .and_downcast::<glib::BoxedAnyObject>()
            else {
                files.splice(0, files.n_items(), &[]);
                files_box.set_visible(false);
                return;
            };
            let name = boxed.borrow::<(String, i64)>().0.clone();
            let rows = (data.files_with_tag)(&name);
            let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
            files.splice(0, files.n_items(), refs.as_slice());
            heading.set_text(&name);
            files_box.set_visible(true);
        }
    });

    let refill: Rc<dyn Fn()> = Rc::new({
        let (tags, selection, data) = (tags.clone(), selection.clone(), data.clone());
        move || {
            let rows: Vec<glib::BoxedAnyObject> = (data.tags)()
                .into_iter()
                .map(glib::BoxedAnyObject::new)
                .collect();
            tags.splice(0, tags.n_items(), &rows);
            selection.set_selected(gtk::INVALID_LIST_POSITION);
        }
    });

    let select: Rc<dyn Fn(&str)> = Rc::new({
        let (tags, selection) = (tags.clone(), selection.clone());
        move |wanted: &str| {
            let found = (0..tags.n_items()).find(|i| {
                tags.item(*i)
                    .and_downcast::<glib::BoxedAnyObject>()
                    .is_some_and(|b| b.borrow::<(String, i64)>().0 == wanted)
            });
            selection.set_selected(found.unwrap_or(gtk::INVALID_LIST_POSITION));
        }
    });

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&scroller(&view));
    column.append(&files_box);

    TagsPane {
        widget: column.upcast(),
        // The first time the pane is shown there is nothing in it yet.
        dirty: Rc::new(Cell::new(true)),
        select,
        refill,
    }
}

fn backlinks_body(model: &gtk::StringList, on_open: Rc<dyn Fn(&str)>) -> gtk::Stack {
    let stack = gtk::Stack::builder().vexpand(true).build();
    stack.add_named(
        &status_page(
            BACKLINK_ICON,
            "No Backlinks",
            "No note links to the open one.",
        ),
        Some("empty"),
    );
    stack.add_named(&scroller(&path_list(model, on_open)), Some("list"));
    stack.set_visible_child_name("empty");
    stack
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(title: Option<&str>) -> SearchHit {
        SearchHit {
            rel_path: "notes/deep/thought.md".into(),
            title: title.map(str::to_string),
            snippet: String::new(),
        }
    }

    #[test]
    fn snippet_escapes_before_marking_up() {
        // `<` inside the note must survive as text, not as the start of a tag.
        let out = snippet_markup("a «b» < c & d");
        assert_eq!(out, "a <b>b</b> &lt; c &amp; d");
        assert!(pango::parse_markup(&out, '\u{0}').is_ok());
    }

    #[test]
    fn snippet_markup_stays_valid_when_markers_are_unbalanced() {
        // A note may contain guillemets of its own, so every shape has to parse.
        for s in ["«open", "close»", "«a «b»", "»«", "«<»", ""] {
            let out = snippet_markup(s);
            assert!(
                pango::parse_markup(&out, '\u{0}').is_ok(),
                "{s:?} -> {out:?}"
            );
        }
    }

    #[test]
    fn row_title_falls_back_to_the_path() {
        assert_eq!(row_title(&hit(Some("Deep Thought"))), "Deep Thought");
        assert_eq!(row_title(&hit(None)), "notes/deep/thought.md");
        assert_eq!(row_title(&hit(Some("  "))), "notes/deep/thought.md");
    }
}
