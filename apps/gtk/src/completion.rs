//! The completion popup, for every flavour of text tab.
//!
//! One provider, whatever the language: a note's `[[wikilinks]]` and `#tags` arrive from the
//! index-backed notes provider and a C file's members from clangd, through the same call and in
//! the same shape, so nothing here knows which it is looking at.
//!
//! What the GtkSourceView 5.20 machinery guarantees, read out of `gtksourcecompletion.c` rather
//! than assumed:
//!
//! * `is_trigger` is called from the buffer's `insert-text` handler connected `G_CONNECT_AFTER`,
//!   with the *insert cursor*, so the iter sits immediately **after** the character `c` that was
//!   just typed.
//! * `populate_async` is the entry point the completion always uses, and the Rust binding routes
//!   it to `populate_future`, so the request can be awaited on the main loop.
//! * `refilter` is called as the user types past the trigger. It is answered from the answer we
//!   already have rather than by asking again: a server ranks once, for the position the popup
//!   opened at, and re-requesting per keystroke is a round trip for a narrowing list.

use crate::editor::Tab;
use accent_api::{Completion, Pos, Range, TextEdit};
use gtk::glib;
use gtk::subclass::prelude::*;
use sourceview5::prelude::*;
use std::rc::Rc;

/// How many rows the popup shows before it scrolls.
const PAGE_SIZE: u32 = 8;

/// `edits` in the order they can be applied without invalidating each other: last in the document
/// first, so an edit never moves the range of one still to come.
fn ordered(mut edits: Vec<TextEdit>) -> Vec<TextEdit> {
    edits.sort_by_key(|e| {
        std::cmp::Reverse((e.range.start.line, e.range.start.character, e.text.len()))
    });
    edits
}

// ------------------------------------------------------------------------------------ proposal

mod proposal_imp {
    use accent_api::Completion;
    use gtk::glib;
    use gtk::subclass::prelude::*;
    use sourceview5::subclass::prelude::*;
    use std::cell::{Cell, RefCell};

    #[derive(Default)]
    pub struct Proposal {
        pub item: RefCell<Option<Completion>>,
        /// The documentation the details panel shows, once `completionItem/resolve` has answered.
        pub doc: RefCell<Option<String>>,
        /// A resolve is in flight or has been done; it is asked for at most once per row.
        pub resolved: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Proposal {
        const NAME: &'static str = "AccentCompletionProposal";
        type Type = super::Proposal;
        type Interfaces = (sourceview5::CompletionProposal,);
    }

    impl ObjectImpl for Proposal {}

    // `CompletionProposalImpl` is a marker: the interface has no vfuncs, the provider does all the
    // work in `display`.
    impl CompletionProposalImpl for Proposal {}
}

glib::wrapper! {
    pub struct Proposal(ObjectSubclass<proposal_imp::Proposal>)
        @implements sourceview5::CompletionProposal;
}

impl Proposal {
    fn new(item: Completion) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().doc.replace(item.doc.clone());
        obj.imp().item.replace(Some(item));
        obj
    }

    fn item(&self) -> Option<Completion> {
        self.imp().item.borrow().clone()
    }
}

// ------------------------------------------------------------------------------------ provider

mod provider_imp {
    use std::cell::{Cell, RefCell};
    use std::future::Future;
    use std::pin::Pin;
    use std::rc::{Rc, Weak};

    use accent_api::Completion;
    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{gio, glib};
    use sourceview5::prelude::*;
    use sourceview5::subclass::prelude::*;
    use sourceview5::{
        CompletionCell, CompletionColumn, CompletionContext, CompletionProposal, CompletionProvider,
    };

    use super::{Proposal, ordered};
    use crate::editor::Tab;
    use crate::{diagnostics, hover, lang};

