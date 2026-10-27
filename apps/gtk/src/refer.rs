//! A Files row dropped with Shift into what a pane shows writes a reference to it there, in the
//! form that surface reads: a link in a note or a markdown cell, the path from the file in code,
//! the quoted absolute path at a shell's prompt (DESIGN.md, Files). A plain drop opens or splits.

use super::*;
use accent_core::attachment::{link_name, wikilink};
use accent_core::note::Syntax;
use accent_core::notebook::Kind as CellKind;
use accent_core::path::{parent_dir, relative};
use attach::InsertAt;
use vte4::TerminalExt as _;

/// How a surface writes a reference.
#[derive(Clone, Copy)]
enum Form {
    Link(Syntax),
    Path,
}

/// What takes a reference at a point of a pane.
enum Surface {
    /// Text, a note's, a cell's or code's; `file` is the document it is in.
    Text {
        view: sourceview5::View,
        form: Form,
        file: String,
    },
    Shell(Rc<terminal::Term>),
}

/// Files rows over the middle of a pane, and what would take a reference to them there.
pub(crate) struct Reference {
    surface: Surface,
    /// Each row's path, and whether it is a folder's.
    rows: Vec<(String, bool)>,
    /// Shift is held: the drop offers a move alone, which is Shift on X11 and under Mutter
    /// alike, a row otherwise being only copied.
    pub shift: bool,
}

impl Reference {
    /// What the shade says while Shift is up.
    pub(crate) fn hint(&self) -> &'static str {
        match self.surface {
            Surface::Text {
                form: Form::Link(_),
                ..
            } => "Hold Shift to insert a link",
            _ => "Hold Shift to insert a path",
        }
    }
}

impl App {
    /// What the drag `drop` would leave over `pane`'s document at `at` with Shift: `None` for a
    /// drag of anything but Files rows, or over what takes no reference to them.
    pub(crate) fn reference_at(
        &self,
        pane: &Pane,
        drop: &gdk::Drop,
        at: (f64, f64),
    ) -> Option<Reference> {
        let rows = self.dragged_rows(drop)?;
        let surface = self.surface_at(pane, at)?;
        // A note follows no link to a folder, so its text takes none.
        let link = matches!(
            surface,
            Surface::Text {
                form: Form::Link(_),
                ..
            }
        );
        if link && rows.iter().any(|(_, dir)| *dir) {
            return None;
        }
        let shift = drop.actions() == gdk::DragAction::MOVE;
        Some(Reference {
            surface,
            rows,
            shift,
        })
    }

    /// Where `reference` would go, in `pane`'s document coordinates: the text position under
    /// `at`, or a shell's cursor.
    pub(crate) fn reference_caret(
        &self,
        pane: &Pane,
        reference: &Reference,
        at: (f64, f64),
    ) -> Option<graphene::Rect> {
        let doc: &gtk::Widget = pane.tabs.upcast_ref();
        match &reference.surface {
            Surface::Text { view, .. } => {
                let iter = iter_at(pane, view, at)?;
                let r = view.iter_location(&iter);
                let (x, y) =
                    view.buffer_to_window_coords(gtk::TextWindowType::Widget, r.x(), r.y());
                let p = graphene::Point::new(x as f32, y as f32);
                let top = view.compute_point(doc, &p)?;
                Some(graphene::Rect::new(
                    top.x() - 1.0,
                    top.y(),
                    2.0,
                    r.height() as f32,
                ))
            }
            Surface::Shell(term) => {
                let (column, row) = term.view.cursor_position();
                let (w, h) = (
                    term.view.char_width() as f32,
                    term.view.char_height() as f32,
                );
                let first = term.view.vadjustment().map_or(0.0, |a| a.value());
                let p = graphene::Point::new(column as f32 * w, (row as f64 - first) as f32 * h);
                let top = term.view.compute_point(doc, &p)?;
                Some(graphene::Rect::new(top.x() - 1.0, top.y(), 2.0, h))
            }
        }
    }

