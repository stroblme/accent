//! SyncTeX between a LaTeX build's PDF and its sources (DESIGN.md, PDF): Go to Source from a page
//! and Show in PDF from a `.tex` tab, read by `accent_core::synctex`. A local vault's alone: a
//! remote vault's PDF is read from a copy with no SyncTeX file beside it, and its sources are on
//! the host.

use super::*;
use accent_core::synctex::{self, Synctex};

/// How many SyncTeX files stay read, the last ones asked about: each is held whole, and a long
/// document's is megabytes.
const KEEP: usize = 2;

thread_local! {
    /// The SyncTeX files read, oldest first, each with the etag it was read at: a rebuild is
    /// read again, and nothing else is.
    static READ: RefCell<Vec<(PathBuf, Etag, Arc<Synctex>)>> = const { RefCell::new(Vec::new()) };
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

/// Whether `key` names a LaTeX source, the one kind of file Show in PDF goes from.
fn is_tex(key: &str) -> bool {
    Path::new(key)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("tex"))
}

impl App {
    /// Go to Source (`win.pdf-go-to-source`): the line of the source the active PDF was typeset
    /// from at [`PdfTab::source_point`](pdftab::PdfTab::source_point), opened with the caret on
    /// it as Go to Definition opens one. A line of a file outside the vault — a class's, a
    /// package's — is named in a toast instead.
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

    /// Show in PDF (`win.show-in-pdf`): the line of text the caret's line of the active `.tex`
    /// went into, in the PDF of the first build near it that lists it ([`synctex::near`]),
    /// opened or brought forward as a link into a PDF is, and marked a moment.
    pub(crate) fn show_in_pdf(self: &Rc<Self>) {
        let Some(tab) = self.active() else {
            return;
        };
        let Some(tex) = self.local_tex(&tab.rel()) else {
            return;
        };
        let line = tab.cursor_line();
        let builds = synctex::near(&self.root(), &tex);
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

    /// Offer Go to Source over a PDF with a SyncTeX file beside it, and Show in PDF over a `.tex`
    /// with a build near it, both of a local vault. Disabled anywhere else, which takes each off
    /// the menu that names it and greys it in the palette.
    pub(crate) fn sync_synctex(&self, doc: Option<Doc>) {
        let source = doc
            .as_ref()
            .and_then(Doc::pdf)
            .is_some_and(|pdf| self.synctex_of(pdf).is_some());
        let show = (doc.as_ref().and_then(Doc::tab))
            .and_then(|tab| self.local_tex(&tab.rel()))
            .is_some_and(|tex| !synctex::near(&self.root(), &tex).is_empty());
        for (name, on) in [("pdf-go-to-source", source), ("show-in-pdf", show)] {
            let action = self.window.lookup_action(name);
            if let Some(action) = action.and_downcast::<gio::SimpleAction>() {
                action.set_enabled(on);
            }
        }
    }

    /// A PDF opened or read again, which a LaTeX build does to it: Show in PDF's mark lands once
    /// its pages are there, its SyncTeX file is read again if one is kept, and the offer follows.
    pub(crate) fn synctex_opened(self: &Rc<Self>, pdf: &Rc<pdftab::PdfTab>) {
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

    /// Show in PDF on a `.tex` tab's own menu, in a section of its own after the editor's items,
    /// shown while the window offers it. A secondary press asks again first: a build may have
    /// been made since the tab came forward, and the press may be on a pane other than the active
    /// one.
    pub(crate) fn offer_show_in_pdf(self: &Rc<Self>, tab: &Rc<Tab>) {
        if self.local_tex(&tab.rel()).is_none() {
            return;
        }
        let item = gio::MenuItem::new(
            Some(actions::label_of("win.show-in-pdf")),
            Some("win.show-in-pdf"),
        );
        item.set_attribute_value("hidden-when", Some(&"action-disabled".to_variant()));
        let section = gio::Menu::new();
        section.append_item(&item);
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
            move |_, _, _, _| app.sync_synctex(Some(Doc::Text(tab)))
        ));
        tab.view.add_controller(press);
    }

    /// The SyncTeX file of a local vault's PDF, where it has one ([`synctex::beside`]).
    fn synctex_of(&self, pdf: &pdftab::PdfTab) -> Option<PathBuf> {
        let key = pdf.key();
        if doc::is_loose_key(&key) || self.on_host(&key) {
            return None;
        }
        synctex::beside(&pdf.path())
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
