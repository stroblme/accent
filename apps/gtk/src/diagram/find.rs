//! Find over a diagram's labels, for the pane's find bar, which addresses a diagram as it does a
//! PDF (`find::PreviewOp`): each label is matched by `accent_core::search` on the one line search
//! reads it as (`accent_drawio::text::labels`), and a hit is the cell it labels, counted through
//! the pages in document order, selected and brought to the middle of the view.

use std::rc::Rc;

use accent_core::search::{self, Options};
use accent_drawio::CellId;

use super::DiagramTab;

/// The find bar's query over the labels, and the cells it hits.
#[derive(Default)]
pub(super) struct Find {
    query: String,
    invalid: bool,
    /// Each page's cells whose label matches, by page and then document order.
    hits: Vec<(usize, CellId)>,
    current: Option<usize>,
}

impl DiagramTab {
    /// Search every page's labels for `text` under `options` and show the first hit on or after
    /// the page shown, round past the last.
    pub fn find(self: &Rc<Self>, text: &str, options: Options) {
        let compiled = (!text.is_empty()).then(|| search::pattern(text, options));
        let hits = match &compiled {
            Some(Ok(re)) => accent_drawio::text::labels(&self.file())
                .into_iter()
                .filter(|label| re.is_match(&label.text))
                .map(|label| (label.page, label.cell))
                .collect(),
            _ => Vec::new(),
        };
        *self.finding.borrow_mut() = Find {
            query: text.to_string(),
            invalid: matches!(compiled, Some(Err(_))),
            hits,
            current: None,
        };
        self.step_match(true);
    }

    /// Show the next hit or the one before, round past either end; with none shown yet, the first
    /// from the page shown.
    pub fn step_match(self: &Rc<Self>, forward: bool) {
        let next = {
            let find = self.finding.borrow();
            let total = find.hits.len();
            match (total, find.current) {
                (0, _) => None,
                (_, None) => {
                    let page = self.page_index();
                    let after = find.hits.partition_point(|(p, _)| *p < page);
                    Some(match forward {
                        true => after % total,
                        false => (after + total - 1) % total,
                    })
                }
                (_, Some(at)) if forward => Some((at + 1) % total),
                (_, Some(at)) => Some((at + total - 1) % total),
            }
        };
        if let Some(at) = next {
            self.show_hit(at);
        }
        self.on_matches.emit(self);
    }

    /// Hit `at`: its page shown, and its cell selected in the middle of the view; a presented
    /// page selects nothing, so there the page alone.
    fn show_hit(self: &Rc<Self>, at: usize) {
        let (page, cell) = {
            let mut find = self.finding.borrow_mut();
            find.current = Some(at);
            find.hits[at].clone()
        };
        if page != self.page_index() {
            self.show_page(page);
        }
        if self.presenting.get().is_some() {
            return;
        }
        self.select(vec![cell.clone()]);
        if let Some(frame) = self.view.sheet().and_then(|sheet| sheet.frame_of(&cell)) {
            self.view.centre(&frame);
        }
    }

    /// Forget the query, the bar being put away.
    pub fn find_clear(&self) {
        *self.finding.borrow_mut() = Find::default();
    }

    /// "3 of 12", or what to say when there is nothing to show.
    pub fn matches_label(&self) -> String {
        let find = self.finding.borrow();
        match (find.hits.len(), find.current) {
            _ if find.invalid => "Invalid pattern".to_string(),
            _ if find.query.is_empty() => String::new(),
            (0, _) => "No results".to_string(),
            (1, None) => "1 match".to_string(),
            (total, None) => format!("{total} matches"),
            (total, Some(at)) => format!("{} of {total}", at + 1),
        }
    }

    pub fn connect_matches(&self, f: impl Fn(&Rc<DiagramTab>) + 'static) {
        self.on_matches.set(Rc::new(f));
    }
}
