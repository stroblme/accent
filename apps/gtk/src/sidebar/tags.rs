//! The Tags pane: every tag in the vault over the files carrying the selected one.

use super::widgets::path_list;
use super::{OnOpen, Target};
use crate::widgets::scroller;
use adw::prelude::*;
use gtk::{gio, glib, pango};
use std::cell::{Cell, Ref, RefCell};
use std::rc::Rc;
use std::sync::Arc;

/// What the Tags pane asks of the index.
///
/// Both run on a worker thread, so they may touch nothing the main loop owns: on a remote vault
/// each is a round trip, and the pane must not hold the window while the host answers.
#[allow(clippy::type_complexity)]
pub struct Data {
    pub tags: Arc<dyn Fn() -> Vec<(String, i64)> + Send + Sync>,
    pub files_with_tag: Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>,
}

/// Case-insensitive substring filtering for the Tags pane. An empty needle keeps everything, so
/// the filter costs nothing until it is typed in.
fn filtered(all: &[(String, i64)], needle: &str) -> Vec<(String, i64)> {
    let needle = needle.trim().to_lowercase();
    all.iter()
        .filter(|(name, _)| needle.is_empty() || name.to_lowercase().contains(&needle))
        .cloned()
        .collect()
}

/// The tag list gets two thirds of the pane, the files under the selected tag the lower third.
pub(super) const SHARE: (i32, i32) = (2, 3);

pub(super) struct Pane {
    pub(super) widget: gtk::Widget,
    /// Kept so a double-click on it can be reset to [`SHARE`].
    pub(super) divider: gtk::Paned,
    /// Set by `mark_tags_dirty`, cleared by the refill the next time the pane is shown.
    pub(super) dirty: Rc<Cell<bool>>,
    pub(super) select: Rc<dyn Fn(&str)>,
    pub(super) refill: Rc<dyn Fn()>,
    /// The tag names on screen and the one picked, which is what `ACCENT_BENCH_TAGS` reads:
    /// nothing else can say whether a refill landed, or whether it kept the reader's place.
    pub(super) names: Rc<dyn Fn() -> Vec<String>>,
    pub(super) picked: Rc<dyn Fn() -> Option<String>>,
}

/// The name of the tag in row `i`, or `None` past the end of the list.
fn name_at(tags: &gio::ListStore, i: u32) -> Option<String> {
    tags.item(i)
        .and_downcast::<glib::BoxedAnyObject>()
        .map(|boxed| boxed.borrow::<(String, i64)>().0.clone())
}

/// Where `wanted` sits in the list as it stands, or `None` when the list does not hold it.
fn position_of(tags: &gio::ListStore, wanted: &str) -> Option<u32> {
    (0..tags.n_items()).find(|i| name_at(tags, *i).as_deref() == Some(wanted))
}

/// The tag the reader has picked, or `None` while none is.
fn selected_name(selection: &gtk::SingleSelection) -> Option<String> {
    selection
        .selected_item()
        .and_downcast::<glib::BoxedAnyObject>()
        .map(|boxed| boxed.borrow::<(String, i64)>().0.clone())
}

