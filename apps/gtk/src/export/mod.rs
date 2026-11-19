//! Print… and Export as… for whichever tab the menu acts on, and what their kinds share.
//!
//! A note or a notebook goes out as its HTML page ([`html`]): Export as HTML… writes it, and
//! Print… opens it in the system browser, whose print dialog prints it. PDF and diagram tabs use
//! their own export modules.

use super::*;
use crate::work::Slices;
use accent_api::link::Stop;

pub(crate) mod css;
mod html;

pub(crate) use html::Paper;

/// What an export writes.
#[derive(Clone, Copy)]
pub enum Export {
    Pdf,
    Html,
    Png,
    Svg,
}

/// A printout's margin on every side, LibreOffice's default: GTK's own quarter inch crowds prose.
const MARGIN_MM: f64 = 20.0;

/// What `doc` can be printed and exported as, `None` where it cannot be: a note, markdown or
/// org, a notebook, a PDF and a diagram are the four (DESIGN.md, Preview, Notebook, PDF and
/// Diagram).
pub fn printable(doc: &Doc) -> Option<Kind> {
    match doc {
        Doc::Text(tab) if tab.flavour().is_note() => Some(Kind::Note),
        Doc::Pdf(_) => Some(Kind::Pdf),
        Doc::Diagram(_) => Some(Kind::Diagram),
        Doc::Notebook(_) => Some(Kind::Notebook),
        _ => None,
    }
}

/// What the file at `key` can be printed and exported as by its name alone, for a file's menu
/// away from its tab, where [`printable`] answers. A marimo `.py` and a diagram saved as `.xml`
/// are known by their bytes, so only their tabs offer it.
pub fn printable_name(key: &str) -> Option<Kind> {
    match doc::kind_of(key) {
        Kind::Text if accent_core::path::is_org(key) => Some(Kind::Note),
        Kind::Text | Kind::Image => None,
        kind => Some(kind),
    }
}

impl App {
    /// Print…: the tab right-clicked, else the one in front.
    pub(crate) fn print(self: &Rc<Self>) {
        if let Some(doc) = self.menu_doc() {
            self.print_doc(&doc);
        }
    }

    /// Print `doc`: a note or a notebook opens in the system browser, which prints it, a PDF and
    /// a diagram go through their tabs' print dialogs; any other tab does nothing, as Save As
    /// does.
    fn print_doc(self: &Rc<Self>, doc: &Doc) {
        match doc {
            Doc::Pdf(pdf) => pdf::export::print(self, pdf),
            Doc::Diagram(tab) => diagram::export::print(self, tab),
            doc => {
                if let Some(paper) = Paper::of(doc) {
                    self.open_in_browser(paper, Some("Opened in your browser. Print from there"))
                }
            }
        }
    }

    /// Print… (`None`) or an export of `key` from a file's menu in the tree or the Git pane, on
    /// its tab: opened, kept, where it is not open yet, and run once it lands.
    pub(crate) fn make(self: &Rc<Self>, key: &str, what: Option<Export>) {
        let run = move |app: &Rc<App>, doc: &Doc| match what {
            None => app.print_doc(doc),
            Some(to) => app.export_doc(doc.clone(), to),
        };
        if let Some(doc) = self.doc_for(key) {
            return run(self, &doc);
        }
        self.pending.reveal(key, open::Reveal::Then(Box::new(run)));
        self.open_path(key);
    }

    /// Open in Browser: the note or notebook the menu acts on, as Print… opens it; any other tab
    /// does nothing.
    pub(crate) fn browse(self: &Rc<Self>) {
        if let Some(paper) = self.menu_doc().as_ref().and_then(Paper::of) {
            self.open_in_browser(paper, None);
        }
    }

    /// Export as…: the tab right-clicked, else the one in front.
    pub(crate) fn export(self: &Rc<Self>, to: Export) {
        if let Some(doc) = self.menu_doc() {
            self.export_doc(doc, to);
        }
    }

