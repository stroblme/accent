//! Quick Fix: what a language server offers over the selection or at the caret, in a menu at
//! the caret, and the edits the one picked makes (DESIGN.md, Language servers).

use super::*;
use accent_api::{ActionEdits, CodeAction, Range, TextEdit};

impl App {
    /// Ask the active text tab's server for its fixes and refactorings over the selection, or at
    /// the caret, and list them in a menu there.
    pub fn quick_fix(self: &Rc<Self>) {
        if self.active_notebook().is_some() {
            return self.toast("Quick fixes are not offered in notebook cells yet");
        }
        let Some(tab) = self.active() else {
            return;
        };
        let Some(vault) = tab.lang.vault() else {
            return self.needs_vault("list quick fixes");
        };
        if let Some(server) = tab.lang.support().and_then(|s| s.missing.clone()) {
            let language = tab.language().unwrap_or_else(|| "this file".to_string());
            return self.toast(&format!(
                "No language server for {language} ({server} not found)"
            ));
        }
        if !tab.view.is_editable() {
            return self.cannot("apply a quick fix", "the file is read-only");
        }
        // At the primary caret alone, as `Ctrl+Space` asks, and with nothing else over the text.
        if let Some(column) = tab.view.downcast_ref::<multicaret::View>() {
            column.clear_carets();
        }
        if let Some(session) = completion::session(&tab) {
            session.close();
        }
        tab.lang.signature.dismiss();
        let caret = editor::caret(&tab.buffer);
        let (from, to) = tab.buffer.selection_bounds().unwrap_or((caret, caret));
        let range = Range {
            start: lang::pos_of(&from),
            end: lang::pos_of(&to),
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            lang::flush(tab.clone()).await;
            let asked = tab.lang.doc.version();
            let found = vault.code_actions(&tab.language_key(), range).await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            // Typed past: the answer is about text that is not there any more.
            if tab.lang.doc.version() != asked {
                return;
            }
            match found {
                Ok(actions) => app.offer_fixes(&tab, actions),
                Err(e) => app.cannot("list quick fixes", e),
            }
        });
    }

    /// The menu of `actions` at `tab`'s caret, in [`sections`]; picking one applies it.
    fn offer_fixes(self: &Rc<Self>, tab: &Rc<Tab>, actions: Vec<CodeAction>) {
        let sections = sections(actions);
        if sections.iter().all(Vec::is_empty) {
            return self.toast("No quick fixes here");
        }
        let menu = gio::Menu::new();
        let listed: Vec<CodeAction> = sections.concat();
        let mut at = 0u32;
        for section in sections.iter().filter(|s| !s.is_empty()) {
            let items = gio::Menu::new();
            for action in section {
                // A menu reads `_` as a mnemonic: `snake_case` in a title must stay itself.
                let item = gio::MenuItem::new(Some(&action.title.replace('_', "__")), None);
                item.set_action_and_target_value(Some("fix.apply"), Some(&at.to_variant()));
                items.append_item(&item);
                at += 1;
            }
            menu.append_section(None, &items);
        }
        let apply = gio::SimpleAction::new("apply", Some(glib::VariantTy::UINT32));
        let (app, picked) = (Rc::downgrade(self), Rc::downgrade(tab));
        apply.connect_activate(move |_, target| {
            let index = target.and_then(u32::from_variant).map(|i| i as usize);
            if let (Some(app), Some(tab), Some(index)) = (app.upgrade(), picked.upgrade(), index)
                && let Some(action) = listed.get(index)
            {
                app.apply_fix(&tab, action.clone());
            }
        });
        let group = gio::SimpleActionGroup::new();
        group.add_action(&apply);
        // On the view before the menu is up: an item whose action is not found yet starts greyed.
        tab.view.insert_action_group("fix", Some(&group));
        let at = tab.view.iter_location(&editor::caret(&tab.buffer));
        let (x, y) = tab
            .view
            .buffer_to_window_coords(gtk::TextWindowType::Widget, at.x(), at.y());
        let anchor = gdk::Rectangle::new(x, y, 1, at.height());
        widgets::popup_menu(&tab.view, &menu, Some(anchor));
    }

    /// Apply `action`, picked in `tab`: resolved or run at its server first, then its edits made.
    pub(crate) fn apply_fix(self: &Rc<Self>, tab: &Rc<Tab>, action: CodeAction) {
        let Some(vault) = tab.lang.vault() else {
            return;
        };
        let (weak, tab) = (Rc::downgrade(self), tab.clone());
        glib::spawn_future_local(async move {
            lang::flush(tab.clone()).await;
            let asked = tab.lang.doc.version();
            let edits = vault.code_action_edits(&tab.language_key(), action).await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            match edits {
                Ok(edits) => app.make_edits(&tab, asked, vault, edits),
                Err(e) => app.cannot("apply the fix", e),
            }
        });
    }

    /// Make `edits`: each open file's in its tab as one undo step, all of them or none, then the
    /// files no tab holds on disk, gated on their etags. `asked` is the version of `tab`'s text
    /// the server answered about.
    fn make_edits(
        self: &Rc<Self>,
        tab: &Rc<Tab>,
        asked: u64,
        vault: Arc<Vault>,
        edits: ActionEdits,
    ) {
        let mut open = Vec::new();
        for (rel, list) in edits.open {
            let Some(target) = self.tab_for(&rel) else {
                return self.cannot("apply the fix", format!("{rel} is not open here"));
            };
            let unchanged = match Rc::ptr_eq(&target, tab) {
                true => target.lang.doc.version() == asked,
                false => target.lang.doc.in_step(),
            };
            if !unchanged {
                return self.cannot("apply the fix", format!("{rel} changed meanwhile"));
            }
            if !target.view.is_editable() {
                return self.cannot("apply the fix", format!("{rel} is read-only"));
            }
            open.push((target, list));
        }
        for (target, list) in open {
            apply_edits(target.buffer.upcast_ref(), list);
        }
        if edits.files.is_empty() {
            return;
        }
        let names: Vec<String> = edits.files.iter().map(|f| f.rel.clone()).collect();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let files = edits.files;
            let done =
                work::attempt("change the other files", move || vault.edit_files(&files)).await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            match done {
                Ok(failed) if failed.is_empty() => app.toast(&match names.as_slice() {
                    [one] => format!("Also changed {one}"),
                    many => format!("Also changed {} other files", many.len()),
                }),
                Ok(failed) => {
                    for (rel, why) in failed {
                        app.cannot(&format!("change {rel}"), why);
                    }
                }
                Err(message) => app.toast(&message),
            }
        });
    }
}