    #[derive(Default)]
    pub struct Provider {
        /// The tab whose document this completes. Weak: the view holds the provider and the tab
        /// holds the view, so a strong handle here would keep every closed tab alive.
        pub tab: RefCell<Weak<Tab>>,
        /// The whole of the last answer, which is what [`refilter`] narrows. The model handed to
        /// the popup is only ever a filtered view of this.
        items: RefCell<Vec<Completion>>,
        /// The server stopped at a cap, so `items` is not the whole answer and the next
        /// keystroke asks again rather than narrowing.
        incomplete: Cell<bool>,
        /// Which ask is the latest; an earlier answer arriving later is not kept.
        asked: Cell<u64>,
        /// The proposal whose details panel was asked for last. A resolve that lands after the
        /// selection has moved on writes nothing.
        showing: RefCell<Option<Proposal>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Provider {
        const NAME: &'static str = "AccentCompletionProvider";
        type Type = super::Provider;
        type Interfaces = (CompletionProvider,);
    }

    impl ObjectImpl for Provider {}

    impl Provider {
        fn tab(&self) -> Option<Rc<Tab>> {
            self.tab.borrow().upgrade()
        }

        /// What the user has typed since `item` opened the popup: the text from where the
        /// accepted insert would start to the caret, which is what a filter matches against.
        fn typed(context: &CompletionContext, item: &Completion) -> String {
            let Some(buffer) = context.buffer() else {
                return String::new();
            };
            let start = diagnostics::iter_at(&buffer, item.replace.start);
            let caret = buffer.iter_at_mark(&buffer.get_insert());
            match start <= caret {
                true => buffer.text(&start, &caret, true).to_string(),
                false => String::new(),
            }
        }

        /// The items still matching what has been typed, in the order the server ranked them.
        fn matching(&self, context: &CompletionContext) -> gio::ListStore {
            let store = gio::ListStore::new::<Proposal>();
            for item in self.items.borrow().iter() {
                let typed = Self::typed(context, item);
                let haystack = item.filter.as_deref().unwrap_or(&item.label);
                if !typed.is_empty()
                    && sourceview5::Completion::fuzzy_match(Some(haystack), &typed.to_lowercase())
                        .is_none()
                {
                    continue;
                }
                store.append(&Proposal::new(item.clone()));
            }
            store
        }

        /// Ask the provider at the caret and keep its answer as the list to narrow.
        ///
        /// A later ask outranks an earlier one still in flight: the answer to where the caret
        /// was is not the list for where it is.
        fn fetch(
            &self,
            context: CompletionContext,
        ) -> Pin<Box<dyn Future<Output = gio::ListStore>>> {
            let (tab, me) = (self.tab(), self.ref_counted());
            let asked = self.asked.get().wrapping_add(1);
            self.asked.set(asked);
            Box::pin(async move {
                let Some(tab) = tab else {
                    return gio::ListStore::new::<Proposal>();
                };
                let (Some(vault), Some(buffer)) = (tab.lang.vault(), context.buffer()) else {
                    return gio::ListStore::new::<Proposal>();
                };
                let caret = buffer.iter_at_mark(&buffer.get_insert());
                let (pos, trigger) = (lang::pos_of(&caret), trigger_before(&tab, &caret));
                lang::flush(tab.clone()).await;
                let rel = tab.rel();
                let answer = match vault.completion(&rel, pos, trigger).await {
                    Ok(answer) => answer,
                    Err(e) => {
                        // Never an `Err` out of here: GtkSourceView drops the whole popup on a
                        // failing provider, and a server that is still indexing fails a lot.
                        tracing::debug!("completion for {rel}: {e:#}");
                        Default::default()
                    }
                };
                tracing::debug!(
                    "completion for {rel} at {pos:?}: {} items{}",
                    answer.items.len(),
                    if answer.incomplete {
                        ", more where they came from"
                    } else {
                        ""
                    }
                );
                if me.asked.get() == asked {
                    *me.items.borrow_mut() = answer.items;
                    me.incomplete.set(answer.incomplete);
                }
                me.matching(&context)
            })
        }
    }

    impl CompletionProviderImpl for Provider {
        fn is_trigger(&self, _iter: &gtk::TextIter, c: char) -> bool {
            // Whatever the provider said opens a list. Whether there is anything to offer at this
            // exact spot is the provider's own answer: an empty model hides the popup again,
            // which is how a lone `[` in a note opens nothing while `[[` opens the note list.
            self.tab()
                .and_then(|tab| tab.lang.support())
                .is_some_and(|s| s.completion_triggers.contains(&c))
        }

