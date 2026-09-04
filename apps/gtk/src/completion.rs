//! Completion providers for `[[wikilinks]]` and `#tags`.
//!
//! Both providers are driven by a closure, not by a vault handle, so this file never touches the
//! index and the prefix scanning is testable without a display.
//!
//! What the GtkSourceView 5.20 machinery guarantees, read out of `gtksourcecompletion.c` rather
//! than assumed:
//!
//! * `is_trigger` is called from the buffer's `insert-text` handler connected `G_CONNECT_AFTER`,
//!   with the *insert cursor*, so the iter sits immediately **after** the character `c` that was
//!   just typed.
//! * `populate` is synchronous here. The interface's primary entry point is `populate_async`, but
//!   its default implementation calls the sync `populate` vfunc, and the Rust binding's
//!   `populate_future` default routes back through it, so implementing `populate` alone is enough.

use gtk::glib;
use gtk::subclass::prelude::*;
use sourceview5::prelude::*;

/// How many rows the popup shows before it scrolls.
const PAGE_SIZE: u32 = 8;

/// What a provider offers, and how the accepted text is written back.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    #[default]
    WikiLink,
    Tag,
}

/// Byte offset of the trigger that opened this completion, and the text typed since.
///
/// `line` is the current line from its start up to the cursor, and `cursor` is a byte offset into
/// it, so nothing here ever looks at the rest of the buffer.
///
/// `None` means there is nothing to complete: no trigger on this line, a `#` run that opens the
/// line (an ATX heading), a wikilink that is already closed, or a tag the user has typed past.
fn scan(kind: Kind, line: &str, cursor: usize) -> Option<(usize, &str)> {
    if !line.is_char_boundary(cursor) {
        return None;
    }
    let head = &line[..cursor];
    match kind {
        Kind::WikiLink => {
            let start = head.rfind("[[")?;
            let prefix = &head[start + 2..];
            // `]` means the link was already closed; the cursor is past it, not inside it.
            (!prefix.contains(']')).then_some((start, prefix))
        }
        Kind::Tag => {
            let start = head.rfind('#')?;
            // A `#` run that opens the line, indented or not, is an ATX heading marker.
            if head[..start].trim_end_matches('#').trim().is_empty() {
                return None;
            }
            let prefix = &head[start + 1..];
            // Whitespace ends a tag, so the cursor is no longer inside one.
            (!prefix.contains(char::is_whitespace)).then_some((start, prefix))
        }
    }
}

// ------------------------------------------------------------------------------------ proposal

mod proposal_imp {
    use std::cell::RefCell;

    use gtk::glib;
    use gtk::subclass::prelude::*;
    use sourceview5::subclass::prelude::*;

    #[derive(Default)]
    pub struct Proposal {
        pub text: RefCell<String>,
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
    fn new(text: &str) -> Self {
        let obj: Self = glib::Object::new();
        obj.imp().text.replace(text.to_owned());
        obj
    }

    fn text(&self) -> String {
        self.imp().text.borrow().clone()
    }
}

// ------------------------------------------------------------------------------------ provider

mod provider_imp {
    use std::cell::{Cell, RefCell};

    use gtk::prelude::*;
    use gtk::subclass::prelude::*;
    use gtk::{gio, glib};
    use sourceview5::subclass::prelude::*;
    use sourceview5::{
        CompletionCell, CompletionColumn, CompletionContext, CompletionProposal, CompletionProvider,
    };

    use super::{Kind, Proposal, scan};

    type Candidates = Box<dyn Fn(&str) -> Vec<String>>;

    #[derive(Default)]
    pub struct Provider {
        pub kind: Cell<Kind>,
        pub candidates: RefCell<Option<Candidates>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Provider {
        const NAME: &'static str = "AccentCompletionProvider";
        type Type = super::Provider;
        type Interfaces = (CompletionProvider,);
    }

    impl ObjectImpl for Provider {}

