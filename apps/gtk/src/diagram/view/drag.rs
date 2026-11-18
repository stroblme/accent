//! Canvas drag initiation and release edits.

use accent_drawio::geom::rotate;
use accent_drawio::guide::{self, Neighbour};
use accent_drawio::handle::{self, Kind, Knob, Terminal};
use accent_drawio::{CellId, Point, Rect};
use gtk::gdk;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::{DiagramView, Edit};
use crate::diagram::geometry::{self, DEFAULT_SIZE, HANDLE, Handle, Sheet, TOLERANCE};
use crate::diagram::tools::Tool;

/// The keys a drag reads on every event, as draw.io's handlers do: Alt places freely, Shift
/// constrains (a move to one axis, a line's end and a turn to steps of 15°), and the primary
/// modifier makes a move drop a copy (`isCloneEvent`).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct Mods {
    pub free: bool,
    pub constrained: bool,
    pub copy: bool,
}

impl Mods {
    pub(super) fn of(state: gdk::ModifierType) -> Mods {
        Mods {
            free: state.contains(gdk::ModifierType::ALT_MASK),
            constrained: state.contains(gdk::ModifierType::SHIFT_MASK),
            copy: state.contains(crate::platform::PRIMARY_MODIFIER),
        }
    }
}

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
    /// Moving `ids`, or with the primary modifier copying them, whose boxes take up `bounds`,
    /// aligning them to the boxes in `guides`; a release that did not move selects `click`
    /// instead, when there is one (a click into a selected group or on one of several cells
    /// selected, a Ctrl+click letting go).
    Move {
        from: Point,
        ids: Vec<CellId>,
        click: Option<Vec<CellId>>,
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
        /// It keeps its proportions (`aspect=fixed`).
        fixed: bool,
    },
    /// Dragging the source end (else the target end) of edge `id`, whose other end is at
    /// `other` and whose point next to the dragged end, a waypoint or the other end, is
    /// `anchor`.
    End {
        id: CellId,
        source: bool,
        other: Point,
        anchor: Point,
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
    /// Turning shape `id`, whose unturned rectangle is `rect`, by its rotate handle; for a line,
    /// `rect` is its frame.
    Rotate {
        id: CellId,
        rect: Rect,
    },
    /// A band from `from`, adding what it takes to the selection with `add`; a release that
    /// did not move selects `click` instead, when there is one (a click on a cell not selected,
    /// or on a selected group's empty middle).
    Band {
        from: Point,
        add: bool,
        click: Option<Vec<CellId>>,
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
    /// Where a press at widget `(x, y)` starts a drag, if it starts one. With `add` (Ctrl held)
    /// a click toggles the cell in the selection and a band adds to it.
    pub(super) fn start_drag(&self, x: f64, y: f64, add: bool) -> Option<Drag> {
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
                    let rect = sheet.frame_of(&id).unwrap_or_default();
                    return Some(match grip {
                        Grip::End(source) => {
                            let route = sheet.scene.route(&id).unwrap_or_default();
                            let (other, next) = match source {
                                true => (route.last(), sheet.waypoints(&id).first().copied()),
                                false => (route.first(), sheet.waypoints(&id).last().copied()),
                            };
                            let other = other.copied().unwrap_or(p);
                            Drag::End {
                                id,
                                source,
                                other,
                                anchor: next.unwrap_or(other),
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
                        // Turned from its ring, which sits beyond the frame's corner.
                        Grip::Rotate => Drag::Rotate {
                            rect: rect.grow(sheet.padding(&id)),
                            id,
                        },
                        Grip::Resize(handle) => Drag::Resize {
                            from: p,
                            handle,
                            rect,
                            rotation,
                            guides: match sheet.guides {
                                true => sheet.size_guides(&id, &self.guide_area()),
                                false => Vec::new(),
                            },
                            fixed: sheet.page.cell(&id).is_some_and(|c| {
                                c.style.resolve(false).get("aspect") == Some("fixed")
                            }),
                            id,
                        },
                    });
                }
                let Some(pick) = sheet.pick(p, tolerance, &selection) else {
                    // A click on a selected group's empty middle opens it, where draw.io's lets go
                    // of the selection (user decision 2026-11-15); a drag still draws a band.
                    let click = sheet.opened_at(p, &selection).filter(|_| !add);
                    if !add && click.is_none() {
                        self.emit(Edit::Select(Vec::new()));
                    }
                    return Some(Drag::Band {
                        from: p,
                        add,
                        click,
                    });
                };
                // Only what is selected moves, so a drag from a cell not selected draws a band,
                // while a click on it selects it, alone or with Ctrl added (user decision
                // 2026-11-18).
                let Some(pressed) = pick.held.clone() else {
                    let click = match add {
                        true => [selection, vec![pick.cell]].concat(),
                        false => vec![pick.cell],
                    };
                    let click = Some(click);
                    return Some(Drag::Band {
                        from: p,
                        add,
                        click,
                    });
                };
                // The selected cell under the pointer, whose parent and edges pick the guides, is
                // let go of by a click with Ctrl, and a click without it goes one level in; a
                // drag moves or copies the whole selection either way.
                let click = match add {
                    true => {
                        let rest = selection.iter().filter(|s| **s != pressed).cloned();
                        Some(rest.collect())
                    }
                    false => pick.clicked(&selection),
                };
                let ids: Vec<CellId> = selection
                    .into_iter()
                    .filter(|id| !sheet.is_pinned(id))
                    .collect();
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
        let alone = selection.len() == 1;
        selection
            .iter()
            .filter(|id| alone || sheet.is_edge(id))
            .find_map(|id| Some((id.clone(), self.grip_at(sheet, id, p, alone)?)))
    }

    /// The grip of selected cell `id`, selected `alone` or not, under page point `p`: a lone
    /// line's rotate handle first, then an edge's ends, the handles between them (a faded one
    /// last), and its label's handle or text; a shape's rotate handle first, then a resize
    /// handle, found in the shape's own frame.
    fn grip_at(&self, sheet: &Sheet, id: &str, p: Point, alone: bool) -> Option<Grip> {
        let frame = self.imp().frame.get();
        if sheet.is_edge(id) && !sheet.is_pinned(id) {
            let framed = |r: Rect| frame.rect(&r.grow(sheet.padding(id)));
            let ring = sheet
                .frame_of(id)
                .map(|r| geometry::rotate_handle(&framed(r)));
            let near = |at: Point| at.distance(frame.to_content(p)) <= HANDLE;
            if alone && sheet.is_turnable(id) && ring.is_some_and(near) {
                return Some(Grip::Rotate);
            }
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
        let r = sheet.rect(id).filter(|_| sheet.is_resizable(id))?;
        let local = frame.to_content(rotate(p, r.centre(), -sheet.rotation(id)));
        // The handles sit on the frame the overlay draws, just outside the shape.
        let b = frame.rect(&r.grow(sheet.padding(id)));
        if sheet.is_turnable(id) && geometry::rotate_handle(&b).distance(local) <= HANDLE {
            return Some(Grip::Rotate);
        }
        geometry::handle_at(&b, local, HANDLE).map(Grip::Resize)
    }

    /// What an edge end dragged to `p`, its other end at `other`, lands on: as a connector's end
    /// does ([`Sheet::end_at`]), dangling where [`DiagramView::dangle`] puts it from `anchor`.
    pub(super) fn end_to(
        &self,
        p: Point,
        other: Point,
        anchor: Point,
        mods: Mods,
    ) -> geometry::End {
        let Some(sheet) = self.sheet() else {
            return (None, p, None);
        };
        let scale = self.scale();
        let mut end = sheet.end_at(p, other, TOLERANCE / scale, HANDLE / scale);
        if end.0.is_none() {
            end.1 = self.dangle(p, anchor, mods);
        }
        end
    }

    /// Where a line's end on no shape goes with the pointer at `p`, the point before it at
    /// `anchor`: on the grid unless `free`, and with Shift at a step of 15° from `anchor`.
    fn dangle(&self, p: Point, anchor: Point, mods: Mods) -> Point {
        let grid = self.grid(mods.free);
        match mods.constrained {
            true => geometry::snap_angle(anchor, p, grid),
            false => geometry::snap_point(p, grid),
        }
    }

    /// The turn a rotate handle dragged to `pointer` gives the shape at `rect` (page units).
    pub(super) fn turn_to(&self, rect: &Rect, pointer: Point, mods: Mods) -> f64 {
        let frame = self.imp().frame.get();
        let r = frame.rect(rect);
        let handle = geometry::rotate_handle(&r);
        let pointer = frame.to_content(pointer);
        geometry::rotation_to(r.centre(), handle, pointer, mods.free, mods.constrained)
    }

    /// A move of the selection from `from` to `to`, and the guides that show where it lands:
    /// along the axis it goes furthest along with Shift, aligned to `guides` and the page and on
    /// the grid, unless `free` — Alt, which turns both off in draw.io (Graph.js 17465) — or the
    /// page has its guides off (`guides="0"`).
    pub(super) fn move_delta(
        &self,
        from: Point,
        to: Point,
        bounds: &Rect,
        guides: &[Rect],
        mods: Mods,
    ) -> (Point, Vec<guide::Line>) {
        let raw = Point::new(to.x - from.x, to.y - from.y);
        let axes = geometry::move_axes(raw, mods.constrained);
        let along = |v: f64, axis: bool| if axis { v } else { 0.0 };
        let only = |d: Point| Point::new(along(d.x, axes[0]), along(d.y, axes[1]));
        let raw = only(raw);
        let Some(sheet) = self.sheet().filter(|_| !mods.free) else {
            return (raw, Vec::new());
        };
        if !sheet.guides {
            let origin = Point::new(bounds.x, bounds.y);
            let snapped = self
                .grid(false)
                .map(|g| only(geometry::snap_move(origin, raw, g)));
            return (snapped.unwrap_or(raw), Vec::new());
        }
        let targets = guide::Targets {
            shapes: guides,
            page: sheet.page_rect,
        };
        let px = 1.0 / self.scale();
        guide::snap(bounds, raw, axes, &targets, self.grid(false), px)
    }

    /// `rect`, turned `rotation`, resized by dragging `handle` by `delta`, and the lines that
    /// show the guides it snapped to: on the grid and the size guides unless `free` (Alt, as in
    /// draw.io, Graph.js 27881) or the page has its guides off. A shape `fixed` to its
    /// proportions keeps them, and so snaps to no size guide.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn resize_to(
        &self,
        rect: &Rect,
        rotation: f64,
        handle: Handle,
        delta: Point,
        guides: &[Neighbour],
        free: bool,
        fixed: bool,
    ) -> (Rect, Vec<guide::Line>) {
        let grid = self.grid(free);
        let guided =
            !free && !fixed && !guides.is_empty() && self.sheet().is_some_and(|s| s.guides);
        let px = 1.0 / self.scale();
        let mut snapped = None;
        let resized = geometry::resize_rotated(rect, rotation, handle, delta, grid, |r, bounds| {
            if guided {
                let sides = handle.sides();
                snapped = Some(guide::snap_resize(
                    r, bounds, sides, rotation, guides, grid, px,
                ));
            }
            if fixed {
                *r = geometry::keep_proportions(*r, bounds, handle);
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

    /// The grid a drag snaps to at the present zoom, unless `free` or the page has it off.
    pub(super) fn grid(&self, free: bool) -> Option<f64> {
        let grid = self.sheet().filter(|_| !free).and_then(|s| s.grid)?;
        Some(geometry::grid_step(grid, self.scale()))
    }

    /// What a move, a resize, a turn or a drawing asks for with the pointer at `p` and `mods`
    /// held: the edit its release makes, and the one the live preview shows meanwhile. `None`
    /// for any other drag.
    pub(super) fn drag_edit(&self, drag: &Drag, p: Point, mods: Mods) -> Option<Edit> {
        let free = mods.free;
        match drag {
            Drag::Draw { tool, from } => {
                let snap = |q: Point| geometry::snap_point(q, self.grid(free));
                let rect = Rect::from_corners(snap(*from), snap(p));
                (rect.w >= 1.0 && rect.h >= 1.0).then_some(Edit::Add { tool: *tool, rect })
            }
            // Each end on no shape dangles as an end dragged does, the second from the first.
            Drag::Connect { from } => {
                let (sheet, scale) = (self.sheet()?, self.scale());
                let (mut source, mut target) =
                    sheet.connect_ends(*from, p, TOLERANCE / scale, HANDLE / scale);
                if source.0.is_none() {
                    source.1 = geometry::snap_point(source.1, self.grid(free));
                }
                if target.0.is_none() {
                    target.1 = self.dangle(p, source.1, mods);
                }
                // Both ends snapped onto one point make no line.
                let line = source.0.is_some() || target.0.is_some() || source.1 != target.1;
                line.then_some(Edit::Connect { source, target })
            }
            Drag::Move {
                from,
                ids,
                bounds,
                guides,
                ..
            } if !ids.is_empty() => {
                let (ids, delta) = (
                    ids.clone(),
                    self.move_delta(*from, p, bounds, guides, mods).0,
                );
                Some(match mods.copy {
                    true => Edit::Copy { ids, delta },
                    false => Edit::Move { ids, delta },
                })
            }
            Drag::Resize {
                from,
                id,
                handle,
                rect,
                rotation,
                guides,
                fixed,
            } => {
                let delta = Point::new(p.x - from.x, p.y - from.y);
                let (rect, _) =
                    self.resize_to(rect, *rotation, *handle, delta, guides, free, *fixed);
                Some(Edit::Resize {
                    id: id.clone(),
                    rect,
                    alone: free,
                })
            }
            Drag::Rotate { id, rect } => {
                let (id, degrees) = (id.clone(), self.turn_to(rect, p, mods));
                Some(match self.sheet()?.is_edge(&id) {
                    true => Edit::Turn {
                        id,
                        centre: rect.centre(),
                        degrees,
                    },
                    false => Edit::Rotate { id, degrees },
                })
            }
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
            Drag::End {
                id,
                source,
                other,
                anchor,
            } => Some(Edit::End {
                id: id.clone(),
                source: *source,
                end: self.end_to(p, *other, *anchor, mods),
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

    /// Finalize the preview edit by converting segment waypoints to routed corners
    /// (`mxEdgeSegmentHandler.updatePreviewState`), preventing a route jump on release.
    fn settled(&self, drag: &Drag, edit: Edit, p: Point, mods: Mods) -> Edit {
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
        let at = self.aim(p, route, *ends, mods.free);
        let points = handle::merged_points(&routed, at, route, *ends, 1.0 / self.scale());
        Edit::Points {
            id: id.clone(),
            points,
        }
    }

    /// What a drag that ends at widget `(x, y)` asks for.
    pub(super) fn end_drag(&self, drag: Drag, x: f64, y: f64, moved: bool, mods: Mods) {
        let p = self.page_at(x, y);
        let Some(sheet) = self.sheet() else { return };
        if let Some(edit) = self.drag_edit(&drag, p, mods).filter(|_| moved) {
            return self.emit(self.settled(&drag, edit, p, mods));
        }
        match drag {
            Drag::Move {
                click: Some(ids), ..
            } if !moved => self.emit(Edit::Select(ids)),
            // A click on a presented page follows a link, as one on a PDF read does.
            Drag::Pan { .. } if !moved && !self.imp().panning.get() => {
                if let Some(link) = sheet.link_at(p, TOLERANCE / self.scale()) {
                    self.emit(Edit::Follow(link.1));
                }
            }
            Drag::Band {
                click: Some(ids), ..
            } if !moved => self.emit(Edit::Select(ids)),
            Drag::Band { from, add, .. } if moved => {
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
            // A click draws a shape of draw.io's size around it.
            Drag::Draw { tool, from } if !moved => {
                let c = geometry::snap_point(from, self.grid(mods.free));
                let (w, h) = DEFAULT_SIZE;
                let rect = Rect::new(c.x - w / 2.0, c.y - h / 2.0, w, h);
                self.emit(Edit::Add { tool, rect });
            }
            // A click with the connector selects the cell under it, and over empty page lets go
            // of the selection, the arrow just drawn too.
            Drag::Connect { .. } if !moved => {
                self.emit(Edit::Select(self.cell_at(x, y).into_iter().collect()));
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
                    // Only over what is selected, which alone a drag moves.
                    None => sheet
                        .pick(p, TOLERANCE / frame.scale, &selection)
                        .filter(|pick| pick.held.is_some())
                        .map(|_| "move"),
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
