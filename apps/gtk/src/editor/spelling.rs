//! The spell checker of a view of prose, libspelling's, and the suggestions its text menu holds.
//!
//! libspelling's menu is about the word at the caret, whatever is selected, and its Correct does
//! nothing while anything is: so the view shows a menu of its own, whose word part each opening
//! fills from libspelling's for that word only while the selection is nothing or the word itself
//! ([`suggest`]), a correction then going over the selection ([`corrections`]).

use super::lines;
use crate::multicaret;
use gtk::{gdk, gio, glib};
use sourceview5::prelude::*;
use std::cell::RefCell;

/// Attach a spell checker and suggestion-menu action group to `buffer`
/// and `view`, initially disabled.
pub fn spell_adapter(
    buffer: &sourceview5::Buffer,
    view: &sourceview5::View,
) -> libspelling::TextBufferAdapter {
    let adapter = libspelling::TextBufferAdapter::new(buffer, &libspelling::Checker::default());
    view.insert_action_group("spelling", Some(&adapter));
    view.insert_action_group("spell", Some(&corrections(&adapter, buffer)));
    let model = items_of(&adapter);
    // Check Spelling and Languages stay; the word's part is filled as the menu opens.
    let word = gio::Menu::new();
    let menu = gio::Menu::new();
    menu.append_section(None, &word);
    for i in (0..model.n_items()).filter(|&i| !of_the_word(&model, i)) {
        menu.append_item(&gio::MenuItem::from_model(&model, i));
    }
    view.set_extra_menu(Some(&menu));
    // The suggestions are the caret's word's, and a secondary click puts the caret on the word
    // clicked ([`caret_to_click`]): asked again at once rather than after the adapter's own pause
    // on a caret move, so the menu opening on this press already holds them. The caret is put
    // here too, whichever of the two presses runs first.
    //
    // [`caret_to_click`]: lines::caret_to_click
    let click = gtk::GestureClick::builder()
        .button(gdk::BUTTON_SECONDARY)
        .propagation_phase(gtk::PropagationPhase::Capture)
        .build();
    let (weak, words) = (adapter.downgrade(), word.clone());
    click.connect_pressed(move |click, presses, x, y| {
        if let (Some(view), Some(adapter)) = (
            click.widget().and_downcast::<sourceview5::View>(),
            weak.upgrade(),
        ) && presses == 1
        {
            lines::place_at_click(&view, x, y);
            suggest(&view, &adapter, &words);
        }
    });
    view.add_controller(click);
    // The keys that open the menu where the caret is (GtkTextView's `menu.popup`).
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = adapter.downgrade();
    keys.connect_key_pressed(move |keys, key, _, state| {
        let menu_key = key == gdk::Key::Menu
            || (key == gdk::Key::F10 && state == gdk::ModifierType::SHIFT_MASK);
        if let (true, Some(view), Some(adapter)) = (
            menu_key,
            keys.widget().and_downcast::<sourceview5::View>(),
            weak.upgrade(),
        ) {
            suggest(&view, &adapter, &word);
        }
        glib::Propagation::Proceed
    });
    view.add_controller(keys);
    adapter
}

/// Turn spell checking on or off in a view of prose, its adapter kept in `slot` and made the
/// first time it is turned on.
pub(crate) fn set_spelling(
    slot: &RefCell<Option<libspelling::TextBufferAdapter>>,
    buffer: &sourceview5::Buffer,
    view: &sourceview5::View,
    on: bool,
) {
    // Cloned out first: the adapter is created inside the `else`, which borrows mutably.
    let existing = slot.borrow().clone();
    let adapter = match existing {
        Some(adapter) => adapter,
        None if !on => return,
        None => {
            let adapter = spell_adapter(buffer, view);
            *slot.borrow_mut() = Some(adapter.clone());
            adapter
        }
    };
    adapter.set_enabled(on);
}

/// libspelling's menu: the one section of the model it hands out.
fn items_of(adapter: &libspelling::TextBufferAdapter) -> gio::MenuModel {
    let top = adapter.menu_model();
    top.item_link(0, gio::MENU_LINK_SECTION).unwrap_or(top)
}

/// Whether item `i` of libspelling's menu is about the word at the caret: its corrections'
/// section, Add to Dictionary or Ignore.
fn of_the_word(model: &gio::MenuModel, i: i32) -> bool {
    let action = model
        .item_attribute_value(i, gio::MENU_ATTRIBUTE_ACTION, Some(glib::VariantTy::STRING))
        .and_then(|a| a.str().map(str::to_string));
    model.item_link(i, gio::MENU_LINK_SECTION).is_some()
        || matches!(action.as_deref(), Some("spelling.add" | "spelling.ignore"))
}

/// Fill `word` with libspelling's suggestions for the word at the caret, Add to Dictionary and
/// Ignore, while what is selected is nothing or that word: a selection of more, or a column of
/// carets, is not one word to correct, and leaves it empty.
fn suggest(view: &sourceview5::View, adapter: &libspelling::TextBufferAdapter, word: &gio::Menu) {
    word.remove_all();
    let column = view
        .downcast_ref::<multicaret::View>()
        .is_some_and(|view| view.has_carets());
    if column || !one_word(view.buffer().selection_bounds()) {
        return;
    }
    adapter.update_corrections();
    let model = items_of(adapter);
    let corrections = gio::Menu::new();
    for i in (0..model.n_items()).filter(|&i| of_the_word(&model, i)) {
        let Some(section) = model.item_link(i, gio::MENU_LINK_SECTION) else {
            word.append_item(&gio::MenuItem::from_model(&model, i));
            continue;
        };
        // libspelling's own, retargeted at `spell.correct`, which takes a selection too.
        for j in 0..section.n_items() {
            let item = gio::MenuItem::from_model(&section, j);
            let target = section.item_attribute_value(j, gio::MENU_ATTRIBUTE_TARGET, None);
            item.set_action_and_target_value(Some("spell.correct"), target.as_ref());
            corrections.append_item(&item);
        }
    }
    word.insert_section(0, None, &corrections);
}

/// Whether `selected`, the selection's bounds if any, is nothing or a single word.
fn one_word(selected: Option<(gtk::TextIter, gtk::TextIter)>) -> bool {
    let Some((start, end)) = selected else {
        return true;
    };
    let mut word = start;
    // At the end of the buffer it says it did not move, however far it went.
    word.forward_word_end();
    start.starts_word() && word == end
}

/// `spell.correct`, a suggestion: written over the selection where there is one, the word the
/// menu was opened on, and otherwise libspelling's own, which corrects the word at the caret.
fn corrections(
    adapter: &libspelling::TextBufferAdapter,
    buffer: &sourceview5::Buffer,
) -> gio::SimpleActionGroup {
    let correct = gio::SimpleAction::new("correct", Some(glib::VariantTy::STRING));
    correct.connect_activate(glib::clone!(
        #[weak]
        adapter,
        #[weak]
        buffer,
        move |_, word| match (buffer.selection_bounds(), word.and_then(|w| w.str())) {
            (Some((mut start, mut end)), Some(word)) => {
                buffer.begin_user_action();
                buffer.delete(&mut start, &mut end);
                buffer.insert(&mut start, word);
                buffer.end_user_action();
            }
            _ => adapter.activate_action("correct", word),
        }
    ));
    let group = gio::SimpleActionGroup::new();
    group.add_action(&correct);
    group
}
