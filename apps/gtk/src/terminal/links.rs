//! Links in a shell's output, and the context menu that offers them.

use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use vte4::TerminalExt;

use super::Term;

/// Local action group for pointer-targeted URLs, which have no window shortcut or palette action.
const GROUP: &str = "link";
/// What counts as a link on the screen: an `http`, `https` or `mailto` URL, stopping before the
/// punctuation that ends a sentence rather than swallowing it.
const LINK: &str = r"(?:https?://|mailto:)[^\s<>\x22'`]*[^\s<>\x22'`.,:;!?)\]}]";
/// `PCRE2_MULTILINE`. vte4 re-exports no PCRE2 flags, so the value is written out here; a match
/// has to be able to end at a wrapped line rather than only at the end of the buffer.
const PCRE2_MULTILINE: u32 = 0x0000_0400;

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
    view.insert_action_group(GROUP, Some(&link_actions(view)));

    let menu = gio::Menu::new();
    fill_menu(&menu, None, term.share_state());
    view.set_context_menu_model(Some(&menu));

    // Capture phase, so the model is rebuilt before VTE reads it to pop the menu up.
    let secondary = gtk::GestureClick::new();
    secondary.set_button(gdk::BUTTON_SECONDARY);
    secondary.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(term);
    secondary.connect_pressed(glib::clone!(
        #[weak(rename_to = under)]
        view,
        move |_, _, x, y| {
            let Some(term) = weak.upgrade() else { return };
            let link = link_at(&under, x, y);
            fill_menu(&menu, link.as_deref(), term.share_state());
            if term.share_vault.is_some() {
                let menu = menu.clone();
                term.ask(move |term| fill_menu(&menu, link.as_deref(), term.share_state()));
            }
        }
    ));
    view.add_controller(secondary);

    // Ctrl and the primary button, which is what GNOME Terminal asks for and what a PDF link here
    // already asks for. A plain click cannot have it: VTE claims the sequence for its own
    // selection, which cancels any gesture of ours before the release, and a press that both
    // starts a selection and launches a browser is the wrong gesture in any case.
    let primary = gtk::GestureClick::new();
    primary.set_button(gdk::BUTTON_PRIMARY);
    primary.set_propagation_phase(gtk::PropagationPhase::Capture);
    primary.connect_pressed(glib::clone!(
        #[weak(rename_to = under)]
        view,
        move |gesture, _, x, y| {
            if gesture
                .current_event_state()
                .contains(gdk::ModifierType::CONTROL_MASK)
                && let Some(uri) = link_at(&under, x, y)
            {
                launch(&uri);
            }
        }
    ));
    view.add_controller(primary);
}

/// What the menu holds: the clipboard, then the tab, then the link under the pointer when there
/// is one. No Select All — VTE binds nothing for it — and no Split, which is a tab gesture the
/// tab strip already offers.
fn fill_menu(menu: &gio::Menu, link: Option<&str>, shared: Option<bool>) {
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
    let Some(uri) = link else {
        return;
    };
    let links = gio::Menu::new();
    for (label, action) in [("Open Link", "open"), ("Copy Link Address", "copy")] {
        let item = gio::MenuItem::new(Some(label), None);
        item.set_action_and_target_value(
            Some(&format!("{GROUP}.{action}")),
            Some(&uri.to_variant()),
        );
        links.append_item(&item);
    }
    menu.append_section(None, &links);
}

/// The two link items' actions, each taking the URL as its parameter, as the tree's row menu does
/// with a path: a detailed-action string would have to quote it.
fn link_actions(view: &vte4::Terminal) -> gio::SimpleActionGroup {
    let group = gio::SimpleActionGroup::new();
    let open = gio::SimpleAction::new("open", Some(glib::VariantTy::STRING));
    open.connect_activate(|_, target| {
        if let Some(uri) = target.and_then(|t| t.str()) {
            launch(uri);
        }
    });
    group.add_action(&open);
    let copy = gio::SimpleAction::new("copy", Some(glib::VariantTy::STRING));
    copy.connect_activate(glib::clone!(
        #[weak]
        view,
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
}
