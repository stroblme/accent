//! What a press on the canvas takes hold of, and what the drag it starts asks of the diagram
//! when it ends.

use accent_drawio::geom::rotate;
use accent_drawio::{CellId, Point, Rect};
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::{DiagramView, Edit};
use crate::diagram::geometry::{self, DEFAULT_SIZE, HANDLE, Handle, Sheet, TOLERANCE};
use crate::diagram::tools::Tool;

/// What a press on the one selected shape's frame takes hold of.
#[derive(Debug, Clone, Copy)]
enum Grip {
    Rotate,
    Resize(Handle),
}

/// A drag under way, in page units.
#[derive(Debug, Clone)]
pub(super) enum Drag {
    /// Moving `ids`, whose frames start at `origin`; a release that did not move selects
    /// `click` instead, when there is one (a click into a selected group).
    Move {
        from: Point,
        ids: Vec<CellId>,
        click: Option<CellId>,
        origin: Point,
    },
    Resize {
        from: Point,
        id: CellId,
        handle: Handle,
        rect: Rect,
        /// The shape's turn, in degrees: its handles are in its own frame.
        rotation: f64,
    },
    /// Turning shape `id`, whose unturned rectangle is `rect`, by its rotate handle.
    Rotate {
        id: CellId,
        rect: Rect,
    },
    Band {
        from: Point,
        add: bool,
    },
    Draw {
        tool: Tool,
        from: Point,
    },
    Connect {
        from: Point,
    },
    /// Dragging the page itself, from this scroll position.
    Pan {
        scroll: (f64, f64),
    },
}

impl DiagramView {
    /// Where a press at widget `(x, y)` starts a drag, if it starts one.
    pub(super) fn start_drag(&self, x: f64, y: f64, shift: bool) -> Option<Drag> {
        let imp = self.imp();
        let sheet = self.sheet()?;
        let frame = imp.frame.get();
        let p = self.page_at(x, y);
        if imp.panning.get() {
            return Some(Drag::Pan {
                scroll: self.scroll(),
            });
        }
        let selection = self.selection();
        let tolerance = TOLERANCE / frame.scale;
        match imp.tool.get() {
            Tool::Select => {
                if let [id] = selection.as_slice()
                    && let Some(grip) = self.grip_at(&sheet, id, p)
                    && let Some(rect) = sheet.rect(id)
                {
                    let (id, rotation) = (id.clone(), sheet.rotation(id));
                    return Some(match grip {
                        Grip::Rotate => Drag::Rotate { id, rect },
                        Grip::Resize(handle) => Drag::Resize {
                            from: p,
                            id,
                            handle,
                            rect,
                            rotation,
                        },
                    });
                }
                let Some(pick) = sheet.pick(p, tolerance, &selection) else {
                    if !shift {
                        self.emit(Edit::Select(Vec::new()));
                    }
                    return Some(Drag::Band {
                        from: p,
                        add: shift,
                    });
                };
                if shift {
                    let toggled = pick.held.clone().unwrap_or(pick.cell.clone());
                    let mut next = selection.clone();
                    match next.iter().position(|s| *s == toggled) {
                        Some(i) => {
                            next.remove(i);
                        }
                        None => next.push(pick.cell),
                    }
                    self.emit(Edit::Select(next));
                    return None;
                }
                let (ids, click) = match &pick.held {
                    Some(held) => (selection, (pick.cell != *held).then_some(pick.cell)),
                    None => {
                        self.emit(Edit::Select(vec![pick.cell.clone()]));
                        (vec![pick.cell], None)
                    }
                };
                let ids: Vec<CellId> = ids.into_iter().filter(|id| !sheet.is_pinned(id)).collect();
                let origin = ids
                    .iter()
                    .filter_map(|id| sheet.frame_of(id))
                    .reduce(|a, b| a.union(&b))
                    .map_or(p, |r| Point::new(r.x, r.y));
                Some(Drag::Move {
                    from: p,
                    ids,
                    click,
                    origin,
                })
            }
            tool if tool.draws_box() => Some(Drag::Draw { tool, from: p }),
            // Which shapes the ends attach to is decided on release, once both are known.
            Tool::Connector => Some(Drag::Connect { from: p }),
            _ => None,
        }
    }

    /// The grip of the one selected shape `id` under page point `p`, found in the shape's own
    /// frame: its rotate handle first, then a resize handle.
    fn grip_at(&self, sheet: &Sheet, id: &str, p: Point) -> Option<Grip> {
        let r = sheet.rect(id).filter(|_| !sheet.is_pinned(id))?;
        let frame = self.imp().frame.get();
        let local = frame.to_content(rotate(p, r.centre(), -sheet.rotation(id)));
        let b = frame.rect(&r);
        if sheet.is_turnable(id) && geometry::rotate_handle(&b).distance(local) <= HANDLE {
            return Some(Grip::Rotate);
        }
        geometry::handle_at(&b, local, HANDLE).map(Grip::Resize)
    }

