//! The card a held `Ctrl+Tab` puts over its pane (DESIGN.md, Tabs): the pane's tabs in the order
//! they were last used, the one letting go of Ctrl lands on highlighted.
//!
//! A view over what the pane already keeps — the order is `Pane::recent`, the cursor
//! `Pane::cycling` — filled by the window, which knows what each tab holds. It never takes the
//! keyboard, so the chord's own keys, Tab pressed again and Ctrl let go, still reach the window's
//! handlers whatever has the focus, a shell included.

use crate::widgets::{self, Debounce};
use adw::prelude::*;
use gtk::{gio, pango};
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

/// How long the chord is held before the card shows: a quick flip to the last tab never shows it.
const DELAY: Duration = Duration::from_millis(200);
/// The most tabs the card lists at once; past that it scrolls to keep the highlighted one in it.
const ROWS: usize = 10;

/// One tab as the card lists it: the file lists' row, icon, name and folder.
pub struct Entry {
    pub icon: Option<gio::Icon>,
    pub name: String,
    pub folder: String,
}

pub struct Switcher {
    list: gtk::ListBox,
    /// The entry the card's first row shows, where there are more than fit.
    first: Rc<Cell<usize>>,
    soon: Debounce,
}

impl Switcher {
    pub fn new() -> Switcher {
        // `.navigation-sidebar` is the palette's row; `.accent-switcher` the popover's surface,
        // opaque over the document (`build::install_chrome_css`).
        let list = gtk::ListBox::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .can_focus(false)
            .visible(false)
            .css_classes(["navigation-sidebar", "accent-switcher"])
            .build();
        Switcher {
            list,
            first: Rc::new(Cell::new(0)),
            soon: Debounce::new(DELAY),
        }
    }

    pub fn widget(&self) -> &gtk::Widget {
        self.list.upcast_ref()
    }

    pub fn shown(&self) -> bool {
        self.list.is_visible()
    }

    /// Run `show` once the chord has been held for [`DELAY`], unless that is already coming.
    pub fn soon(&self, show: impl FnOnce() + 'static) {
        self.soon.call_once(show);
    }

    /// List `entries` with the one at `at` highlighted, as many as the pane has room for.
    pub fn show(&self, entries: &[Entry], at: usize) {
        let Some(probe) = entries.first() else {
            return self.hide();
        };
        let fresh = !self.shown();
        self.list.set_visible(true);
        self.list.remove_all();
        let fit = self.fit(probe);
        let from = if fresh { 0 } else { self.first.get() };
        let first = window(entries.len(), fit, at, from);
        self.first.set(first);
        for entry in entries.iter().skip(first).take(fit) {
            self.list.append(&row(entry));
        }
        let selected = i32::try_from(at - first).ok();
        self.list
            .select_row(selected.and_then(|i| self.list.row_at_index(i)).as_ref());
        if fresh {
            widgets::fade_in(&self.list);
        }
    }

    pub fn hide(&self) {
        self.soon.cancel();
        if self.shown() {
            self.list.set_visible(false);
            self.list.remove_all();
        }
    }

    /// Call `pick` with the entry a click on the card chose.
    pub fn connect_pick(&self, pick: impl Fn(usize) + 'static) {
        let first = self.first.clone();
        self.list.connect_row_activated(move |_, row| {
            if let Ok(i) = usize::try_from(row.index()) {
                pick(first.get() + i);
            }
        });
    }

    /// How many rows the pane's height holds, at most [`ROWS`]: measured, the theme deciding how
    /// tall a row and the card around it are. One at least, the highlighted one.
    fn fit(&self, probe: &Entry) -> usize {
        let room = self.list.parent().map_or(0, |pane| pane.height());
        let row = row(probe);
        self.list.append(&row);
        let (_, card, _, _) = self.list.measure(gtk::Orientation::Vertical, -1);
        let (_, step, _, _) = row.measure(gtk::Orientation::Vertical, -1);
        self.list.remove(&row);
        // Not laid out yet, which only a pane that has never been drawn is.
        if room <= 0 || step <= 0 {
            return ROWS;
        }
        let more = usize::try_from((room - card) / step).unwrap_or(0);
        (1 + more).min(ROWS)
    }

