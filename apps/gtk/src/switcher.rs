//! Ctrl+P file switcher: fuzzy-match every markdown rel_path in the index.
//!
//! Opening must be instant, so the dialog goes up showing the most recently modified notes (one
//! indexed query, no matching) and only pulls the full note list the first time the user actually
//! types. Keystrokes are debounced, so holding a key down cannot queue up one full match per
//! character.

use adw::prelude::*;
use gtk::gdk;
use gtk::glib;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Beyond this the list stops being scannable and nucleo's single-threaded matcher starts to show.
const MAX_RESULTS: usize = 200;
/// Long enough to swallow a burst of keystrokes, short enough to feel immediate.
const DEBOUNCE: Duration = Duration::from_millis(50);

fn rank(files: &[String], query: &str, matcher: &mut Matcher) -> Vec<String> {
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    pattern
        .match_list(files, matcher)
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(s, _)| s.clone())
        .collect()
}

/// `recent` is shown until the first keystroke; `load_all` is called at most once, lazily, to get
/// the corpus to match against.
pub fn present(
    parent: &impl IsA<gtk::Widget>,
    recent: Vec<String>,
    load_all: impl Fn() -> Vec<String> + 'static,
    on_pick: impl Fn(&str) + 'static,
) {
    let matcher = Rc::new(RefCell::new(Matcher::new(Config::DEFAULT.match_paths())));
    let model = gtk::StringList::new(&[]);
    let selection = gtk::SingleSelection::new(Some(model.clone()));

    let factory = gtk::SignalListItemFactory::new();
    factory.connect_setup(|_, item| {
        let label = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .margin_top(6)
            .margin_bottom(6)
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

    let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
    list.set_single_click_activate(true);
    let scroller = gtk::ScrolledWindow::builder()
        .vexpand(true)
        .child(&list)
        .build();

    let entry = gtk::SearchEntry::builder()
        .placeholder_text("Search notes…")
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&entry);
    content.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    content.append(&scroller);

    let dialog = adw::Dialog::builder()
        .title("Open note")
        .content_width(560)
        .content_height(420)
        .child(&content)
        .build();

    let recent = Rc::new(recent);
    // Filled on the first non-empty query, then reused for the life of the dialog.
    let corpus: Rc<RefCell<Option<Rc<Vec<String>>>>> = Rc::new(RefCell::new(None));
    let load_all = Rc::new(load_all);

    let refresh = Rc::new({
        let (model, recent, corpus, matcher, load_all, selection) = (
            model.clone(),
            recent.clone(),
            corpus.clone(),
            matcher.clone(),
            load_all.clone(),
            selection.clone(),
        );
        move |query: &str| {
            let t0 = Instant::now();
            let hits: Vec<String> = if query.trim().is_empty() {
                recent.iter().take(MAX_RESULTS).cloned().collect()
            } else {
                // Borrow, clone the handle, drop: `load_all` reaches into the index and must not
                // run while `corpus` is borrowed.
                let cached = corpus.borrow().clone();
                let files = match cached {
                    Some(f) => f,
                    None => {
                        let f = Rc::new(load_all());
                        *corpus.borrow_mut() = Some(f.clone());
                        f
                    }
                };
                let mut m = matcher.borrow_mut();
                rank(&files, query, &mut m)
            };
            let refs: Vec<&str> = hits.iter().map(String::as_str).collect();
            model.splice(0, model.n_items(), &refs);
            if !refs.is_empty() {
                selection.set_selected(0);
            }
            tracing::debug!(
                query,
                hits = refs.len(),
                ms = t0.elapsed().as_secs_f64() * 1e3,
                "switcher query"
            );
        }
    });
    refresh("");

    // Debounce: one pending source at a time, replaced on every keystroke.
    let pending: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    entry.connect_search_changed({
        let (refresh, pending) = (refresh.clone(), pending.clone());
        move |e| {
            if let Some(id) = pending.borrow_mut().take() {
                id.remove();
            }
            let query = e.text().to_string();
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
        let (dialog, selection, on_pick) = (dialog.clone(), selection.clone(), on_pick.clone());
        move || {
            if let Some(s) = selection
                .selected_item()
                .and_downcast::<gtk::StringObject>()
            {
                dialog.close();
                on_pick(&s.string());
            }
        }
    };
    entry.connect_activate({
        let pick = pick.clone();
        move |_| pick()
    });
    list.connect_activate({
        let pick = pick.clone();
        move |_, _| pick()
    });

    // Arrow keys move the list selection while the entry keeps focus.
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(move |_, key, _, _| {
        let n = selection.n_items();
        let cur = selection.selected();
        match key {
            gdk::Key::Down if n > 0 => selection.set_selected((cur + 1).min(n - 1)),
            gdk::Key::Up if n > 0 => selection.set_selected(cur.saturating_sub(1)),
            _ => return glib_propagate(),
        }
        list.scroll_to(selection.selected(), gtk::ListScrollFlags::NONE, None);
        gtk::glib::Propagation::Stop
    });
    entry.add_controller(keys);

    dialog.present(Some(parent));
    entry.grab_focus();
}

fn glib_propagate() -> gtk::glib::Propagation {
    gtk::glib::Propagation::Proceed
}
