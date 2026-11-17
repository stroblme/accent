//! The diagram tab's side of the Properties pane: filling it from the selection and the page,
//! and turning its changes into edits, a drag's preview and the clipboard's Copy and Paste.

use std::rc::Rc;

use accent_drawio::CellId;
use adw::prelude::*;
use gtk::glib;

use super::{Change, Clip, Placed, Target, effects, ends, number};
use crate::diagram::view::Edit;
use crate::diagram::{DiagramTab, Tool};

impl DiagramTab {
    /// What the Properties pane shows for this diagram.
    pub fn properties(&self) -> gtk::Widget {
        self.props.widget().clone()
    }

    /// Put the selection's look, or the page's, into the Properties pane, and the page's layers.
    pub(in crate::diagram) fn fill_props(&self) {
        // What the rows hold is the selection's still; it lands before they show the model.
        self.props.flush();
        let (target, layers, current) = {
            let editor = self.editor.borrow();
            let Ok(page) = editor.page(self.page_index.get()) else {
                return;
            };
            let layers: Vec<crate::diagram::layers::Layer> = page
                .layers()
                .iter()
                .rev()
                .map(|l| crate::diagram::layers::Layer {
                    id: l.id.clone(),
                    name: l.label().to_string(),
                    visible: l.is_visible(),
                    locked: l.is_locked(),
                })
                .collect();
            let current = editor
                .current_layer(self.page_index.get())
                .map(str::to_string);
            let selection = self.selection.borrow();
            let cells: Vec<&accent_drawio::Cell> =
                selection.iter().filter_map(|id| page.cell(id)).collect();
            let pinned = |id: &str| self.view.sheet().is_some_and(|s| s.is_pinned(id));
            let geometry = match cells.as_slice() {
                [cell] if cell.vertex => cell.geometry.as_ref().filter(|g| !g.relative),
                _ => None,
            };
            let geometry = geometry.map(|g| (g.rect(), pinned(&cells[0].id)));
            let default_tool = match cells.as_slice() {
                [cell] => Tool::of(cell).map(|tool| {
                    let key = tool.default_attr().expect("a creation tool");
                    let default = accent_drawio::model::attr(&editor.file().attrs, key);
                    (tool, default == Some(cell.style.to_string().as_str()))
                }),
                _ => None,
            };
            let target = match cells.first() {
                Some(first) => Target::Cells {
                    style: first.style.resolve(first.edge),
                    raw: first.style.to_string(),
                    count: cells.len(),
                    vertices: cells.iter().any(|c| c.vertex),
                    edges: cells.iter().any(|c| c.edge),
                    fills: cells.iter().any(|c| c.takes_fill()),
                    swimlane: cells
                        .iter()
                        .any(|c| c.vertex && c.style.resolve(false).shape() == "swimlane"),
                    effects: effects::Shown::of(
                        &cells,
                        self.view.sheet().as_deref().map(|s| &s.scene),
                    ),
                    geometry,
                    default_tool,
                },
                None => Target::Page {
                    name: page.name().to_string(),
                    size: page.size(),
                    background: page.background(),
                },
            };
            (target, layers, current)
        };
        self.props.fill(&target);
        self.fill_ends();
        self.props.fill_layers(&layers, current.as_deref());
    }

    /// A row of the Properties pane changed.
    pub(in crate::diagram) fn apply_property(self: &Rc<Self>, change: Change) {
        if let Change::Held(placed) = change {
            return self.show_held(placed);
        }
        // What lands is the held change, or comes after it has landed.
        self.view.show_held(None);
        let ids = self.selection();
        match change {
            Change::Style(pairs) if !ids.is_empty() => {
                let pairs: Vec<(&str, Option<&str>)> =
                    pairs.iter().map(|(k, v)| (*k, v.as_deref())).collect();
                self.edit(|e, page| e.set_styles(page, &ids, &pairs));
            }
            Change::EdgeStyle(pairs) => {
                let edges: Vec<CellId> = {
                    let editor = self.editor.borrow();
                    let page = editor.page(self.page_index.get()).ok();
                    let edge = |id: &CellId| page.and_then(|p| p.cell(id)).is_some_and(|c| c.edge);
                    ids.into_iter().filter(edge).collect()
                };
                let pairs: Vec<(&str, Option<&str>)> =
                    pairs.iter().map(|(k, v)| (*k, v.as_deref())).collect();
                if !edges.is_empty() {
                    self.edit(|e, page| e.set_styles(page, &edges, &pairs));
                }
            }
            Change::Raw(style) => {
                if let [id] = ids.as_slice() {
                    self.edit(|e, page| e.set_style_string(page, id, &style));
                }
            }
            // The cell's style becomes the default, or the default goes where it is that style.
            Change::DefaultStyle => {
                let captured = {
                    let editor = self.editor.borrow();
                    let page = editor.page(self.page_index.get()).ok();
                    let [id] = ids.as_slice() else { return };
                    page.and_then(|p| p.cell(id)).and_then(|cell| {
                        let key = Tool::of(cell)?.default_attr()?;
                        let style = cell.style.to_string();
                        let is_default = accent_drawio::model::attr(&editor.file().attrs, key)
                            == Some(style.as_str());
                        Some((key, (!is_default).then_some(style)))
                    })
                };
                if let Some((key, style)) = captured {
                    match style {
                        Some(style) => self.editor.borrow_mut().set_file_attr(key, &style),
                        None => self.editor.borrow_mut().clear_file_attr(key),
                    }
                    self.changed();
                }
            }
            Change::EditStyle => self.on_style.emit(self),
            Change::PageAttr(key, value) => {
                self.edit(|e, page| e.set_page_attr(page, key, value.as_deref()));
            }
            Change::PageName(name) if !name.trim().is_empty() => {
                self.rename_page(name.trim());
            }
            Change::Geometry(rect) => {
                if let [id] = ids.as_slice() {
                    self.set_geometry(id, rect);
                }
            }
            Change::End(source, at) => self.place_end(source, at),
            Change::Fixed(what, on) if !ids.is_empty() => {
                self.edit(|e, page| e.fix(page, &ids, what, on))
            }
            Change::Flip(across) if !ids.is_empty() => {
                let shown = self.view.sheet().unwrap_or_default();
                self.edit(|e, page| e.flip(page, &ids, across, &shown.scene))
            }
            Change::Copy(clip) => self.copy_property(clip),
            Change::Paste(clip) => self.paste_property(clip),
            Change::Layer(change) => self.apply_layer(change),
            _ => {}
        }
    }