        fn populate_future(
            &self,
            context: &CompletionContext,
        ) -> Pin<Box<dyn Future<Output = Result<gio::ListModel, glib::Error>>>> {
            let fetch = self.fetch(context.clone());
            Box::pin(async move { Ok(fetch.await.upcast()) })
        }

        fn refilter(&self, context: &CompletionContext, _model: &gio::ListModel) {
            // Narrowed here rather than at the server: the ranking was done for the position the
            // popup opened at, and typing one more character does not change it. Unless the
            // server stopped at a cap, in which case what it left out may be exactly what the
            // next character asks for, so the list is fetched again for the caret as it is now.
            let provider = self.obj().clone();
            if self.incomplete.get() {
                let (fetch, context) = (self.fetch(context.clone()), context.clone());
                glib::spawn_future_local(async move {
                    let store = fetch.await;
                    context.set_proposals_for_provider(
                        provider.upcast_ref::<CompletionProvider>(),
                        Some(&store),
                    );
                });
                return;
            }
            let store = self.matching(context);
            context.set_proposals_for_provider(
                provider.upcast_ref::<CompletionProvider>(),
                Some(&store),
            );
        }

        fn display(
            &self,
            context: &CompletionContext,
            proposal: &CompletionProposal,
            cell: &CompletionCell,
        ) {
            let Some(proposal) = proposal.downcast_ref::<Proposal>() else {
                return;
            };
            let Some(item) = proposal.item() else {
                return;
            };
            match cell.column() {
                CompletionColumn::Icon => cell.set_icon_name(lang::icon_name(item.kind)),
                CompletionColumn::TypedText => {
                    let typed = Self::typed(context, &item).to_lowercase();
                    match sourceview5::Completion::fuzzy_highlight(&item.label, &typed) {
                        Some(attrs) => cell.set_text_with_attributes(&item.label, &attrs),
                        None => cell.set_text(Some(&item.label)),
                    }
                }
                CompletionColumn::After => cell.set_text(item.detail.as_deref()),
                CompletionColumn::Details => {
                    self.showing.replace(Some(proposal.clone()));
                    let doc = proposal.imp().doc.borrow().clone();
                    if let Some(doc) = doc.filter(|d| !d.is_empty()) {
                        return cell.set_markup(&hover::markup_of(&doc));
                    }
                    cell.set_text(None);
                    // The server keeps the documentation back until an item is looked at, which
                    // is what `completionItem/resolve` is for. Asked once per row, and written
                    // only if that row is still the one the panel is showing.
                    if item.resolve.is_none() || proposal.imp().resolved.replace(true) {
                        return;
                    }
                    let Some(tab) = self.tab() else { return };
                    let Some(vault) = tab.lang.vault() else {
                        return;
                    };
                    let cell = cell.clone();
                    let (me, proposal) = (self.ref_counted(), proposal.clone());
                    glib::spawn_future_local(async move {
                        let Ok(full) = vault.resolve_completion(&tab.rel(), item).await else {
                            return;
                        };
                        proposal.imp().doc.replace(full.doc.clone());
                        let still = me.showing.borrow().as_ref().is_some_and(|p| p == &proposal);
                        if let (true, Some(doc)) = (still, full.doc.filter(|d| !d.is_empty())) {
                            cell.set_markup(&hover::markup_of(&doc));
                        }
                    });
                }
                _ => {}
            }
        }