pub(super) fn pane(data: &Rc<Data>, on_open: &OnOpen) -> Pane {
    let tags = gio::ListStore::new::<glib::BoxedAnyObject>();

    let factory = crate::widgets::factory(
        |_| {
            let name = gtk::Label::builder().xalign(0.0).hexpand(true).build();
            name.set_ellipsize(pango::EllipsizeMode::End);
            let count = gtk::Label::builder().xalign(1.0).build();
            count.add_css_class("dim-label");
            let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            row.append(&name);
            row.append(&count);
            row
        },
        |row: &gtk::Box, item| {
            let (Some(name), Some(count), Some(boxed)) = (
                row.first_child().and_downcast::<gtk::Label>(),
                row.last_child().and_downcast::<gtk::Label>(),
                item.item().and_downcast::<glib::BoxedAnyObject>(),
            ) else {
                return;
            };
            let tag: Ref<(String, i64)> = boxed.borrow();
            name.set_text(&tag.0);
            count.set_text(&tag.1.to_string());
        },
    );

    // No autoselect: the file list stays hidden until the user actually picks a tag.
    let selection = gtk::SingleSelection::new(Some(tags.clone()));
    selection.set_autoselect(false);
    selection.set_can_unselect(true);
    selection.set_selected(gtk::INVALID_LIST_POSITION);

    let view = gtk::ListView::new(Some(selection.clone()), Some(factory));
    view.add_css_class("navigation-sidebar");

    // Which tag the file list is answering for. The listing lands from a worker thread, so an
    // answer for a tag the user has already clicked past is dropped rather than painted; and a
    // row opened from it names the tag, so the note opens on where it writes it.
    let showing: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));

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

    let files_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    files_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
    files_box.append(&heading);
    files_box.append(&scroller(&path_list(&files, crate::doc::icon_for, {
        let (on_open, showing) = (on_open.clone(), showing.clone());
        move |rel: &str| {
            let tag = showing.borrow().clone();
            on_open(rel, (!tag.is_empty()).then_some(Target::Tag(tag)));
        }
    })));
    files_box.set_visible(false);

    // A paned rather than a fixed height: the two lists share the pane, and where the user puts
    // the divider survives the window being resized.
    let paned = gtk::Paned::builder()
        .orientation(gtk::Orientation::Vertical)
        .start_child(&scroller(&view))
        .end_child(&files_box)
        .resize_start_child(true)
        .resize_end_child(true)
        .shrink_start_child(false)
        .shrink_end_child(false)
        .vexpand(true)
        .build();
    // The default position is set the first time there is anything below the divider, when the
    // pane already knows how tall it is. Afterwards the position is the user's.
    let placed = Cell::new(false);
    // Weak: the paned holds the box this is connected to, and a strong handle would be a cycle
    // keeping the pane, and the vault behind `data`, alive after the window has closed.
    files_box.connect_map(glib::clone!(
        #[weak]
        paned,
        move |_| {
            if !placed.replace(true) {
                paned.set_position(paned.height() * SHARE.0 / SHARE.1);
            }
        }
    ));

    // Selection drives the filter, so a single click picks a tag and the refill's "no selection"
    // hides the list through the same path.
    selection.connect_selected_item_notify({
        let (files, files_box, heading, data) = (
            files.clone(),
            files_box.clone(),
            heading.clone(),
            data.clone(),
        );
        let showing = showing.clone();
        move |selection| {
            let Some(boxed) = selection
                .selected_item()
                .and_downcast::<glib::BoxedAnyObject>()
            else {
                showing.borrow_mut().clear();
                files.splice(0, files.n_items(), &[]);
                files_box.set_visible(false);
                return;
            };
            let name = boxed.borrow::<(String, i64)>().0.clone();
            heading.set_text(&name);
            files_box.set_visible(true);
            *showing.borrow_mut() = name.clone();
            let (files, showing) = (files.clone(), showing.clone());
            let look_up = data.files_with_tag.clone();
            glib::spawn_future_local(async move {
                let wanted = name.clone();
                let listed = crate::work::off_thread("tag", move || look_up(&wanted)).await;
                let Some(rows) = listed else { return };
                if *showing.borrow() != name {
                    return;
                }
                let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
                files.splice(0, files.n_items(), refs.as_slice());
            });
        }
    });

    let filter = gtk::SearchEntry::builder()
        .placeholder_text("Filter tags…")
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();

    // The whole list is kept, so filtering is a splice rather than a query: the tags come from one
    // GROUP BY over the index and re-running it per keystroke would buy nothing.
    let all: Rc<RefCell<Vec<(String, i64)>>> = Rc::new(RefCell::new(Vec::new()));
    // The filter weakly, because its own handler runs this: a strong one is a cycle.
    let apply: Rc<dyn Fn()> = Rc::new({
        let (tags, selection, all) = (tags.clone(), selection.clone(), all.clone());
        glib::clone!(
            #[weak]
            filter,
            move || {
                // What the user had picked, so a refill behind them — a note saved with a tag
                // added or gone — puts the same tag back instead of snapping the file list shut
                // under the pointer. A tag the refill dropped, or one the filter now hides,
                // selects nothing, which is the "no selection" the file list already hides on.
                let picked = selected_name(&selection);
                let rows: Vec<glib::BoxedAnyObject> = filtered(&all.borrow(), &filter.text())
                    .into_iter()
                    .map(glib::BoxedAnyObject::new)
                    .collect();
                tags.splice(0, tags.n_items(), &rows);
                let found = picked.and_then(|name| position_of(&tags, &name));
                selection.set_selected(found.unwrap_or(gtk::INVALID_LIST_POSITION));
            }
        )
    });
    filter.connect_search_changed({
        let apply = apply.clone();
        move |_| apply()
    });

    // One GROUP BY over the index on a local vault, a round trip on a remote one: asked for off
    // the main loop, and spliced in when it lands.
    let refill: Rc<dyn Fn()> = Rc::new({
        let (all, apply, data) = (all.clone(), apply.clone(), data.clone());
        move || {
            let (all, apply, tags) = (all.clone(), apply.clone(), data.tags.clone());
            glib::spawn_future_local(async move {
                let Some(rows) = crate::work::off_thread("tag", move || tags()).await else {
                    return;
                };
                *all.borrow_mut() = rows;
                apply();
            });
        }
    });

    let select: Rc<dyn Fn(&str)> = Rc::new({
        let (tags, selection, filter) = (tags.clone(), selection.clone(), filter.clone());
        move |wanted: &str| {
            // The tag the caller wants may be filtered out of the list; clearing the filter puts
            // every tag back before it is looked for.
            filter.set_text("");
            let found = position_of(&tags, wanted);
            selection.set_selected(found.unwrap_or(gtk::INVALID_LIST_POSITION));
        }
    });

    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&filter);
    column.append(&paned);

    let names: Rc<dyn Fn() -> Vec<String>> = Rc::new({
        let tags = tags.clone();
        move || {
            (0..tags.n_items())
                .filter_map(|i| name_at(&tags, i))
                .collect()
        }
    });

    let picked: Rc<dyn Fn() -> Option<String>> = Rc::new({
        let selection = selection.clone();
        move || selected_name(&selection)
    });

    Pane {
        divider: paned.clone(),
        widget: column.upcast(),
        // The first time the pane is shown there is nothing in it yet.
        dirty: Rc::new(Cell::new(true)),
        select,
        refill,
        names,
        picked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_filter_is_a_case_insensitive_substring() {
        let all = [
            ("rust".to_string(), 3),
            ("Rustaceans".to_string(), 1),
            ("go".to_string(), 2),
        ];
        assert_eq!(filtered(&all, "").len(), 3);
        assert_eq!(filtered(&all, " RUST ").len(), 2);
        assert_eq!(filtered(&all, "ace")[0].0, "Rustaceans");
        assert!(filtered(&all, "zzz").is_empty());
    }
}