    impl Provider {
        /// The current line up to the cursor, plus the cursor iter itself.
        fn line_to_cursor(context: &CompletionContext) -> Option<(gtk::TextIter, String)> {
            let buffer = context.buffer()?;
            let end = buffer.iter_at_mark(&buffer.get_insert());
            let mut start = end;
            start.set_line_offset(0);
            Some((end, buffer.text(&start, &end, true).to_string()))
        }

        /// Ranked, capped completions for whatever the user has typed since the trigger.
        fn proposals(&self, context: &CompletionContext) -> gio::ListStore {
            let store = gio::ListStore::new::<Proposal>();
            let Some((_, line)) = Self::line_to_cursor(context) else {
                return store;
            };
            let Some((_, prefix)) = scan(self.kind.get(), &line, line.len()) else {
                // ponytail: GtkSourceView hides the popup once every provider's model is empty,
                // but one opened on a trigger the cursor has since left occasionally stays up
                // until the window is reloaded, so ask for the hide as well. Remove it once the
                // built-in `notify::empty` path proves sufficient on its own. It has to wait for
                // an idle: `populate` and `refilter` run while the caller still holds `context`,
                // and hiding it from under them would drop that context mid-iteration.
                if let Some(completion) = context.completion() {
                    glib::idle_add_local_once(move || completion.hide());
                }
                return store;
            };
            if let Some(candidates) = self.candidates.borrow().as_ref() {
                for text in candidates(prefix) {
                    store.append(&Proposal::new(&text));
                }
            }
            store
        }
    }

    impl CompletionProviderImpl for Provider {
        fn is_trigger(&self, iter: &gtk::TextIter, _c: char) -> bool {
            // One source of truth with `populate`: a trigger is whatever `scan` recognises with
            // nothing typed after it yet. `iter` is the insert cursor, so the line up to it is
            // exactly the text `scan` reads, and the character just typed is already part of it.
            // Deciding this from the cursor column instead is how `## Heading` used to open a
            // popup listing every tag in the vault.
            let mut start = *iter;
            start.set_line_offset(0);
            let line = iter.buffer().text(&start, iter, true);
            matches!(scan(self.kind.get(), &line, line.len()), Some((_, "")))
        }

        fn populate(&self, context: &CompletionContext) -> Result<gio::ListModel, glib::Error> {
            Ok(self.proposals(context).upcast())
        }

        fn refilter(&self, context: &CompletionContext, _model: &gio::ListModel) {
            // The prefix grew or shrank; recompute and hand the new model back through the context.
            let store = self.proposals(context);
            context.set_proposals_for_provider(
                self.obj().upcast_ref::<CompletionProvider>(),
                Some(&store),
            );
        }

        fn display(
            &self,
            _context: &CompletionContext,
            proposal: &CompletionProposal,
            cell: &CompletionCell,
        ) {
            if cell.column() != CompletionColumn::TypedText {
                return;
            }
            if let Some(proposal) = proposal.downcast_ref::<Proposal>() {
                cell.set_text(Some(&proposal.text()));
            }
        }

