//! The Filter button beside the Search box (user decision 2026-11-03): a menu of the qualifiers a
//! ranked query takes, each put in at the box's cursor, so the query language is found by looking
//! rather than remembered.

use accent_core::query::Doc;
use adw::prelude::*;
use gtk::{gio, glib};

/// The action group the menu's items name.
const GROUP: &str = "search-filter";
/// What the Today item writes in place of the day, filled in when it is picked.
const TODAY: &str = "{today}";

/// The Filter menu button, putting what its items write into `entry`.
pub(super) fn button(entry: &gtk::SearchEntry) -> gtk::MenuButton {
    let qualifiers = gio::Menu::new();
    qualifiers.append_item(&item("Tag (#)", "#"));
    qualifiers.append_item(&item("Folder (path:)", "path:"));
    let kinds = gio::Menu::new();
    for doc in Doc::ALL {
        kinds.append_item(&item(kind_label(doc), &format!("kind:{} ", doc.name())));
    }
    qualifiers.append_submenu(Some("Kind (kind:)"), &kinds);
    qualifiers.append_item(&item("Links To (links:)", "links:"));
    qualifiers.append_item(&item("Linked From (linked-from:)", "linked-from:"));
    qualifiers.append_item(&item("Has Highlights (has:highlights)", "has:highlights "));
    let dates = gio::Menu::new();
    dates.append_item(&item("Today", &format!("modified:{TODAY} ")));
    dates.append_item(&item("Last 7 Days", "modified:7d "));
    dates.append_item(&item("Last 30 Days", "modified:30d "));
    qualifiers.append_submenu(Some("Modified (modified:)"), &dates);
    let exclude = gio::Menu::new();
    exclude.append_item(&item("Exclude (-)", "-"));
    let menu = gio::Menu::new();
    menu.append_section(None, &qualifiers);
    menu.append_section(None, &exclude);

    let insert = gio::SimpleAction::new("insert", Some(glib::VariantTy::STRING));
    let asked = entry.downgrade();
    insert.connect_activate(move |_, target| {
        let (Some(entry), Some(text)) = (asked.upgrade(), target.and_then(String::from_variant))
        else {
            return;
        };
        let text = text.replace(TODAY, &chrono::Local::now().format("%Y-%m-%d").to_string());
        // Once the menu has closed and handed the keyboard back to its button.
        glib::idle_add_local_once(move || put(&entry, &text));
    });
    let group = gio::SimpleActionGroup::new();
    group.add_action(&insert);
    let button = gtk::MenuButton::builder()
        .icon_name("search-filter-symbolic")
        .tooltip_text("Filter by Tag, Folder, Kind, Links or Date")
        .menu_model(&menu)
        .valign(gtk::Align::Center)
        .build();
    button.add_css_class("flat");
    button.insert_action_group(GROUP, Some(&group));
    button
}

/// A menu row writing `text`.
fn item(label: &str, text: &str) -> gio::MenuItem {
    let item = gio::MenuItem::new(Some(label), None);
    item.set_action_and_target_value(Some(&format!("{GROUP}.insert")), Some(&text.to_variant()));
    item
}

fn kind_label(doc: Doc) -> &'static str {
    match doc {
        Doc::Note => "Note",
        Doc::Pdf => "PDF",
        Doc::Diagram => "Diagram",
        Doc::Notebook => "Notebook",
        Doc::Code => "Code",
        Doc::Text => "Text",
        Doc::Image => "Image",
    }
}

/// Put `text` in at the box's cursor, the keyboard in the box and the cursor after it.
fn put(entry: &gtk::SearchEntry, text: &str) {
    let mut at = entry.position();
    let text = spaced(&entry.chars(0, at), text);
    // The box selects all it holds when it takes the keyboard, and typing would then replace it.
    match entry.delegate().and_downcast::<gtk::Text>() {
        Some(text) => {
            text.grab_focus_without_selecting();
        }
        None => {
            entry.grab_focus();
        }
    }
    entry.insert_text(&text, &mut at);
    entry.set_position(at);
}

/// `text` with a space before it where it would otherwise run into the word before the cursor:
/// not at the start, after a space, or after a `-` that begins a token, which it then negates.
fn spaced(before: &str, text: &str) -> String {
    let mut back = before.chars().rev();
    let joined = match (back.next(), back.next()) {
        (None, _) => true,
        (Some(c), _) if c.is_whitespace() => true,
        (Some('-'), None) => true,
        (Some('-'), Some(c)) => c.is_whitespace(),
        _ => false,
    };
    match joined {
        true => text.to_string(),
        false => format!(" {text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_filter_is_its_own_token() {
        assert_eq!(spaced("", "#"), "#");
        assert_eq!(spaced("ferris ", "kind:pdf "), "kind:pdf ");
        assert_eq!(spaced("ferris", "kind:pdf "), " kind:pdf ");
        assert_eq!(spaced("ferris -", "#"), "#");
        assert_eq!(spaced("-", "path:"), "path:");
        assert_eq!(spaced("a-", "#"), " #");
    }
}