        fn activate(&self, context: &CompletionContext, proposal: &CompletionProposal) {
            let (Some(proposal), Some(buffer), Some(view)) = (
                proposal.downcast_ref::<Proposal>(),
                context.buffer(),
                context.view(),
            ) else {
                return;
            };
            let Some(item) = proposal.item() else {
                return;
            };

            // One user action, so Ctrl+Z takes the whole acceptance back: the replaced range, the
            // inserted text and whatever import the item brought with it.
            buffer.begin_user_action();
            let caret = lang::pos_of(&buffer.iter_at_mark(&buffer.get_insert()));
            let replace = super::grown(item.replace, caret);
            let mut start = diagnostics::iter_at(&buffer, replace.start);
            let mut end = diagnostics::iter_at(&buffer, replace.end);
            buffer.delete(&mut start, &mut end);
            // `delete` leaves both iters at the deletion point, so this writes exactly there.
            match item.is_snippet {
                // A snippet parked in the view: its tab stops are what makes `add($1, $2)` worth
                // accepting. A snippet the parser refuses goes in as the text it is, which is
                // wrong in a small way rather than losing the acceptance altogether.
                true => match sourceview5::Snippet::new_parsed(&item.insert) {
                    Ok(snippet) => view.push_snippet(&snippet, Some(&mut start)),
                    Err(e) => {
                        tracing::debug!("cannot parse the snippet {:?}: {e}", item.insert);
                        buffer.insert(&mut start, &item.insert);
                    }
                },
                false => buffer.insert(&mut start, &item.insert),
            }
            // Last in the document first, so applying one does not move the next. They sit
            // before the caret in practice — an import at the top of the file — which is why
            // they can be applied after the insert at all.
            for edit in ordered(item.extra_edits) {
                let mut from = diagnostics::iter_at(&buffer, edit.range.start);
                let mut to = diagnostics::iter_at(&buffer, edit.range.end);
                buffer.delete(&mut from, &mut to);
                buffer.insert(&mut from, &edit.text);
            }
            buffer.end_user_action();
        }
    }

    /// The character just typed, when it is one the provider asked to be told about. What the
    /// server needs to tell a member access from a plain word.
    fn trigger_before(tab: &Rc<Tab>, caret: &gtk::TextIter) -> Option<char> {
        let mut before = *caret;
        if !before.backward_char() {
            return None;
        }
        let c = before.char();
        tab.lang
            .support()
            .filter(|s| s.completion_triggers.contains(&c))
            .map(|_| c)
    }
}

glib::wrapper! {
    pub struct Provider(ObjectSubclass<provider_imp::Provider>)
        @implements sourceview5::CompletionProvider;
}

/// Attach the provider to a tab's view. Called by [`lang::attach`], which is what decides that
/// this tab has a vault to ask at all.
pub fn install(tab: &Rc<Tab>) {
    let completion = tab.view.completion();
    completion.set_page_size(PAGE_SIZE);
    // Every item now has a kind, and the glyph is what makes a list of thirty members scannable.
    completion.set_show_icons(true);
    let provider: Provider = glib::Object::new();
    *provider.imp().tab.borrow_mut() = Rc::downgrade(tab);
    completion.add_provider(&provider);
}

/// The range an accepted item replaces, once the caret has moved on: the popup opens on a
/// trigger with an empty word (`self.` offers everything), and what is typed after that to
/// narrow the list belongs to the word being completed. Without this `get` + `get_func` gave
/// `getget_func`. The caret only extends the range; a range reaching past it (the `]]` a note's
/// completion eats) is kept as it is.
fn grown(replace: Range, caret: Pos) -> Range {
    match caret.line == replace.end.line && caret.character > replace.end.character {
        true => Range {
            start: replace.start,
            end: caret,
        },
        false => replace,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_after_the_popup_opened_grows_what_is_replaced() {
        let at = |character| Pos { line: 3, character };
        let opened = Range {
            start: at(5),
            end: at(5),
        };
        assert_eq!(
            grown(opened, at(8)),
            Range {
                start: at(5),
                end: at(8)
            }
        );
        // A range reaching past the caret is the note's paired `]]`, and stays.
        let eats = Range {
            start: at(2),
            end: at(9),
        };
        assert_eq!(grown(eats, at(7)), eats);
        // Another line is another story; nothing is guessed.
        assert_eq!(
            grown(
                opened,
                Pos {
                    line: 4,
                    character: 1
                }
            ),
            opened
        );
    }

    fn edit(line: u32, text: &str) -> TextEdit {
        let at = Pos { line, character: 0 };
        TextEdit {
            range: Range { start: at, end: at },
            text: text.to_string(),
        }
    }

    /// Applying an edit moves everything after it, so the extra edits an item brings are applied
    /// from the end of the document backwards and none of them is ever asked about a stale
    /// position.
    #[test]
    fn extra_edits_are_applied_last_first() {
        let order: Vec<String> = ordered(vec![edit(0, "a"), edit(12, "c"), edit(4, "b")])
            .into_iter()
            .map(|e| e.text)
            .collect();
        assert_eq!(order, ["c", "b", "a"]);
    }
}
