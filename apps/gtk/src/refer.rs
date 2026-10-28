//! A Files row dropped with Shift into what a pane shows writes a reference to it there, in the
//! form that surface reads: a link in a note or a markdown cell, the path from the file in code,
//! the quoted absolute path at a shell's prompt (DESIGN.md, Files). A plain drop opens or splits.
//! An Outline, Search or References row does the same with Shift ([`Dragged`]), and nothing
//! without it; an Outline row of a note's headings names its heading too.

use super::*;
use accent_core::attachment::{link_name, wikilink};
use accent_core::note::Syntax;
use accent_core::notebook::Kind as CellKind;
use accent_core::path::{parent_dir, relative};
use attach::InsertAt;
use vte4::TerminalExt as _;

/// What an Outline, Search or References row carries into a pane: the file it names and, an
/// Outline row of a note's, the heading. Only a reference reads it, so a plain drop opens nothing.
#[derive(Clone, Debug, glib::Boxed)]
#[boxed_type(name = "AccentDragged")]
pub(crate) struct Dragged {
    pub rel: String,
    pub heading: Option<String>,
}

/// What the row a list item is bound to names, if anything.
pub(crate) type Names = Rc<dyn Fn(&gtk::ListItem) -> Option<Dragged>>;

/// A list's rows dragged as references: what each names, and who hears a drag start and end,
/// which puts every pane's drop sheet up.
#[derive(Clone)]
pub(crate) struct RowDrag {
    pub of: Names,
    pub dragging: Rc<dyn Fn(bool)>,
}

/// Rows a drag carries, each with whether it is a folder, and the heading an Outline row names.
type Rows = (Vec<(String, bool)>, Option<String>);

impl RowDrag {
    /// Make `row`, the widget `item` shows, draggable as what it names when the drag starts:
    /// offering a copy or a move, so Shift reads as the move, as a Files row's does.
    pub(crate) fn attach(&self, row: &impl IsA<gtk::Widget>, item: &gtk::ListItem) {
        let source = gtk::DragSource::builder()
            .actions(gdk::DragAction::COPY | gdk::DragAction::MOVE)
            .build();
        let (of, item) = (self.of.clone(), item.downgrade());
        source.connect_prepare(move |_, _, _| {
            let dragged = of(&item.upgrade()?)?;
            Some(gdk::ContentProvider::for_value(&dragged.to_value()))
        });
        let begin = self.dragging.clone();
        source.connect_drag_begin(move |_, _| begin(true));
        let end = self.dragging.clone();
        source.connect_drag_end(move |_, _, _| end(false));
        row.add_controller(source);
    }
}

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
    /// The heading an Outline row names, in the one file `rows` holds then.
    heading: Option<String>,
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
        let (rows, heading) = self.dragged_rows(drop)?;
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
            heading,
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
        let heading = reference.heading;
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
                    Form::Path => {
                        let path = |rel: &String| code_path(from(rel), rel, heading.as_deref());
                        at.fill(&rels.iter().map(path).collect::<Vec<_>>().join("\n"));
                    }
                    Form::Link(Syntax::Org) => {
                        let link = |rel: &String| org_link(&from(rel), heading.as_deref());
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
                                    .map(|rel| {
                                        let link = wikilink(rel, by_name(rel).as_deref());
                                        to_heading(&link, heading.as_deref())
                                    })
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
    /// marked set its list. Or the one file an Outline, Search or References row names, and its
    /// heading.
    fn dragged_rows(&self, drop: &gdk::Drop) -> Option<Rows> {
        if let Some(rel) = one_file(drop) {
            return Some((vec![(rel, false)], None));
        }
        let content = drop.drag()?.content();
        let read = |kind: glib::Type| content.value(kind).ok();
        if let Some(row) = read(Dragged::static_type()).and_then(|v| v.get::<Dragged>().ok()) {
            return Some((vec![(row.rel, false)], row.heading));
        }
        if let Some(set) =
            read(gtk::StringList::static_type()).and_then(|v| v.get::<gtk::StringList>().ok())
        {
            let marked = self.tree.get()?.marked();
            let dir = |rel: &str| marked.iter().any(|(r, dir)| r == rel && *dir);
            let rows = (0..set.n_items()).filter_map(|i| set.string(i));
            return Some((rows.map(|rel| (rel.to_string(), dir(&rel))).collect(), None));
        }
        let one = read(gtk::StringObject::static_type())?
            .get::<gtk::StringObject>()
            .ok()?;
        Some((vec![(one.string().to_string(), true)], None))
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

/// `link`, a note's `[[…]]`, to `heading` in what it names, as Obsidian links one.
fn to_heading(link: &str, heading: Option<&str>) -> String {
    match (heading, link.strip_suffix("]]")) {
        (Some(heading), Some(open)) => format!("{open}#{heading}]]"),
        _ => link.to_string(),
    }
}

/// A reference in code: `path`, `rel`'s from the file, to `heading` in a markdown note by its
/// GitHub slug, as a markdown link reaches one; an org file's heading has no such anchor.
fn code_path(path: String, rel: &str, heading: Option<&str>) -> String {
    let slug = heading.and_then(|h| accent_core::markdown::slugs([h]).into_iter().next());
    match (slug, Syntax::of(rel)) {
        (Some(slug), Syntax::Markdown) => format!("{path}#{slug}"),
        _ => path,
    }
}

/// A reference in an org file: a `file:` link to `path`, to `heading` by org's own `::*` search,
/// which accent follows into a markdown note's headings too.
fn org_link(path: &str, heading: Option<&str>) -> String {
    let search = heading.map(|h| format!("::*{h}")).unwrap_or_default();
    accent_core::org::link(&format!("file:{path}{search}"), None)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A heading goes into each surface's own link: a wikilink's `#`, a markdown note's slug after
    /// a path in code, org's `::*` search; with none, the file's link as before.
    #[test]
    fn a_headingis_written_as_each_surface_links_one() {
        assert_eq!(
            to_heading("[[Plan]]", Some("Next Steps")),
            "[[Plan#Next Steps]]"
        );
        assert_eq!(to_heading("[[Plan]]", None), "[[Plan]]");
        let path = || "../docs/Plan.md".to_string();
        assert_eq!(
            code_path(path(), "docs/Plan.md", Some("Next Steps")),
            "../docs/Plan.md#next-steps"
        );
        assert_eq!(code_path(path(), "docs/Plan.org", Some("Next")), path());
        assert_eq!(code_path(path(), "docs/Plan.md", None), path());
        assert_eq!(org_link("Plan.md", Some("Next")), "[[file:Plan.md::*Next]]");
        assert_eq!(org_link("Plan.md", None), "[[file:Plan.md]]");
    }
}
