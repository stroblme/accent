//! Ctrl+P file switcher: fuzzy-match every markdown rel_path in the index.

use adw::prelude::*;
use gtk::gdk;
use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use std::cell::RefCell;
use std::rc::Rc;

/// Beyond this the list stops being scannable and nucleo's single-threaded matcher starts to show.
const MAX_RESULTS: usize = 200;

fn rank(files: &[String], query: &str, matcher: &mut Matcher) -> Vec<String> {
    if query.trim().is_empty() {
        return files.iter().take(MAX_RESULTS).cloned().collect();
    }
    let pattern = Pattern::parse(query, CaseMatching::Ignore, Normalization::Smart);
    pattern
        .match_list(files, matcher)
        .into_iter()
        .take(MAX_RESULTS)
        .map(|(s, _)| s.clone())
        .collect()
}

pub fn present(
    parent: &impl IsA<gtk::Widget>,
    files: Vec<String>,
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

    let files = Rc::new(files);
    let refresh = {
        let (model, files, matcher) = (model.clone(), files.clone(), matcher.clone());
        move |query: &str| {
            let hits = rank(&files, query, &mut matcher.borrow_mut());
            let refs: Vec<&str> = hits.iter().map(String::as_str).collect();
            model.splice(0, model.n_items(), &refs);
        }
    };
    refresh("");
    entry.connect_search_changed({
        let refresh = refresh.clone();
        move |e| refresh(&e.text())
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
