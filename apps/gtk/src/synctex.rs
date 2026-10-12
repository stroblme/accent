//! SyncTeX between a LaTeX build's PDF and its sources (DESIGN.md, PDF): Go to Source from a page
//! and Show in PDF from a `.tex` tab, read by `accent_core::synctex`. A local vault's alone: a
//! remote vault's PDF is read from a copy with no SyncTeX file beside it, and its sources are on
//! the host.

use super::*;
use accent_core::synctex::{self, Synctex};
use std::time::SystemTime;

/// Maximum recently used SyncTeX files retained whole, bounding memory for large documents.
const KEEP: usize = 2;

/// Disabled-action explanation for LaTeX builds missing SyncTeX; menu items use the shorter label below.
pub(crate) const NO_SYNCTEX: &str = "This build has no SyncTeX data: build with -synctex=1";
const NO_SYNCTEX_LABEL: &str = "(no SyncTeX data)";

thread_local! {
    /// The SyncTeX files read, oldest first, each with the etag it was read at: a rebuild is
    /// read again, and nothing else is.
    static READ: RefCell<Vec<(PathBuf, Etag, Arc<Synctex>)>> = const { RefCell::new(Vec::new()) };
    /// Trusted PDF build state keyed by path, surviving tab closure for this process.
    static TRUSTED: RefCell<HashMap<PathBuf, Trust>> = RefCell::new(HashMap::new());
}

/// Remember the last PDF build and Accent writes since it.
/// Ink and highlights preserve SyncTeX coordinates; page edits invalidate them until rebuilding.
#[derive(Clone)]
struct Trust {
    /// The SyncTeX file, and when it was written.
    file: PathBuf,
    built: SystemTime,
    /// The PDF as accent's own last write left it, if accent has written it since.
    written: Option<Etag>,
    moved: bool,
}

/// Find the PDF's current SyncTeX build through [`synctex::beside`] or [`Trust`].
/// Accent-only ink and highlight saves retain trust even when the PDF mtime becomes newer.
fn build_of(pdf: &Path) -> Option<PathBuf> {
    let trust = TRUSTED.with_borrow(|trusted| trusted.get(pdf).cloned());
    if trust.as_ref().is_some_and(|trust| trust.moved) {
        return None;
    }
    synctex::beside(pdf).or_else(|| {
        let trust = trust?;
        let built = std::fs::metadata(&trust.file)
            .and_then(|m| m.modified())
            .ok();
        let ours = trust.written.is_some() && trust.written == Etag::of(pdf).ok();
        (ours && built == Some(trust.built)).then_some(trust.file)
    })
}

/// Whether `pdf` is a LaTeX build missing current SyncTeX.
/// Page-edit invalidation differs: the SyncTeX file exists but its page numbers no longer match.
fn without_synctex(root: &Path, pdf: &Path) -> bool {
    let moved = TRUSTED.with_borrow(|trusted| trusted.get(pdf).is_some_and(|trust| trust.moved));
    !moved && build_of(pdf).is_none() && synctex::is_build(root, pdf)
}