    /// The turn a rotate handle dragged to `pointer` gives the shape at `rect` (page units).
    pub(super) fn turn_to(&self, rect: &Rect, pointer: Point, free: bool) -> f64 {
        let frame = self.imp().frame.get();
        let r = frame.rect(rect);
        let handle = geometry::rotate_handle(&r);
        geometry::rotation_to(r.centre(), handle, frame.to_content(pointer), free)
    }

    /// A move of the selection from `from` to `to`, on the grid unless `free`.
    pub(super) fn move_delta(&self, from: Point, to: Point, origin: Point, free: bool) -> Point {
        let raw = Point::new(to.x - from.x, to.y - from.y);
        match self.grid(free) {
            Some(grid) => geometry::snap_move(origin, raw, grid),
            None => raw,
        }
    }

    pub(super) fn grid(&self, free: bool) -> Option<f64> {
        self.sheet().filter(|_| !free).and_then(|s| s.grid)
    }

    /// What a move, a resize or a turn asks for with the pointer at `p`: the edit its release
    /// makes, and the one the live preview shows meanwhile. `None` for any other drag.
    pub(super) fn drag_edit(&self, drag: &Drag, p: Point, free: bool) -> Option<Edit> {
        match drag {
            Drag::Move {
                from, ids, origin, ..
            } if !ids.is_empty() => Some(Edit::Move {
                ids: ids.clone(),
                delta: self.move_delta(*from, p, *origin, free),
            }),
            Drag::Resize {
                from,
                id,
                handle,
                rect,
                rotation,
            } => {
                let delta = Point::new(p.x - from.x, p.y - from.y);
                let grid = self.grid(free);
                Some(Edit::Resize {
                    id: id.clone(),
                    rect: geometry::resize_rotated(rect, *rotation, *handle, delta, grid),
                })
            }
            Drag::Rotate { id, rect } => Some(Edit::Rotate {
                id: id.clone(),
                degrees: self.turn_to(rect, p, free),
            }),
            _ => None,
        }
    }

    /// What a drag that ends at widget `(x, y)` asks for.
    pub(super) fn end_drag(&self, drag: Drag, x: f64, y: f64, moved: bool, free: bool) {
        let p = self.page_at(x, y);
        let Some(sheet) = self.sheet() else { return };
        let snap = |q: Point| match self.grid(free) {
            Some(g) => Point::new(geometry::snap(q.x, g), geometry::snap(q.y, g)),
            None => q,
        };
        if let Some(edit) = self.drag_edit(&drag, p, free).filter(|_| moved) {
            return self.emit(edit);
        }
        match drag {
            Drag::Move {
                click: Some(cell), ..
            } if !moved => self.emit(Edit::Select(vec![cell])),
            Drag::Band { from, add } if moved => {
                let mut ids = sheet.band(Rect::from_corners(from, p));
                if add {
                    let mut all = self.selection();
                    all.extend(
                        ids.into_iter()
                            .filter(|id| !all.contains(id))
                            .collect::<Vec<_>>(),
                    );
                    ids = all;
                }
                self.emit(Edit::Select(ids));
            }
            Drag::Draw { tool, from } => {
                let rect = match moved {
                    true => Rect::from_corners(snap(from), snap(p)),
                    false => {
                        let c = snap(from);
                        let (w, h) = DEFAULT_SIZE;
                        Rect::new(c.x - w / 2.0, c.y - h / 2.0, w, h)
                    }
                };
                if rect.w >= 1.0 && rect.h >= 1.0 {
                    self.emit(Edit::Add { tool, rect });
                }
            }
            Drag::Connect { from } if moved => {
                let scale = self.scale();
                let (source, target) =
                    sheet.connect_ends(from, p, TOLERANCE / scale, HANDLE / scale);
                self.emit(Edit::Connect { source, target });
            }
            _ => {}
        }
    }

    /// The pointer over the canvas: say with the cursor what a press there would do.
    pub(super) fn hover(&self, x: f64, y: f64) {
        let imp = self.imp();
        if imp.panning.get() || imp.drag.borrow().is_some() {
            return;
        }
        let Some(sheet) = self.sheet() else { return };
        let frame = imp.frame.get();
        let p = self.page_at(x, y);
        let name = match imp.tool.get() {
            Tool::Select => {
                let grip = match imp.selection.borrow().as_slice() {
                    [id] => self.grip_at(&sheet, id, p).map(|g| (g, sheet.rotation(id))),
                    _ => None,
                };
                match grip {
                    Some((Grip::Rotate, _)) => Some("grab"),
                    // A turned handle shows the cursor of the way it now points.
                    Some((Grip::Resize(h), rotation)) => Some(h.turned(rotation).cursor()),
                    None => sheet.scene.hit(p, TOLERANCE / frame.scale).map(|_| "move"),
                }
            }
            Tool::Image => None,
            _ => Some("crosshair"),
        };
        // Only when it changes: this runs on every motion.
        if self.cursor().and_then(|c| c.name()).as_deref() != name {
            self.set_cursor_from_name(name);
        }
        // The connector shows the connection points under the pointer before it is pressed.
        if imp.tool.get() == Tool::Connector {
            imp.pointer.set(p);
            self.queue_draw();
        }
    }
}
