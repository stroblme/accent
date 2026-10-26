//! Quick Fix: what a language server offers over the selection or at the caret, in a menu at
//! the caret, and the edits the one picked makes (DESIGN.md, Language servers).

use super::*;
use accent_api::{ActionEdits, CodeAction, Range, TextEdit};
use lang::Editor;

impl App {
    /// Ask the server for its fixes and refactorings over the selection, or at the caret, of the
    /// text tab in front or the notebook's selected code cell, and list them in a menu there.
    pub fn quick_fix(self: &Rc<Self>) {
        let (editor, cell, what): (Rc<dyn Editor>, bool, String) = match self.active_notebook() {
            Some(notebook) => match notebook.language_editor() {
                Some(cell) => (cell, true, "this notebook".to_string()),
                None => return self.toast("No quick fixes here"),
            },
            None => match self.active() {
                Some(tab) => {
                    let what = tab.language().unwrap_or_else(|| "this file".to_string());
                    (tab, false, what)
                }
                None => return,
            },
        };
        let doc = editor.document();
        let Some(vault) = doc.vault() else {
            return self.needs_vault("list quick fixes");
        };
        if let Some(server) = doc.support().and_then(|s| s.missing.clone()) {
            return self.toast(&format!(
                "No language server for {what} ({server} not found)"
            ));
        }
        if !editor.offers_fixes() {
            return self.cannot("apply a quick fix", "the file is read-only");
        }
        // At the primary caret alone, as `Ctrl+Space` asks, and with nothing else over the text.
        let view = editor.view().clone();
        if let Some(column) = view.downcast_ref::<multicaret::View>() {
            column.clear_carets();
        }
        if let Some(session) = editor.completion() {
            session.close();
        }
        editor.signature().dismiss();
        let buffer = view.buffer();
        let caret = editor::caret(&buffer);
        let (from, to) = buffer.selection_bounds().unwrap_or((caret, caret));
        let (from, to) = (lang::pos_of(&from), lang::pos_of(&to));
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            editor.clone().flush().await;
            let asked = doc.version();
            let (Some(start), Some(end)) = (editor.to_document(from), editor.to_document(to))
            else {
                return;
            };
            let found = vault.code_actions(&doc.key(), Range { start, end }).await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            // Typed past: the answer is about text that is not there any more.
            if doc.version() != asked {
                return;
            }
            match found {
                Ok(actions) => app.offer_fixes(&editor, cell, actions),
                Err(e) => app.cannot("list quick fixes", e),
            }
        });
    }

    /// The menu of `actions` at `editor`'s caret, in [`sections`]; picking one applies it.
    fn offer_fixes(self: &Rc<Self>, editor: &Rc<dyn Editor>, cell: bool, actions: Vec<CodeAction>) {
        let sections = sections(actions);
        if sections.iter().all(Vec::is_empty) {
            return self.toast("No quick fixes here");
        }
        let menu = gio::Menu::new();
        let listed: Vec<CodeAction> = sections.concat();
        let mut index = 0u32;
        for section in sections.iter().filter(|s| !s.is_empty()) {
            let items = gio::Menu::new();
            for action in section {
                // A menu reads `_` as a mnemonic: `snake_case` in a title must stay itself.
                let item = gio::MenuItem::new(Some(&action.title.replace('_', "__")), None);
                item.set_action_and_target_value(Some("fix.apply"), Some(&index.to_variant()));
                items.append_item(&item);
                index += 1;
            }
            menu.append_section(None, &items);
        }
        let apply = gio::SimpleAction::new("apply", Some(glib::VariantTy::UINT32));
        let (app, picked) = (Rc::downgrade(self), Rc::downgrade(editor));
        apply.connect_activate(move |_, target| {
            let index = target.and_then(u32::from_variant).map(|i| i as usize);
            if let (Some(app), Some(editor), Some(index)) = (app.upgrade(), picked.upgrade(), index)
                && let Some(action) = listed.get(index)
            {
                app.apply_fix(editor, cell, action.clone());
            }
        });
        let group = gio::SimpleActionGroup::new();
        group.add_action(&apply);
        // On the view before the menu is up: an item whose action is not found yet starts greyed.
        let view = editor.view();
        view.insert_action_group("fix", Some(&group));
        let place = view.iter_location(&editor::caret(&view.buffer()));
        let (x, y) =
            view.buffer_to_window_coords(gtk::TextWindowType::Widget, place.x(), place.y());
        let anchor = gdk::Rectangle::new(x, y, 1, place.height());
        widgets::popup_menu(view, &menu, Some(anchor));
    }

    /// Apply `action`, picked in `editor`: resolved or run at its server first, then its edits
    /// made; in a notebook `cell`, only edits inside it.
    fn apply_fix(self: &Rc<Self>, editor: Rc<dyn Editor>, cell: bool, action: CodeAction) {
        let doc = editor.document();
        let Some(vault) = doc.vault() else {
            return;
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            editor.clone().flush().await;
            let asked = doc.version();
            let edits = vault.code_action_edits(&doc.key(), action).await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            match edits {
                Ok(edits) => app.make_edits(&editor, cell, asked, vault, edits),
                Err(e) => app.cannot("apply the fix", e),
            }
        });
    }

    /// Make `edits`: each open file's in its view as one undo step, all of them or none, then the
    /// files it creates and those no tab holds on disk, gated on their etags. `asked` is the
    /// version of `editor`'s document the server answered about; a notebook `cell` takes edits
    /// inside it alone.
    fn make_edits(
        self: &Rc<Self>,
        editor: &Rc<dyn Editor>,
        cell: bool,
        asked: u64,
        vault: Arc<Vault>,
        edits: ActionEdits,
    ) {
        let beyond = || self.cannot("apply the fix", "it changes more than this cell");
        if cell && !(edits.files.is_empty() && edits.created.is_empty()) {
            return beyond();
        }
        let doc = editor.document();
        let mut open: Vec<(gtk::TextBuffer, Vec<TextEdit>)> = Vec::new();
        for (rel, list) in edits.open {
            if rel == doc.key() {
                if doc.version() != asked {
                    return self.cannot("apply the fix", format!("{rel} changed meanwhile"));
                }
                let into_view = |edit: TextEdit| {
                    let start = editor.to_view(edit.range.start)?;
                    let end = editor.to_view(edit.range.end)?;
                    Some(TextEdit {
                        range: Range { start, end },
                        ..edit
                    })
                };
                let Some(list) = list.into_iter().map(into_view).collect() else {
                    return beyond();
                };
                open.push((editor.view().buffer(), list));
                continue;
            }
            if cell {
                return beyond();
            }
            let Some(target) = self.tab_for(&rel) else {
                return self.cannot("apply the fix", format!("{rel} is not open here"));
            };
            if !target.lang.doc.in_step() {
                return self.cannot("apply the fix", format!("{rel} changed meanwhile"));
            }
            if !target.view.is_editable() {
                return self.cannot("apply the fix", format!("{rel} is read-only"));
            }
            open.push((target.buffer.clone().upcast(), list));
        }
        for (buffer, list) in open {
            apply_edits(&buffer, list);
        }
        let (files, created) = (edits.files, edits.created);
        let said = match (created.as_slice(), files.as_slice()) {
            ([], []) => return,
            ([(one, _)], []) => format!("Created {one}"),
            ([], [one]) => format!("Also changed {}", one.rel),
            (made, changed) => format!("Also changed {} other files", made.len() + changed.len()),
        };
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let done = work::attempt("change the other files", move || {
                // A file is made new, so one that appeared meanwhile is left as it is.
                let mut failed: Vec<(String, String)> = created
                    .iter()
                    .filter_map(|(rel, text)| {
                        let made = fileops::make_parents(&vault, rel).and_then(|()| {
                            vault
                                .write_file(rel, text.as_bytes())
                                .map_err(|e| e.to_string())
                        });
                        Some((format!("create {rel}"), made.err()?))
                    })
                    .collect();
                let changed = vault.edit_files(&files)?;
                failed.extend(
                    changed
                        .into_iter()
                        .map(|(rel, why)| (format!("change {rel}"), why)),
                );
                Ok::<_, accent_api::Error>(failed)
            })
            .await;
            let Some(app) = weak.upgrade() else {
                return;
            };
            match done {
                Ok(failed) if failed.is_empty() => app.toast(&said),
                Ok(failed) => {
                    for (what, why) in failed {
                        app.cannot(&what, why);
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