/// The SyncTeX file at `path`, read on a worker unless the one kept is what is there now.
async fn read(path: PathBuf) -> Result<Arc<Synctex>, String> {
    let etag = Etag::of(&path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let kept = READ.with_borrow(|read| {
        read.iter()
            .find(|(at, seen, _)| *at == path && *seen == etag)
            .map(|(.., synctex)| synctex.clone())
    });
    if let Some(synctex) = kept {
        return Ok(synctex);
    }
    let file = path.clone();
    let synctex =
        Arc::new(work::attempt("read the SyncTeX file", move || Synctex::read(&file)).await?);
    READ.with_borrow_mut(|read| {
        read.retain(|(at, ..)| *at != path);
        read.push((path, etag, synctex.clone()));
        if read.len() > KEEP {
            read.remove(0);
        }
    });
    Ok(synctex)
}

/// `action` (Go to Source or Show in PDF) as a menu offers it: hidden while it is disabled, or
/// greyed and saying why where it is `missing` only a SyncTeX file.
pub(crate) fn menu_item(action: &str, missing: bool) -> gio::MenuItem {
    let label = actions::label_of(action);
    if missing {
        let label = format!("{label} {NO_SYNCTEX_LABEL}");
        return gio::MenuItem::new(Some(&label), Some(action));
    }
    let item = gio::MenuItem::new(Some(label), Some(action));
    item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
    item
}

/// Whether `key` names a LaTeX source, the one kind of file Show in PDF goes from.
fn is_tex(key: &str) -> bool {
    Path::new(key)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tex"))
}

impl App {
    /// Open the source line at [`PdfTab::source_point`](pdftab::PdfTab::source_point).
    /// Files outside the vault are reported in a toast instead of opened as vault notes.
    pub(crate) fn go_to_source(self: &Rc<Self>) {
        let Some(pdf) = self.active_pdf() else {
            return;
        };
        let (Some(file), Some((page, x, y))) = (self.synctex_of(&pdf), pdf.source_point()) else {
            return;
        };
        let app = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let synctex = match read(file).await {
                Ok(synctex) => synctex,
                Err(why) => {
                    if let Some(app) = app.upgrade() {
                        app.toast(&why);
                    }
                    return;
                }
            };
            let hit = work::off_thread("SyncTeX", move || synctex.edit(page, x, y)).await;
            let Some(app) = app.upgrade() else {
                return;
            };
            let Some(hit) = hit.flatten() else {
                return app.cannot("go to the source", "nothing was typeset there");
            };
            let Some(key) = app.vault_key(&hit.file) else {
                let file = crate::start::abbreviate(&hit.file, Some(&glib::home_dir()));
                return app.cannot("go to the source", format!("{file} is outside this vault"));
            };
            let at = accent_api::Pos {
                line: hit.line.saturating_sub(1),
                character: 0,
            };
            app.open_at(&Location {
                path: key,
                range: accent_api::Range { start: at, end: at },
                ..Default::default()
            });
        });
    }

    /// Find a nearby SyncTeX build containing the active .tex line ([`synctex::near`]),
    /// open or raise its PDF and temporarily highlight the resulting location.
    pub(crate) fn show_in_pdf(self: &Rc<Self>) {
        let Some(tab) = self.active() else {
            return;
        };
        let Some(tex) = self.local_tex(&tab.rel()) else {
            return;
        };
        let line = tab.cursor_line();
        let builds = synctex::near(&self.root(), &tex, build_of);
        let app = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut found = None;
            for file in builds {
                let Ok(synctex) = read(file.clone()).await else {
                    continue;
                };
                let tex = tex.clone();
                // By the path the vault has, or by the one the engine saw through a link.
                let spot = work::off_thread("SyncTeX", move || {
                    let real = || std::fs::canonicalize(&tex).ok();
                    (synctex.view(&tex, line)).or_else(|| synctex.view(&real()?, line))
                });
                if let Some(spot) = spot.await.flatten() {
                    found = Some((file, spot));
                    break;
                }
            }
            let Some(app) = app.upgrade() else {
                return;
            };
            let name = doc::file_name(&tab.rel()).to_string();
            let Some((file, spot)) = found else {
                return app.cannot("show in PDF", format!("no LaTeX build here lists {name}"));
            };
            let Some(key) = synctex::pdf_of(&file).and_then(|pdf| app.vault_key(&pdf)) else {
                return;
            };
            app.mark();
            app.open_as(&key, Opened::Preview);
            if let Some(Doc::Pdf(pdf)) = app.doc_for(&key) {
                let rect = accent_core::pdf::Rect {
                    left: spot.left,
                    top: spot.top,
                    right: spot.right,
                    bottom: spot.bottom,
                };
                pdf.show_spot(spot.page, rect);
            }
        });
    }

    /// Enable SyncTeX for local PDFs with mappings and .tex files with nearby builds.
    /// Hide unavailable actions except missing-map builds, which remain disabled with an explanation.
    pub(crate) fn sync_synctex(&self, doc: Option<Doc>) {
        if let Some(pdf) = doc.as_ref().and_then(Doc::pdf) {
            pdf.set_without_synctex(self.synctex_missing(doc.as_ref()).0);
        }
        let source = doc
            .as_ref()
            .and_then(Doc::pdf)
            .is_some_and(|pdf| self.synctex_of(pdf).is_some());
        let show = (doc.as_ref().and_then(Doc::tab))
            .and_then(|tab| self.local_tex(&tab.rel()))
            .is_some_and(|tex| !synctex::near(&self.root(), &tex, build_of).is_empty());
        for (name, on) in [("pdf-go-to-source", source), ("show-in-pdf", show)] {
            let action = self.window.lookup_action(name);
            if let Some(action) = action.and_downcast::<gio::SimpleAction>() {
                action.set_enabled(on);
            }
        }
    }

    /// Refresh build trust and SyncTeX after opening or reloading a PDF.
    /// Retain trust for Accent-only saves; otherwise require a map no older than the PDF.
    /// Apply pending source highlights once pages arrive and refresh action availability.
    pub(crate) fn synctex_opened(self: &Rc<Self>, pdf: &Rc<pdftab::PdfTab>) {
        if self.is_local_pdf(pdf) {
            let path = pdf.path();
            let fresh = synctex::beside(&path).and_then(|file| {
                let built = std::fs::metadata(&file).and_then(|m| m.modified()).ok()?;
                Some(Trust {
                    file,
                    built,
                    written: None,
                    moved: false,
                })
            });
            TRUSTED.with_borrow_mut(|trusted| match fresh {
                Some(trust) => drop(trusted.insert(path, trust)),
                None => {
                    let ours = |trust: &Trust| trust.written == Etag::of(&path).ok();
                    if !trusted.get(&path).is_some_and(ours) {
                        trusted.remove(&path);
                    }
                }
            });
        }
        pdf.show_pending_spot();
        self.sync_synctex(self.active_doc());
        if let Some(file) = self.synctex_of(pdf)
            && READ.with_borrow(|read| read.iter().any(|(at, ..)| *at == file))
        {
            glib::spawn_future_local(async move {
                let _ = read(file).await;
            });
        }
    }

    /// Add Show in PDF after editor menu items when available.
    /// Recheck on secondary press because builds or active panes may have changed.
    pub(crate) fn offer_show_in_pdf(self: &Rc<Self>, tab: &Rc<Tab>) {
        if self.local_tex(&tab.rel()).is_none() {
            return;
        }
        let section = gio::Menu::new();
        section.append_item(&menu_item("win.show-in-pdf", false));
        let menu = gio::Menu::new();
        menu.append_section(None, &section);
        if let Some(previous) = tab.view.extra_menu() {
            menu.append_section(None, &previous);
        }
        tab.view.set_extra_menu(Some(&menu));
        let press = gtk::GestureClick::builder()
            .button(gdk::BUTTON_SECONDARY)
            .propagation_phase(gtk::PropagationPhase::Capture)
            .build();
        press.connect_pressed(glib::clone!(
            #[weak(rename_to = app)]
            self,
            #[weak]
            tab,
            move |_, _, _, _| {
                let doc = Doc::Text(tab);
                app.sync_synctex(Some(doc.clone()));
                let missing = app.synctex_missing(Some(&doc)).1;
                section.remove_all();
                section.append_item(&menu_item("win.show-in-pdf", missing));
            }
        ));
        tab.view.add_controller(press);
    }

    /// Whether `doc` lacks only SyncTeX: a local LaTeX PDF build or .tex file with a nearby PDF
    /// that was built without `-synctex=1`.
    pub(crate) fn synctex_missing(&self, doc: Option<&Doc>) -> (bool, bool) {
        let root = self.root();
        let source = (doc.and_then(Doc::pdf))
            .filter(|pdf| self.is_local_pdf(pdf))
            .is_some_and(|pdf| without_synctex(&root, &pdf.path()));
        let show = (doc.and_then(Doc::tab))
            .and_then(|tab| self.local_tex(&tab.rel()))
            .is_some_and(|tex| {
                synctex::near(&root, &tex, build_of).is_empty()
                    && synctex::pdf_near(&root, &tex)
                        .is_some_and(|pdf| without_synctex(&root, &pdf))
            });
        (source, show)
    }

    /// Preserve build trust after Accent writes ink or highlights; page edits invalidate it.
    pub(crate) fn synctex_written(&self, pdf: &pdftab::PdfTab) {
        let path = pdf.path();
        TRUSTED.with_borrow_mut(|trusted| {
            if let Some(trust) = trusted.get_mut(&path) {
                trust.written = Etag::of(&path).ok();
            }
        });
    }

    /// Invalidate mapping after page insertion, deletion, movement, Undo or Redo until the next build.
    pub(crate) fn synctex_repaged(&self, pdf: &pdftab::PdfTab) {
        TRUSTED.with_borrow_mut(|trusted| {
            if let Some(trust) = trusted.get_mut(&pdf.path()) {
                trust.moved = true;
            }
        });
        self.sync_synctex(self.active_doc());
    }

    /// The SyncTeX file of a local vault's PDF, where it has one ([`build_of`]).
    fn synctex_of(&self, pdf: &pdftab::PdfTab) -> Option<PathBuf> {
        self.is_local_pdf(pdf)
            .then(|| build_of(&pdf.path()))
            .flatten()
    }

    /// Whether `pdf` is a local vault's, whose own file the tab reads.
    fn is_local_pdf(&self, pdf: &pdftab::PdfTab) -> bool {
        let key = pdf.key();
        !doc::is_loose_key(&key) && !self.on_host(&key)
    }

    /// The path of `key` on this machine, where it is a `.tex` of a local vault.
    fn local_tex(&self, key: &str) -> Option<PathBuf> {
        let local = self.vault().is_some() && !doc::is_loose_key(key) && !self.on_host(key);
        (local && is_tex(key)).then(|| self.root().join(key))
    }

    /// The key of `path` in this vault, read through the root as the vault names it or as the
    /// engine saw it, with its links resolved; `None` outside the vault.
    fn vault_key(&self, path: &Path) -> Option<String> {
        self.vault()?;
        let root = self.root();
        let rel = match path.strip_prefix(&root) {
            Ok(rel) => rel.to_path_buf(),
            Err(_) => path
                .strip_prefix(std::fs::canonicalize(&root).ok()?)
                .ok()?
                .to_path_buf(),
        };
        rel.to_str().map(str::to_string)
    }
}