    /// Choose a local destination and export `doc` in the supported format.
    /// Notes and notebooks support HTML, PDFs PDF, and diagrams PDF/PNG/SVG; other kinds cannot
    /// export.
    fn export_doc(self: &Rc<Self>, doc: Doc, to: Export) {
        if printable(&doc).is_none() {
            return;
        }
        let key = doc.key();
        let stem = Path::new(doc::file_name(&key))
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
        let title = match to {
            Export::Pdf => "Export as PDF",
            Export::Html => "Export as HTML",
            Export::Png => "Export as PNG",
            Export::Svg => "Export as SVG",
        };
        let name = match (to, &doc) {
            (Export::Html, Doc::Text(_) | Doc::Notebook(_)) => format!("{stem}.html"),
            (Export::Pdf, Doc::Pdf(_)) => format!("{stem} (exported).pdf"),
            (_, Doc::Diagram(tab)) => match diagram::export::offered_name(tab, to) {
                Some(name) => name,
                None => return,
            },
            _ => return,
        };
        let dialog = gtk::FileDialog::builder()
            .title(title)
            .initial_name(&name)
            .modal(true)
            .build();
        // A host's folders are not on this machine, so there the chooser starts where it likes.
        if !self.on_host(&key)
            && let Some(dir) = self.root().join(&key).parent()
        {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let app = Rc::downgrade(self);
        dialog.save(Some(&self.window), gio::Cancellable::NONE, move |result| {
            // The error is almost always "the user closed the chooser", which needs no toast.
            let (Some(app), Some(dest)) = (app.upgrade(), result.ok().and_then(|f| f.path()))
            else {
                return;
            };
            match (doc, to) {
                (Doc::Pdf(pdf), _) => pdf::export::export_to(&app, &pdf, dest),
                (Doc::Diagram(tab), to) => diagram::export::export_to(&app, &tab, to, dest),
                (doc, Export::Html) => {
                    if let Some(paper) = Paper::of(&doc) {
                        app.html_to(paper, dest)
                    }
                }
                _ => {}
            }
        });
    }

    /// The document the tab menu's items act on: the tab right-clicked, else the one in front; a
    /// preview tab's note for a preview tab.
    fn menu_doc(&self) -> Option<Doc> {
        let doc = match self.menu_page.borrow().clone() {
            Some(page) => self.doc_for_page(&page),
            None => self.active_doc(),
        };
        doc.and_then(rendered::note_for)
    }

    /// Run the work `start` hands back as a [`Job`] that says `busy`, then say how it went: the
    /// toast it asks for, or that the window cannot `what`, with its reason, and nothing once the
    /// reader stopped it. A PDF's print and export say it so too.
    pub(crate) fn busy_with<F>(
        self: &Rc<Self>,
        busy: String,
        what: String,
        start: impl FnOnce(Rc<Job>) -> F,
    ) where
        F: Future<Output = Result<Option<String>, String>> + 'static,
    {
        let job = Job::start(self, busy);
        let work = start(job.clone());
        let app = self.clone();
        glib::spawn_future_local(async move {
            let said = work.await;
            job.end();
            match said {
                Ok(Some(done)) => app.toast(&done),
                Ok(None) => {}
                // The reader stopped it, which is answer enough.
                Err(why) if why == work::STOPPED => {}
                Err(why) => app.cannot(&what, why),
            }
        });
    }
}

/// A print or an export being made, from its command to its toast (DESIGN.md, Status bar): its
/// line on the status bar, which counts the pages done where the work says, with a Stop where the
/// work runs in slices; and the bar across the top of the document column, once the work has taken
/// [`loading::OPENING`], so a quick export never flashes it.
pub(crate) struct Job {
    app: Rc<App>,
    busy: String,
    /// What its line says now.
    line: RefCell<String>,
    stop: Stop,
    stoppable: Cell<bool>,
    /// How far it has got, where the work says.
    fraction: Cell<Option<f64>>,
    bar: Cell<Held>,
}

/// Where a job is with the bar.
#[derive(Clone, Copy, PartialEq)]
enum Held {
    /// Not up yet.
    Soon,
    Up,
    /// Done with it: the job is over, or waits on the reader.
    Done,
}

impl Job {
    fn start(app: &Rc<App>, busy: String) -> Rc<Job> {
        app.statusbar.set_transfer(&busy, true);
        let job = Rc::new(Job {
            app: app.clone(),
            line: RefCell::new(busy.clone()),
            busy,
            stop: Stop::default(),
            stoppable: Cell::new(false),
            fraction: Cell::new(None),
            bar: Cell::new(Held::Soon),
        });
        let weak = Rc::downgrade(&job);
        glib::timeout_add_local_once(loading::OPENING, move || {
            if let Some(job) = weak.upgrade()
                && job.bar.get() == Held::Soon
            {
                job.bar.set(Held::Up);
                job.app.connect.hold(job.fraction.get());
            }
        });
        job
    }

    /// The work runs on the main loop in slices, which the Stop beside its line ends between two
    /// of them.
    pub(crate) fn slices(&self) -> Slices {
        if !self.stoppable.replace(true) {
            let line = self.line.borrow();
            self.app.statusbar.set_transfer(&line, false);
            self.app.statusbar.set_stoppable(&line, true, &self.stop);
        }
        Slices::stopped_by(self.stop.clone())
    }

    /// `done` of `total` pages are made: "Exporting flow.pdf… 3/12 pages", the bar as far along.
    /// A single page has nothing to count, so its line stays as it is and its bar pulses.
    pub(crate) fn pages(&self, done: usize, total: usize) {
        if total < 2 {
            return;
        }
        let line = format!("{} {done}/{total} pages", self.busy);
        let old = self.line.replace(line.clone());
        self.app.statusbar.retell_transfer(&old, &line);
        let fraction = done as f64 / total.max(1) as f64;
        self.fraction.set(Some(fraction));
        if self.bar.get() == Held::Up {
            self.app.connect.advance(Some(fraction));
        }
    }

    /// The work waits on the reader now, as a print dialog does: the bar lets go, and the line
    /// says what it said at first, with no Stop.
    pub(crate) fn settle(&self) {
        self.let_go();
        let old = self.line.replace(self.busy.clone());
        self.app.statusbar.set_transfer(&old, false);
        self.app.statusbar.set_transfer(&self.busy, true);
    }

    fn end(&self) {
        self.let_go();
        self.app.statusbar.set_transfer(&self.line.borrow(), false);
    }

    fn let_go(&self) {
        if self.bar.replace(Held::Done) == Held::Up {
            self.app.connect.release();
        }
    }
}

/// The locale's paper with [`MARGIN_MM`] all round, which a diagram is printed on.
pub(crate) fn page_setup() -> gtk::PageSetup {
    let setup = gtk::PageSetup::new();
    setup.set_top_margin(MARGIN_MM, gtk::Unit::Mm);
    setup.set_bottom_margin(MARGIN_MM, gtk::Unit::Mm);
    setup.set_left_margin(MARGIN_MM, gtk::Unit::Mm);
    setup.set_right_margin(MARGIN_MM, gtk::Unit::Mm);
    setup
}

/// What an export is called in its toasts: the name the chooser was given.
pub(crate) fn dest_name(dest: &Path) -> String {
    dest.file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned())
}