    /// The names on the card and which row is highlighted, for `ACCENT_BENCH_TABS=cycle:`.
    #[cfg(feature = "bench")]
    pub fn rows(&self) -> (Vec<String>, Option<usize>) {
        let mut names = Vec::new();
        let mut child = self.list.first_child();
        while let Some(row) = child.and_downcast::<gtk::ListBoxRow>() {
            let name = row
                .child()
                .and_then(|line| line.first_child())
                .and_then(|icon| icon.next_sibling())
                .and_downcast::<gtk::Label>();
            names.push(name.map(|n| n.text().to_string()).unwrap_or_default());
            child = row.next_sibling();
        }
        let at = self
            .list
            .selected_row()
            .and_then(|row| usize::try_from(row.index()).ok());
        (names, at)
    }

    /// Where the card's `i`th row is, in the coordinates of `to`, for a click at its middle.
    #[cfg(feature = "bench")]
    pub fn row_centre(&self, i: usize, to: &impl IsA<gtk::Widget>) -> Option<(f64, f64)> {
        let row = self.list.row_at_index(i32::try_from(i).ok()?)?;
        if row.width() == 0 {
            return None;
        }
        let at = row.compute_point(to, &gtk::graphene::Point::new(0.0, 0.0))?;
        Some((
            f64::from(at.x()) + f64::from(row.width()) / 2.0,
            f64::from(at.y()) + f64::from(row.height()) / 2.0,
        ))
    }
}

fn row(entry: &Entry) -> gtk::ListBoxRow {
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let icon = gtk::Image::new();
    if let Some(gicon) = &entry.icon {
        icon.set_from_gicon(gicon);
    }
    line.append(&icon);
    line.append(
        &gtk::Label::builder()
            .label(&entry.name)
            .xalign(0.0)
            .ellipsize(pango::EllipsizeMode::End)
            .max_width_chars(40)
            .build(),
    );
    line.append(
        &gtk::Label::builder()
            .label(&entry.folder)
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(pango::EllipsizeMode::Middle)
            .max_width_chars(40)
            .css_classes(["dim-label"])
            .build(),
    );
    gtk::ListBoxRow::builder().child(&line).build()
}

/// The entry the card's first row shows, out of `len` with room for `fit`: moved from `first`
/// just far enough to keep `at` on the card, as a list scrolls to its cursor.
fn window(len: usize, fit: usize, at: usize, first: usize) -> usize {
    let first = match at {
        at if at < first => at,
        at if at >= first + fit => at + 1 - fit,
        _ => first,
    };
    first.min(len.saturating_sub(fit))
}

#[cfg(test)]
mod tests {
    use super::window;

    #[test]
    fn the_card_scrolls_only_as_far_as_the_cursor_needs() {
        // Fewer tabs than rows: every one is on the card, wherever the cursor is.
        assert_eq!(window(4, 10, 3, 0), 0);
        // Fifteen tabs, ten rows: stepping down the first ten moves nothing...
        assert_eq!(window(15, 10, 9, 0), 0);
        // ...the eleventh scrolls by one, and the cursor stays on the last row.
        assert_eq!(window(15, 10, 10, 0), 1);
        // Stepping back up inside the card leaves it where it is.
        assert_eq!(window(15, 10, 5, 1), 1);
        // Round from the end to the front, and back from the front to the end.
        assert_eq!(window(15, 10, 0, 5), 0);
        assert_eq!(window(15, 10, 14, 0), 5);
        // A pane with room for one row shows the cursor alone.
        assert_eq!(window(15, 1, 7, 0), 7);
    }
}
