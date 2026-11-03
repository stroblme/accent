//! A saved search on the page (user decisions 2026-11-03): a `search` block shows the vault's
//! answer to its query, one widget a block, kept up to date as the vault changes, the note itself
//! left out of it.

use super::Page;
use super::text::{Object, Writer};
use accent_core::index::SearchHit;
use gtk::prelude::*;
use gtk::{glib, pango};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

/// What answers a saved search: the vault's rows for a query, at most so many; `None` when the
/// vault could not be asked, which the caller has said why in the log.
pub type Search = Arc<dyn Fn(&str, usize) -> Option<Vec<SearchHit>> + Send + Sync>;

/// The rows asked for whatever a block lists, the most the Search pane lists, so its last row can
/// count what it leaves out.
const ASKED: usize = 100;

/// How long the vault has to be still before the blocks ask again: a first index is some forty
/// thousand changes, and one round of questions after them is enough.
pub(super) const SETTLE: Duration = Duration::from_secs(1);

/// One block on the page, which a reader may click through to its files.
pub(super) struct Block {
    list: glib::WeakRef<gtk::ListBox>,
    query: String,
    limit: usize,
    /// The note the block is in, which its rows leave out.
    host: String,
    /// What each row opens, in order: a file, or a PDF's page.
    targets: RefCell<Vec<String>>,
    /// The query cannot be read, which no change in the vault mends, so it is never asked.
    unread: bool,
    /// A question of this block's on a worker, and whether another was asked meanwhile.
    asking: Cell<bool>,
    again: Cell<bool>,
}

/// Write the block of the saved search `query`, listing at most `limit` rows, on a line of its
/// own; Copy writes its fence.
pub(super) fn insert(w: &mut Writer, query: &str, limit: usize) {
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .halign(gtk::Align::Start)
        .width_request(w.page.text_width())
        .build();
    list.add_css_class("boxed-list");
    // A query that cannot be read says why at once, as the Search pane's box does.
    let read = accent_core::query::parse(query, &chrono::Local::now());
    let block = Rc::new(Block {
        list: list.downgrade(),
        query: query.to_string(),
        limit,
        host: w.page.note.borrow().0.clone(),
        targets: RefCell::default(),
        unread: read.is_err(),
        asking: Cell::new(false),
        again: Cell::new(false),
    });
    let on_open = w.page.on_open.clone();
    let opened = Rc::downgrade(&block);
    list.connect_row_activated(move |_, row| {
        let target = opened.upgrade().and_then(|block| {
            let targets = block.targets.borrow();
            targets.get(usize::try_from(row.index()).ok()?).cloned()
        });
        if let Some(target) = target {
            on_open(&target);
        }
    });
    let source = w.source.trim_end().to_string();
    w.widget(&list, Object::new(format!("search {query}"), source));
    match read {
        Ok(_) => {
            status(&list, "Searching…", false);
            block.ask(w.page.search.clone());
        }
        Err(why) => status(&list, &why.to_string(), true),
    }
    w.page.saved.borrow_mut().push(block);
}

impl Block {
    /// Ask the vault again on a worker and put its answer up, the rows on show until then; one
    /// question at a time, another asked meanwhile following it.
    fn ask(self: &Rc<Self>, search: Search) {
        if self.asking.replace(true) {
            self.again.set(true);
            return;
        }
        let block = self.clone();
        glib::spawn_future_local(async move {
            loop {
                let (query, asking) = (block.query.clone(), search.clone());
                let answer = crate::work::off_thread("saved search", move || asking(&query, ASKED))
                    .await
                    .flatten();
                let Some(list) = block.list.upgrade() else {
                    break;
                };
                if let Some(hits) = answer {
                    block.fill(&list, hits);
                }
                if !block.again.replace(false) {
                    break;
                }
            }
            block.asking.set(false);
        });
    }