    /// A row of the Layers group changed.
    fn apply_layer(self: &Rc<Self>, change: crate::diagram::layers::LayerChange) {
        use crate::diagram::layers::LayerChange as L;
        let pick = |id: Option<CellId>| {
            self.editor.borrow_mut().set_current_layer(id);
            self.fill_props();
        };
        match change {
            L::Add => {
                let mut added = None;
                self.edit(|e, page| {
                    added = Some(e.add_layer(page, "Untitled Layer")?);
                    Ok(())
                });
                // As draw.io's Add Layer does, the new layer is the one drawn into.
                if added.is_some() {
                    pick(added);
                }
            }
            L::Pick(id) => pick(Some(id)),
            L::Rename(id, name) => self.edit(|e, page| e.rename_layer(page, &id, name.trim())),
            L::Show(id, on) => self.edit(|e, page| e.set_visible(page, &id, on)),
            L::Lock(id, on) => self.edit(|e, page| {
                let ids = std::slice::from_ref(&id);
                e.set_style(page, ids, "locked", on.then_some("1"))
            }),
            L::Raise(id, up) => {
                let z = match up {
                    true => accent_drawio::ZOrder::Forward,
                    false => accent_drawio::ZOrder::Backward,
                };
                self.edit(|e, page| e.reorder(page, std::slice::from_ref(&id), z));
            }
            L::Delete(id) => self.edit(|e, page| e.delete_layer(page, &id)),
        }
    }

    /// Show on the canvas what a geometry or Rotation row holds for the one shape or line.
    fn show_held(&self, placed: Placed) {
        let held = {
            let selection = self.selection.borrow();
            let [id] = selection.as_slice() else { return };
            let editor = self.editor.borrow();
            let Ok(page) = editor.page(self.page_index.get()) else {
                return;
            };
            let id = id.clone();
            match placed {
                Placed::Frame(r) => {
                    let o = page.origin_of(&id);
                    Edit::Resize {
                        id,
                        rect: r.translate(o.x, o.y),
                        alone: false,
                    }
                }
                Placed::Turn(degrees) => Edit::Rotate { id, degrees },
                Placed::Ends(at) => return self.view.show_held(ends::held(page, id, at)),
            }
        };
        self.view.show_held(Some(held));
    }

    /// A drag under way in the one shape's Position, Size or Rotation rows, or the one line's
    /// ends, every frame of it and nothing written until the drop; once it is over, the model
    /// again. What a row still holds lands as the drag begins, so the drag starts from it.
    /// Called as the canvas paints, so the rows change right after.
    pub(in crate::diagram) fn drag_preview(self: &Rc<Self>, edit: Option<&Edit>) {
        let was = self.props.dragging.replace(edit.is_some());
        let placed = edit.and_then(|edit| self.placed(edit).or_else(|| self.dragged_ends(edit)));
        let (began, over) = (!was && edit.is_some(), was && edit.is_none());
        if !began && !over && placed.is_none() {
            return;
        }
        let tab = Rc::downgrade(self);
        glib::idle_add_local_full(glib::Priority::HIGH_IDLE, move || {
            let Some(tab) = tab.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if began {
                tab.props.flush();
            }
            match placed {
                Some(placed) => tab.props.show_drag(placed),
                None if over => tab.fill_props(),
                None => {}
            }
            glib::ControlFlow::Break
        });
    }