/// The menu's sections: the quick fixes, the server's preferred ones first, then the refactorings
/// and what has no kind. Source actions (Organize Imports) are not listed.
fn sections(actions: Vec<CodeAction>) -> [Vec<CodeAction>; 2] {
    let is = |a: &CodeAction, kind: &str| {
        a.kind == kind
            || a.kind
                .strip_prefix(kind)
                .is_some_and(|k| k.starts_with('.'))
    };
    let (mut fixes, rest): (Vec<_>, Vec<_>) = actions
        .into_iter()
        .filter(|a| !is(a, "source"))
        .partition(|a| is(a, "quickfix"));
    fixes.sort_by_key(|a| !a.preferred);
    [fixes, rest]
}

/// Make a server's edits to `buffer` as one undo step.
fn apply_edits(buffer: &gtk::TextBuffer, edits: Vec<TextEdit>) {
    buffer.begin_user_action();
    for edit in order(edits) {
        let mut from = lang::iter_at(buffer, edit.range.start);
        let mut to = lang::iter_at(buffer, edit.range.end);
        buffer.delete(&mut from, &mut to);
        buffer.insert(&mut from, &edit.text);
    }
    buffer.end_user_action();
}

/// `edits` in the order to make them: back to front, so each leaves the places of the rest
/// alone, and inserts at one place the last listed first, so they read in the server's order.
fn order(edits: Vec<TextEdit>) -> Vec<TextEdit> {
    let mut edits: Vec<_> = edits.into_iter().enumerate().collect();
    edits.sort_by_key(|(i, e)| std::cmp::Reverse((e.range.start, *i)));
    edits.into_iter().map(|(_, e)| e).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use accent_api::Pos;

    fn action(title: &str, kind: &str, preferred: bool) -> CodeAction {
        CodeAction {
            title: title.into(),
            kind: kind.into(),
            preferred,
            raw: serde_json::Value::Null,
        }
    }

    #[test]
    fn fixes_come_first_the_preferred_on_top_and_source_actions_stay_out() {
        let [fixes, rest] = sections(vec![
            action("Extract", "refactor.extract", false),
            action("Other fix", "quickfix", false),
            action("Organize Imports", "source.organizeImports", false),
            action("Swap", "", false),
            action("insert ';'", "quickfix", true),
            action("Not a fix", "quickfixes", false),
        ]);
        let titles = |list: &[CodeAction]| list.iter().map(|a| a.title.clone()).collect::<Vec<_>>();
        assert_eq!(titles(&fixes), ["insert ';'", "Other fix"]);
        assert_eq!(titles(&rest), ["Extract", "Swap", "Not a fix"]);
    }

    #[test]
    fn edits_go_back_to_front_and_inserts_at_one_place_keep_their_order() {
        let at = |character| Pos { line: 0, character };
        let edit = |from, to, text: &str| TextEdit {
            range: Range {
                start: at(from),
                end: at(to),
            },
            text: text.into(),
        };
        let made: Vec<String> = order(vec![edit(9, 9, ";"), edit(0, 0, "a"), edit(0, 0, "b")])
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert_eq!(made, [";", "b", "a"]);
    }
}