        fn activate(&self, context: &CompletionContext, proposal: &CompletionProposal) {
            let Some(proposal) = proposal.downcast_ref::<Proposal>() else {
                return;
            };
            let Some(buffer) = context.buffer() else {
                return;
            };
            let Some((mut end, line)) = Self::line_to_cursor(context) else {
                return;
            };
            let Some((start, _)) = scan(self.kind.get(), &line, line.len()) else {
                return;
            };

            // `line` runs from line offset 0 to the cursor, so `start` — a byte offset into it —
            // becomes an iter by counting the characters before it. The replaced range is
            // [trigger, cursor): it swallows the `[[` or `#` itself, because the inserted text
            // carries them again. Anything outside that range is untouched.
            let mut begin = end;
            begin.set_line_offset(line[..start].chars().count() as i32);
            let text = match self.kind.get() {
                Kind::WikiLink => format!("[[{}]]", proposal.text()),
                Kind::Tag => format!("#{}", proposal.text()),
            };

            buffer.begin_user_action();
            buffer.delete(&mut begin, &mut end);
            // `delete` leaves both iters at the deletion point, so this inserts exactly there.
            buffer.insert(&mut begin, &text);
            buffer.end_user_action();
        }
    }
}

glib::wrapper! {
    pub struct Provider(ObjectSubclass<provider_imp::Provider>)
        @implements sourceview5::CompletionProvider;
}

/// `candidates(prefix)` returns already-ranked, already-capped completions.
pub fn provider(
    kind: Kind,
    candidates: impl Fn(&str) -> Vec<String> + 'static,
) -> sourceview5::CompletionProvider {
    let obj: Provider = glib::Object::new();
    obj.imp().kind.set(kind);
    obj.imp().candidates.replace(Some(Box::new(candidates)));
    obj.upcast()
}

/// Attach both providers to a view.
pub fn install(
    view: &sourceview5::View,
    notes: impl Fn(&str) -> Vec<String> + 'static,
    tags: impl Fn(&str) -> Vec<String> + 'static,
) {
    let completion = view.completion();
    completion.set_page_size(PAGE_SIZE);
    // Neither provider has an icon to show, and the empty icon cell with its padding is most of
    // what makes the popup look cramped.
    completion.set_show_icons(false);
    completion.add_provider(&provider(Kind::WikiLink, notes));
    completion.add_provider(&provider(Kind::Tag, tags));
}

#[cfg(test)]
mod tests {
    use super::{Kind, scan};

    #[test]
    fn scan_finds_a_wikilink_prefix() {
        let line = "see [[Dee";
        assert_eq!(scan(Kind::WikiLink, line, line.len()), Some((4, "Dee")));
    }

    #[test]
    fn scan_keeps_a_wikilink_prefix_with_a_space() {
        let line = "[[Deep Work";
        assert_eq!(
            scan(Kind::WikiLink, line, line.len()),
            Some((0, "Deep Work"))
        );
    }

    #[test]
    fn scan_needs_two_brackets() {
        let line = "a [Dee";
        assert_eq!(scan(Kind::WikiLink, line, line.len()), None);
    }

    #[test]
    fn scan_stops_at_a_closed_wikilink() {
        let line = "[[Deep Work]] and";
        assert_eq!(scan(Kind::WikiLink, line, line.len()), None);
    }

    #[test]
    fn scan_finds_a_tag_prefix() {
        let line = "note about #area/";
        assert_eq!(scan(Kind::Tag, line, line.len()), Some((11, "area/")));
    }

    #[test]
    fn scan_ignores_a_heading() {
        let line = "# Heading";
        assert_eq!(scan(Kind::Tag, line, line.len()), None);
    }

    #[test]
    fn scan_ignores_an_indented_heading() {
        for line in ["##", "  #", "\t### "] {
            assert_eq!(scan(Kind::Tag, line, line.len()), None, "{line:?}");
        }
    }

    /// A `#` with nothing typed after it yet is the trigger case: this is what `is_trigger`
    /// asks `scan` about, and it has to say yes there and only there.
    #[test]
    fn scan_accepts_a_tag_that_has_only_been_opened() {
        assert_eq!(scan(Kind::Tag, "a #", 3), Some((2, "")));
        assert_eq!(scan(Kind::Tag, "a #x", 4), Some((2, "x")));
    }

    #[test]
    fn scan_ends_a_tag_at_whitespace() {
        let line = "a #area and more";
        assert_eq!(scan(Kind::Tag, line, line.len()), None);
    }

    #[test]
    fn scan_reads_only_up_to_the_cursor() {
        let line = "a #area/work";
        assert_eq!(scan(Kind::Tag, line, 7), Some((2, "area")));
    }
}