    /// Write `reference` in where the drop at `at` was let go, as one undo step with the caret
    /// after it, and give the pane and what took it the keyboard.
    pub(crate) fn refer(
        self: &Rc<Self>,
        pane: &Rc<Pane>,
        reference: Reference,
        at: (f64, f64),
    ) -> bool {
        let Some(vault) = self.vault().cloned() else {
            return false;
        };
        let rels: Vec<String> = reference.rows.into_iter().map(|(rel, _)| rel).collect();
        let took: gtk::Widget = match reference.surface {
            // Quoted and followed by a space, as GNOME Terminal types a dropped file.
            Surface::Shell(term) => {
                let root = vault.root();
                let typed: String = rels
                    .iter()
                    .map(|rel| format!("{} ", glib::shell_quote(root.join(rel)).to_string_lossy()))
                    .collect();
                term.view.feed_child(typed.as_bytes());
                term.view.clone().upcast()
            }
            Surface::Text { view, form, file } => {
                let Some(iter) = iter_at(pane, &view, at) else {
                    return false;
                };
                let at = InsertAt::new(&view.buffer(), &iter, &iter);
                let from = |rel: &String| relative(parent_dir(&file), rel);
                match form {
                    Form::Path => at.fill(&rels.iter().map(from).collect::<Vec<_>>().join("\n")),
                    Form::Link(Syntax::Org) => {
                        let link = |rel: &String| {
                            accent_core::org::link(&format!("file:{}", from(rel)), None)
                        };
                        at.fill(&rels.iter().map(link).collect::<Vec<_>>().join("\n"));
                    }
                    // By name unless the name finds another file, which the index answers off the
                    // main loop; the marks hold the place meanwhile.
                    Form::Link(Syntax::Markdown) => {
                        glib::spawn_future_local(async move {
                            let links = crate::work::off_thread("refer", move || {
                                let by_name = |rel: &String| {
                                    vault.resolve_link(&link_name(rel)).ok().flatten()
                                };
                                rels.iter()
                                    .map(|rel| wikilink(rel, by_name(rel).as_deref()))
                                    .collect::<Vec<_>>()
                            })
                            .await
                            .unwrap_or_default();
                            if !links.is_empty() {
                                at.fill(&links.join("\n"));
                            }
                        });
                    }
                }
                view.upcast()
            }
        };
        if self.set_active_pane(pane) {
            self.sync_active();
        }
        // From an idle, the drag still ending around the drop.
        glib::idle_add_local_once(move || {
            took.grab_focus();
        });
        true
    }

    /// The Files rows a drag out of this window's tree carries, each with whether it is a
    /// folder: a file's row offers its path as a `String`, a folder's only its move payload, a
    /// marked set its list.
    fn dragged_rows(&self, drop: &gdk::Drop) -> Option<Vec<(String, bool)>> {
        if let Some(rel) = one_file(drop) {
            return Some(vec![(rel, false)]);
        }
        let content = drop.drag()?.content();
        let read = |kind: glib::Type| content.value(kind).ok();
        if let Some(set) =
            read(gtk::StringList::static_type()).and_then(|v| v.get::<gtk::StringList>().ok())
        {
            let marked = self.tree.get()?.marked();
            let dir = |rel: &str| marked.iter().any(|(r, dir)| r == rel && *dir);
            let rows = (0..set.n_items()).filter_map(|i| set.string(i));
            return Some(rows.map(|rel| (rel.to_string(), dir(&rel))).collect());
        }
        let one = read(gtk::StringObject::static_type())?
            .get::<gtk::StringObject>()
            .ok()?;
        Some(vec![(one.string().to_string(), true)])
    }

    /// What in front of `pane` takes a reference at `at`: an editable note, code file or
    /// notebook cell of the vault, or a shell. Nothing while a document is presented.
    fn surface_at(&self, pane: &Pane, (x, y): (f64, f64)) -> Option<Surface> {
        if self.presenting.get().is_some() {
            return None;
        }
        let doc = self.doc_of(pane).filter(|doc| !doc.is_loose())?;
        let picked = pane.tabs.pick(x, y, gtk::PickFlags::DEFAULT)?;
        let within = |w: &gtk::Widget| &picked == w || picked.is_ancestor(w);
        match doc {
            Doc::Terminal(term) => {
                within(term.view.upcast_ref()).then(|| Surface::Shell(term.clone()))
            }
            Doc::Text(tab) => {
                let form = match tab.flavour() {
                    Flavour::Note(syntax) => Form::Link(syntax),
                    _ => Form::Path,
                };
                (within(tab.view.upcast_ref()) && tab.view.is_editable()).then(|| Surface::Text {
                    view: tab.view.clone(),
                    form,
                    file: tab.rel(),
                })
            }
            Doc::Notebook(notebook) => {
                let view = picked.ancestor(sourceview5::View::static_type())?;
                let form = match notebook.cell_kind(&view)? {
                    CellKind::Markdown => Form::Link(Syntax::Markdown),
                    CellKind::Code => Form::Path,
                    CellKind::Raw => return None,
                };
                let view = view
                    .downcast::<sourceview5::View>()
                    .ok()
                    .filter(|v| v.is_editable())?;
                Some(Surface::Text {
                    view,
                    form,
                    file: notebook.key(),
                })
            }
            _ => None,
        }
    }
}

/// The path a drag of one file's row out of this window's tree carries, as its own `String`: a
/// folder's or a marked set's drag reads as a `String` only through their files' list.
pub(crate) fn one_file(drop: &gdk::Drop) -> Option<String> {
    let content = drop.drag()?.content();
    content
        .value(String::static_type())
        .ok()?
        .get::<String>()
        .ok()
}

/// The text position under `at`, in `pane`'s document coordinates, by the paste rule a drop of
/// text uses.
fn iter_at(pane: &Pane, view: &sourceview5::View, (x, y): (f64, f64)) -> Option<gtk::TextIter> {
    let p = pane
        .tabs
        .compute_point(view, &graphene::Point::new(x as f32, y as f32))?;
    let (x, y) =
        view.window_to_buffer_coords(gtk::TextWindowType::Widget, p.x() as i32, p.y() as i32);
    Some(crate::editor::pressed_at(view, x, y))
}