    /// `hits` in place of the rows on show: the note's own left out, at most the block's limit,
    /// and a last row counting the rest.
    fn fill(&self, list: &gtk::ListBox, hits: Vec<SearchHit>) {
        list.remove_all();
        let asked_all = hits.len() >= ASKED;
        let hits: Vec<SearchHit> = hits
            .into_iter()
            .filter(|h| h.rel_path != self.host)
            .collect();
        let mut targets = self.targets.borrow_mut();
        targets.clear();
        if hits.is_empty() {
            return status(list, "Nothing in this vault matches this search", false);
        }
        for hit in hits.iter().take(self.limit) {
            list.append(&row(hit));
            targets.push(match hit.page {
                Some(page) => format!("{}#page={page}", hit.rel_path),
                None => hit.rel_path.clone(),
            });
        }
        let more = hits.len().saturating_sub(self.limit);
        // A list that took all the rows asked for may have left more out: a floor, as a count
        // the Search pane's cap cut is.
        let said = match asked_all {
            true => format!("{more}+ more"),
            false => format!("+{more} more"),
        };
        if more > 0 {
            status(list, &said, false);
        }
    }
}

/// Ask every saved search on the page again, as the vault has changed under them; a page out of
/// sight asks once it is shown.
pub(super) fn requery(page: &Page) {
    page.saved
        .borrow_mut()
        .retain(|block| block.list.upgrade().is_some());
    if !page.view.is_mapped() {
        return page.saved_dirty.set(true);
    }
    page.saved_dirty.set(false);
    for block in page.saved.borrow().iter().filter(|block| !block.unread) {
        block.ask(page.search.clone());
    }
}

/// Each block's query and its rows, a row's labels joined by `|`, for a drill.
#[cfg(feature = "bench")]
pub(super) fn probe(page: &Page) -> Vec<(String, Vec<String>)> {
    let labels = |row: gtk::Widget| {
        let mut out = Vec::new();
        let mut widgets = vec![row];
        while let Some(widget) = widgets.pop() {
            if let Some(label) = widget.downcast_ref::<gtk::Label>()
                && !label.text().is_empty()
            {
                out.push(label.text().to_string());
            }
            let mut child = widget.last_child();
            while let Some(c) = child {
                child = c.prev_sibling();
                widgets.push(c);
            }
        }
        out.join(" | ")
    };
    let blocks = page.saved.borrow();
    let live = blocks.iter().filter_map(|b| Some((b, b.list.upgrade()?)));
    live.map(|(block, list)| {
        let mut rows = Vec::new();
        let mut row = list.first_child();
        while let Some(r) = row {
            row = r.next_sibling();
            rows.push(labels(r));
        }
        (block.query.clone(), rows)
    })
    .collect()
}

/// Every block as wide as the page's text, after a resize or a zoom.
pub(super) fn set_width(page: &Page) {
    let width = page.text_width();
    for block in page.saved.borrow().iter() {
        if let Some(list) = block.list.upgrade() {
            list.set_width_request(width);
        }
    }
}

/// A row as the Search pane draws one: the file's icon, its name and, dim, where in the vault
/// and the file the match is, the matched line under them.
fn row(hit: &SearchHit) -> gtk::ListBoxRow {
    let (name, place, snippet) = crate::sidebar::hit_text(hit);
    let icon = gtk::Image::from_icon_name(crate::doc::icon_for(&hit.rel_path));
    let name = gtk::Label::builder()
        .label(name)
        .xalign(0.0)
        .ellipsize(pango::EllipsizeMode::Middle)
        .build();
    name.add_css_class("heading");
    let place = gtk::Label::builder()
        .label(place)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(pango::EllipsizeMode::Start)
        .build();
    place.add_css_class("dimmed");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    head.append(&icon);
    head.append(&name);
    head.append(&place);
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    column.append(&head);
    if !snippet.is_empty() {
        let line = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .lines(2)
            .ellipsize(pango::EllipsizeMode::End)
            .build();
        line.set_markup(&snippet);
        line.add_css_class("dimmed");
        column.append(&line);
    }
    gtk::ListBoxRow::builder().child(&column).build()
}

/// A row that says something and opens nothing: searching, nothing found, the rest counted, or a
/// query that cannot be read, in the error colour.
fn status(list: &gtk::ListBox, text: &str, error: bool) {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(12)
        .margin_end(12)
        .build();
    label.add_css_class(if error { "error" } else { "dimmed" });
    let row = gtk::ListBoxRow::builder()
        .child(&label)
        .activatable(false)
        .build();
    list.append(&row);
}
