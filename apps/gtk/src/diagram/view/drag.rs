//! What a press on the canvas takes hold of, and what the drag it starts asks of the diagram
//! when it ends.

use accent_drawio::geom::rotate;
use accent_drawio::guide::{self, Neighbour};
use accent_drawio::handle::{self, Kind, Knob, Terminal};
use accent_drawio::{CellId, Point, Rect};
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::{DiagramView, Edit};
use crate::diagram::geometry::{self, DEFAULT_SIZE, HANDLE, Handle, Sheet, TOLERANCE};
use crate::diagram::tools::Tool;

/// What a press on the one selected cell's handles takes hold of.
#[derive(Debug, Clone, Copy)]
enum Grip {
    Rotate,
    Resize(Handle),
    /// An edge's source end, or with `false` its target end.
    End(bool),
    /// A handle between an edge's ends.
    Knob(Knob),
    /// An edge's label, by its handle or its text.
    Label,
}

/// A drag under way, in page units.
#[derive(Debug, Clone)]
pub(super) enum Drag {
    /// Moving `ids`, whose boxes take up `bounds`, aligning them to the boxes in `guides`; a
    /// release that did not move selects `click` instead, when there is one (a click into a
    /// selected group).
    Move {
        from: Point,
        ids: Vec<CellId>,
        click: Option<CellId>,
        bounds: Rect,
        guides: Vec<Rect>,
    },
    Resize {
        from: Point,
        id: CellId,
        handle: Handle,
        rect: Rect,
        /// The shape's turn, in degrees: its handles are in its own frame.
        rotation: f64,
        /// The shapes its size and sides snap to.
        guides: Vec<Neighbour>,
    },
    /// Dragging the source end (else the target end) of edge `id`, whose other end is at
    /// `other`.
    End {
        id: CellId,
        source: bool,
        other: Point,
    },
    /// Dragging a handle between the ends of edge `id`, whose route, ends and waypoints were
    /// `route`, `ends` and `waypoints` when it was pressed.
    Knob {
        id: CellId,
        knob: Knob,
        route: Vec<Point>,
        ends: [Option<Terminal>; 2],
        waypoints: Vec<Point>,
    },
    /// Dragging the label of edge `id`, routed along `route` between `ends`.
    Label {
        id: CellId,
        route: Vec<Point>,
        ends: [Option<Terminal>; 2],
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
        if imp.panning.get() || imp.read_only.get() {
            return Some(Drag::Pan {
                scroll: self.scroll(),
            });
        }
        let selection = self.selection();
        let tolerance = TOLERANCE / frame.scale;
        match imp.tool.get() {
            Tool::Select => {
                if let Some((id, grip)) = self.grip_under(&sheet, &selection, p) {
                    let rotation = sheet.rotation(&id);
                    let rect = sheet.rect(&id).unwrap_or_default();
                    return Some(match grip {
                        Grip::End(source) => {
                            let route = sheet.scene.route(&id).unwrap_or_default();
                            let other = match source {
                                true => route.last(),
                                false => route.first(),
                            };
                            Drag::End {
                                id,
                                source,
                                other: other.copied().unwrap_or(p),
                            }
                        }
                        Grip::Knob(knob) => Drag::Knob {
                            route: sheet.scene.route(&id).unwrap_or_default().to_vec(),
                            ends: sheet.terminals(&id),
                            waypoints: sheet.waypoints(&id),
                            id,
                            knob,
                        },
                        Grip::Label => Drag::Label {
                            route: sheet.scene.route(&id).unwrap_or_default().to_vec(),
                            ends: sheet.terminals(&id),
                            id,
                        },
                        Grip::Rotate => Drag::Rotate { id, rect },
                        Grip::Resize(handle) => Drag::Resize {
                            from: p,
                            handle,
                            rect,
                            rotation,
                            guides: match sheet.guides {
                                true => sheet.size_guides(&id, &self.guide_area()),
                                false => Vec::new(),
                            },
                            id,
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
                // The cell under the pointer, whose parent and edges pick the guides.
                let pressed = pick.held.clone().unwrap_or(pick.cell.clone());
                let (ids, click) = match &pick.held {
                    Some(held) => (selection, (pick.cell != *held).then_some(pick.cell)),
                    None => {
                        self.emit(Edit::Select(vec![pick.cell.clone()]));
                        (vec![pick.cell], None)
                    }
                };
                let ids: Vec<CellId> = ids.into_iter().filter(|id| !sheet.is_pinned(id)).collect();
                let bounds = ids
                    .iter()
                    .filter_map(|id| sheet.guide_box(id))
                    .reduce(|a, b| a.union(&b))
                    .unwrap_or(Rect::new(p.x, p.y, 0.0, 0.0));
                let guides = sheet.guide_boxes(&ids, &pressed);
                Some(Drag::Move {
                    from: p,
                    ids,
                    click,
                    bounds,
                    guides,
                })
            }
            tool if tool.draws_box() => Some(Drag::Draw { tool, from: p }),
            // Which shapes the ends attach to is decided on release, once both are known.
            Tool::Connector => Some(Drag::Connect { from: p }),
            _ => None,
        }
    }

    /// The selected cell whose handle is under page point `p`, and the handle: every selected
    /// edge's, as draw.io gives each one a handler, and a shape's when it is selected alone.
    fn grip_under(&self, sheet: &Sheet, selection: &[CellId], p: Point) -> Option<(CellId, Grip)> {
        selection
            .iter()
            .filter(|id| selection.len() == 1 || sheet.is_edge(id))
            .find_map(|id| Some((id.clone(), self.grip_at(sheet, id, p)?)))
    }

    /// The grip of selected cell `id` under page point `p`: an edge's ends, the handles between
    /// them (a faded one last), and its label's handle or text; a shape's rotate handle first,
    /// then a resize handle, found in the shape's own frame.
    fn grip_at(&self, sheet: &Sheet, id: &str, p: Point) -> Option<Grip> {
        let frame = self.imp().frame.get();
        if sheet.is_edge(id) && !sheet.is_pinned(id) {
            let route = sheet.scene.route(id)?;
            // The nearest handle within reach, an end before a handle between the ends and a
            // faded one last where two are as near; then the label's text.
            let ends = [(route.first(), true), (route.last(), false)];
            let ends = ends
                .into_iter()
                .filter_map(|(q, source)| Some((Grip::End(source), *q?, 0)));
            let knobs = sheet.knobs(id, route).into_iter();
            let knobs = knobs.map(|(k, at, dim)| (Grip::Knob(k), at, 1 + usize::from(dim)));
            let label = self.label_handle(sheet, id, route);
            let handles = ends
                .chain(knobs)
                .chain(label.map(|at| (Grip::Label, at, 3)));
            let away = |q: Point| frame.to_content(q).distance(frame.to_content(p));
            let nearest = handles
                .map(|(grip, at, rank)| (away(at), rank, grip))
                .filter(|h| h.0 <= HANDLE)
                .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
            if let Some((.., grip)) = nearest {
                return Some(grip);
            }
            let cache = &self.imp().cache;
            let on_text = cache.label_at(&sheet.scene.prims, p) == Some(id);
            return (label.is_some() && on_text).then_some(Grip::Label);
        }
        let r = sheet.rect(id).filter(|_| !sheet.is_pinned(id))?;
        let local = frame.to_content(rotate(p, r.centre(), -sheet.rotation(id)));
        let b = frame.rect(&r);
        if sheet.is_turnable(id) && geometry::rotate_handle(&b).distance(local) <= HANDLE {
            return Some(Grip::Rotate);
        }
        geometry::handle_at(&b, local, HANDLE).map(Grip::Resize)
    }

    /// What an edge end dragged to `p`, its other end at `other`, lands on: as a connector's end
    /// does ([`Sheet::end_at`]), dangling on the grid unless `free`.
    pub(super) fn end_to(&self, p: Point, other: Point, free: bool) -> geometry::End {
        let Some(sheet) = self.sheet() else {
            return (None, p, None);
        };
        let scale = self.scale();
        let mut end = sheet.end_at(p, other, TOLERANCE / scale, HANDLE / scale);
        if let Some(g) = self.grid(free).filter(|_| end.0.is_none()) {
            end.1 = Point::new(geometry::snap(p.x, g), geometry::snap(p.y, g));
        }
        end
    }

    /// The turn a rotate handle dragged to `pointer` gives the shape at `rect` (page units).
    pub(super) fn turn_to(&self, rect: &Rect, pointer: Point, free: bool) -> f64 {
        let frame = self.imp().frame.get();
        let r = frame.rect(rect);
        let handle = geometry::rotate_handle(&r);
        geometry::rotation_to(r.centre(), handle, frame.to_content(pointer), free)
    }

    /// A move of the selection from `from` to `to`, and the guides that show where it lands:
    /// aligned to `guides` and the page and on the grid, unless `free` — Alt, which turns both
    /// off in draw.io (Graph.js 17465) — or the page has its guides off (`guides="0"`).
    pub(super) fn move_delta(
        &self,
        from: Point,
        to: Point,
        bounds: &Rect,
        guides: &[Rect],
        free: bool,
    ) -> (Point, Vec<guide::Line>) {
        let raw = Point::new(to.x - from.x, to.y - from.y);
        let Some(sheet) = self.sheet().filter(|_| !free) else {
            return (raw, Vec::new());
        };
        if !sheet.guides {
            let origin = Point::new(bounds.x, bounds.y);
            let snapped = sheet.grid.map(|g| geometry::snap_move(origin, raw, g));
            return (snapped.unwrap_or(raw), Vec::new());
        }
        let targets = guide::Targets {
            shapes: guides,
            page: sheet.page_rect,
        };
        let px = 1.0 / self.scale();
        guide::snap(bounds, raw, &targets, sheet.grid, px)
    }

    /// `rect`, turned `rotation`, resized by dragging `handle` by `delta`, and the lines that
    /// show the guides it snapped to: on the grid and the size guides unless `free` (Alt, as in
    /// draw.io, Graph.js 27881) or the page has its guides off.
    pub(super) fn resize_to(
        &self,
        rect: &Rect,
        rotation: f64,
        handle: Handle,
        delta: Point,
        guides: &[Neighbour],
        free: bool,
    ) -> (Rect, Vec<guide::Line>) {
        let grid = self.grid(free);
        let guided = !free && !guides.is_empty() && self.sheet().is_some_and(|s| s.guides);
        let px = 1.0 / self.scale();
        let mut snapped = None;
        let resized = geometry::resize_rotated(rect, rotation, handle, delta, grid, |r, bounds| {
            if guided {
                let sides = handle.sides();
                snapped = Some(guide::snap_resize(
                    r, bounds, sides, rotation, guides, grid, px,
                ));
            }
        });
        let lines = snapped.map_or(Vec::new(), |s| s.lines(&resized, rotation));
        (resized, lines)
    }

    /// Where guides may be seen, in page units: what is on screen and half as much again all
    /// round (`getSizeGuideStates`).
    fn guide_area(&self) -> Rect {
        let near = self.page_at(0.0, 0.0);
        let scale = self.scale();
        let (w, h) = (
            f64::from(self.width()) / scale,
            f64::from(self.height()) / scale,
        );
        Rect::new(near.x - w / 2.0, near.y - h / 2.0, 2.0 * w, 2.0 * h)
    }

    pub(super) fn grid(&self, free: bool) -> Option<f64> {
        self.sheet().filter(|_| !free).and_then(|s| s.grid)
    }

    /// What a move, a resize or a turn asks for with the pointer at `p`: the edit its release
    /// makes, and the one the live preview shows meanwhile. `None` for any other drag.
    pub(super) fn drag_edit(&self, drag: &Drag, p: Point, free: bool) -> Option<Edit> {
        match drag {
            Drag::Move {
                from,
                ids,
                bounds,
                guides,
                ..
            } if !ids.is_empty() => Some(Edit::Move {
                ids: ids.clone(),
                delta: self.move_delta(*from, p, bounds, guides, free).0,
            }),
            Drag::Resize {
                from,
                id,
                handle,
                rect,
                rotation,
                guides,
            } => {
                let delta = Point::new(p.x - from.x, p.y - from.y);
                Some(Edit::Resize {
                    id: id.clone(),
                    rect: self
                        .resize_to(rect, *rotation, *handle, delta, guides, free)
                        .0,
                })
            }
            Drag::Rotate { id, rect } => Some(Edit::Rotate {
                id: id.clone(),
                degrees: self.turn_to(rect, p, free),
            }),
            Drag::Knob {
                id,
                knob,
                route,
                ends,
                waypoints,
            } => {
                let at = self.aim(p, route, *ends, free);
                let points = match knob {
                    Knob::Segment { index, .. } => {
                        handle::segment_points(&handle::segments(route), *index, at, *ends)
                    }
                    _ => {
                        let scale = self.scale();
                        // The other handles a bend dropped on goes.
                        let sheet = self.sheet()?;
                        let others = [route.first(), route.last()].into_iter().flatten().copied();
                        let bends = sheet.knobs(id, route).into_iter().filter_map(|k| {
                            matches!(k.0, Knob::Bend(_))
                                .then_some(k.1)
                                .filter(|_| k.0 != *knob)
                        });
                        let handles: Vec<Point> = others.chain(bends).collect();
                        let reach = HANDLE / 2.0 / scale;
                        let tolerance = if free { 0.0 } else { TOLERANCE / scale };
                        handle::bend_points(
                            waypoints, route, *knob, at, &handles, *ends, reach, tolerance,
                        )
                    }
                };
                Some(Edit::Points {
                    id: id.clone(),
                    points,
                })
            }
            Drag::Label { id, route, ends } => Some(Edit::LabelAt {
                id: id.clone(),
                at: self.aim(p, route, *ends, free),
            }),
            Drag::End { id, source, other } => Some(Edit::End {
                id: id.clone(),
                source: *source,
                end: self.end_to(p, *other, free),
            }),
            _ => None,
        }
    }

    /// Where the handle of edge `id`'s label sits on screen, routed along `route`: moved off
    /// any other handle of the edge it would cover, above or below it
    /// (`mxEdgeHandler.checkLabelHandle`, draw.io's `manageLabelHandle`).
    pub(super) fn label_handle(&self, sheet: &Sheet, id: &str, route: &[Point]) -> Option<Point> {
        let mut at = sheet.label_handle(id, route)?;
        let size = HANDLE / self.scale();
        let ends = [route.first(), route.last()].into_iter().flatten().copied();
        let knobs = sheet.knobs(id, route).into_iter().map(|k| k.1);
        for h in knobs.chain(ends) {
            if (h.x - at.x).abs() < size && (h.y - at.y).abs() < size {
                at.y = if h.y < at.y { h.y + size } else { h.y - size };
            }
        }
        Some(at)
    }

    /// The pointer as an edge's handle takes it: onto a shape's middle or the route's points
    /// within 2 px, else the grid, unless `free` (`handle::aim`).
    fn aim(&self, p: Point, route: &[Point], ends: [Option<Terminal>; 2], free: bool) -> Point {
        match free {
            true => p,
            false => handle::aim(p, route, ends, self.grid(free), 1.0 / self.scale()),
        }
    }

    /// The edit a drag's release makes of `edit`, what it asked for as the pointer went: a
    /// segment's waypoints become the corners of the route through them
    /// (`mxEdgeSegmentHandler.updatePreviewState`), or the route jumps on release.
    fn settled(&self, drag: &Drag, edit: Edit, p: Point, free: bool) -> Edit {
        let Drag::Knob {
            id,
            knob: Knob::Segment { .. },
            route,
            ends,
            ..
        } = drag
        else {
            return edit;
        };
        let Some(routed) = self.route_after(&edit, id) else {
            return edit;
        };
        let at = self.aim(p, route, *ends, free);
        let points = handle::merged_points(&routed, at, route, *ends, 1.0 / self.scale());
        Edit::Points {
            id: id.clone(),
            points,
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
            return self.emit(self.settled(&drag, edit, p, free));
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
        if imp.panning.get() || imp.read_only.get() || imp.drag.borrow().is_some() {
            return;
        }
        let Some(sheet) = self.sheet() else { return };
        let frame = imp.frame.get();
        let p = self.page_at(x, y);
        let name = match imp.tool.get() {
            Tool::Select => {
                let selection = imp.selection.borrow().clone();
                let grip = self.grip_under(&sheet, &selection, p);
                let elbow = grip.as_ref().and_then(|(id, _)| sheet.edge_kind(id));
                let grip = grip.map(|(id, g)| (g, sheet.rotation(&id)));
                match grip {
                    Some((Grip::End(_), _)) => Some("pointer"),
                    Some((Grip::Label, _)) => Some("move"),
                    Some((Grip::Knob(knob), _)) => Some(knob_cursor(knob, elbow)),
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

/// The pointer over a handle between an edge's ends, as draw.io's: a segment's and the elbow's
/// the way they move, a bend's a hand, a virtual bend's a cross.
fn knob_cursor(knob: Knob, kind: Option<Kind>) -> &'static str {
    let across = |vertical: bool| if vertical { "col-resize" } else { "row-resize" };
    match knob {
        Knob::Segment { vertical, .. } => across(vertical),
        Knob::Elbow => match kind {
            Some(Kind::Elbow { vertical: true }) => "row-resize",
            _ => "col-resize",
        },
        Knob::Bend(_) => "pointer",
        Knob::Virtual(_) => "crosshair",
    }
}