    /// What a drag's `edit` makes of the one shape selected, as its rows show it.
    fn placed(&self, edit: &Edit) -> Option<Placed> {
        let selection = self.selection.borrow();
        let [id] = selection.as_slice() else {
            return None;
        };
        let editor = self.editor.borrow();
        let page = editor.page(self.page_index.get()).ok()?;
        let cell = page.cell(id).filter(|c| c.vertex)?;
        let g = cell.geometry.as_ref().filter(|g| !g.relative)?.rect();
        match edit {
            Edit::Move { ids, delta } if ids.contains(id) => {
                Some(Placed::Frame(g.translate(delta.x, delta.y)))
            }
            Edit::Resize {
                id: resized, rect, ..
            } if resized == id => {
                let o = page.origin_of(id);
                Some(Placed::Frame(rect.translate(-o.x, -o.y)))
            }
            Edit::Rotate {
                id: turned,
                degrees,
            } if turned == id => Some(Placed::Turn(*degrees)),
            _ => None,
        }
    }

    /// What the canvas shows of a Properties row's held change, for a drill.
    #[cfg(feature = "bench")]
    pub fn held(&self) -> Option<String> {
        self.view.held().map(|edit| format!("{edit:?}"))
    }

    /// Give shape `id` the place and size `rect` in its parent, as the file has them.
    fn set_geometry(self: &Rc<Self>, id: &str, rect: accent_drawio::Rect) {
        self.edit(|e, page| {
            let o = e.page(page)?.origin_of(id);
            e.resize(page, id, rect.translate(o.x, o.y), false)
        });
    }

    /// The one selected cell's position (`x, y`), size (`w, h`) or style string onto the
    /// clipboard, as plain text with its kind beside it ([`Clip::held`]).
    pub(in crate::diagram) fn copy_property(self: &Rc<Self>, clip: Clip) {
        let text = {
            let editor = self.editor.borrow();
            let selection = self.selection.borrow();
            let [id] = selection.as_slice() else { return };
            let Some(cell) = editor
                .page(self.page_index.get())
                .ok()
                .and_then(|p| p.cell(id))
            else {
                return;
            };
            let g = cell.geometry.as_ref().map(|g| g.rect()).unwrap_or_default();
            let pair = |a, b| format!("{}, {}", number(a), number(b));
            match clip {
                Clip::Position => pair(g.x, g.y),
                Clip::Size => pair(g.w, g.h),
                Clip::Style => cell.style.to_string(),
            }
        };
        let provider = gtk::gdk::ContentProvider::new_union(&[
            gtk::gdk::ContentProvider::for_value(&text.to_value()),
            gtk::gdk::ContentProvider::for_bytes(
                clip.mime(),
                &glib::Bytes::from_static(clip.noun().as_bytes()),
            ),
        ]);
        if let Err(e) = self.view.clipboard().set_content(Some(&provider)) {
            tracing::warn!("the clipboard would not take the {}: {e}", clip.noun());
            return;
        }
        // A style string can run to a line of its own; a pair is short enough to read back.
        match clip {
            Clip::Style => self.toast("Copied style"),
            _ => self.toast(&format!("Copied {} {text}", clip.noun())),
        }
    }

    /// The clipboard's text onto the selection: a position or a size onto the one shape, a
    /// style's look onto every cell selected, as one step (`Editor::paste_style`).
    pub(in crate::diagram) fn paste_property(self: &Rc<Self>, clip: Clip) {
        let clipboard = self.view.clipboard();
        let tab = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let text = clipboard.read_text_future().await.ok().flatten();
            let Some(tab) = tab.upgrade() else { return };
            let text = text.as_deref().map(str::trim).unwrap_or_default();
            let ids = tab.selection();
            let geometry = || {
                let editor = tab.editor.borrow();
                let page = editor.page(tab.page_index.get()).ok()?;
                let [id] = ids.as_slice() else { return None };
                Some(page.cell(id)?.geometry.as_ref()?.rect())
            };
            match (clip, pair(text), geometry()) {
                (Clip::Position, Some((x, y)), Some(g)) => {
                    tab.set_geometry(&ids[0], accent_drawio::Rect::new(x, y, g.w, g.h))
                }
                (Clip::Size, Some((w, h)), Some(g)) if w >= 0.0 && h >= 0.0 => {
                    tab.set_geometry(&ids[0], accent_drawio::Rect::new(g.x, g.y, w, h))
                }
                (Clip::Style, ..) if text.contains('=') && !text.contains('\n') => {
                    let style = accent_drawio::Style::parse(text);
                    tab.edit(|e, page| e.paste_style(page, &ids, &style));
                }
                (clip, ..) => tab.toast(&format!("No {} on the clipboard", clip.noun())),
            }
        });
    }
}

/// Two numbers as Copy Position and Copy Size write them, `x, y`, or with spaces alone between
/// them.
fn pair(text: &str) -> Option<(f64, f64)> {
    let mut numbers = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<f64>().ok().filter(|n| n.is_finite()));
    let (a, b) = (numbers.next()??, numbers.next()??);
    numbers.next().is_none().then_some((a, b))
}

#[cfg(test)]
mod tests {
    use super::pair;

    #[test]
    fn a_copied_position_reads_back() {
        assert_eq!(pair("100, 20.5"), Some((100.0, 20.5)));
        assert_eq!(pair(" -3 4 "), Some((-3.0, 4.0)));
        assert_eq!(pair("1, 2, 3"), None);
        assert_eq!(pair("rounded=1;"), None);
    }
}
