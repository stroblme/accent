//! The Layers group on a template's page, and the page's Name, by hand and by XTEST.

use super::*;

/// A page of two layers, as a slide template has them: a locked one with no name holding the
/// template's shape, and "Content" over it with a shape the edge `e` joins to the template's.
const LAYERED: &str = r#"<mxfile host="accent">
  <diagram name="Slide" id="bench-layers">
    <mxGraphModel grid="1" gridSize="10" page="1" pageWidth="800" pageHeight="500">
      <root>
        <mxCell id="0" />
        <mxCell id="1" style="locked=1;" parent="0" />
        <mxCell id="bg" value="Template" style="rounded=0;whiteSpace=wrap;html=1;" parent="1" vertex="1">
          <mxGeometry x="40" y="40" width="200" height="100" as="geometry" />
        </mxCell>
        <mxCell id="2" value="Content" parent="0" />
        <mxCell id="c" value="C" style="ellipse;whiteSpace=wrap;html=1;" parent="2" vertex="1">
          <mxGeometry x="400" y="200" width="120" height="80" as="geometry" />
        </mxCell>
        <mxCell id="e" style="endArrow=classic;html=1;" parent="2" source="bg" target="c" edge="1">
          <mxGeometry relative="1" as="geometry" />
        </mxCell>
      </root>
    </mxGraphModel>
  </diagram>
</mxfile>
"#;

/// Count visible AdwEntryRow pencils; expect none because clicking a row already starts editing.
fn pencils(pane: &gtk::Widget) -> String {
    let (mut rows, mut shown) = (0, 0);
    let mut todo = vec![pane.clone()];
    while let Some(w) = todo.pop() {
        rows += usize::from(w.is::<adw::EntryRow>());
        let pencil = w.has_css_class("edit-icon") && w.is_visible() && w.is_child_visible();
        shown += usize::from(pencil);
        let mut child = w.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            todo.push(c);
        }
    }
    format!("{shown}/{rows}")
}

/// Probe template Layers: locked-cell selection, hiding content, adding/selecting
/// insertion layers, pasting, reordering, renaming and deletion with dependent edges
/// as one undo step. Save locks/visibility and require no entry pencils. XTEST clicks,
/// double-click rename and undo exercise real input. Create the template if absent.
pub(super) fn layers(app: &Rc<App>, rel: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, LAYERED).expect("write the layered diagram");
    }
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        app.show_pane("properties");
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        let find = |root: &gtk::Widget, tooltip: &str| {
            let found = find_widget(root, &|w| {
                w.is::<gtk::Button>() && w.tooltip_text().as_deref() == Some(tooltip)
            });
            found.and_downcast::<gtk::Button>()
        };
        let press = |id: &str, tooltip: &str| {
            let row = tab.layer_row(id);
            match row.and_then(|r| find(r.upcast_ref(), tooltip)) {
                Some(button) => button.emit_clicked(),
                None => println!("bench diagram layers no_button {id} {tooltip}"),
            }
        };
        let rows = || tab.layer_rows();
        let parent = |id: &str| {
            let file = tab.file();
            file.pages[0].cell(id).and_then(|c| c.parent.clone())
        };
        let cells = || {
            let file = tab.file();
            let ids = file.pages[0].cells.iter().map(|c| c.id.clone());
            ids.collect::<Vec<_>>()
        };
        println!(
            "bench diagram layers open rows={:?} pick_bg={:?} pencils={}",
            rows(),
            tab.pick("bg"),
            pencils(&tab.properties())
        );
        // Every revealer in a row, its Move and Delete buttons' and libadwaita's own, before and
        // while the pointer is on it (PRELIGHT by hand, Xvfb having no pointer).
        if let Some(row) = tab.layer_row("2") {
            let revealers = || {
                let mut out = Vec::new();
                let mut todo = vec![row.clone().upcast::<gtk::Widget>()];
                while let Some(w) = todo.pop() {
                    if let Some(r) = w.downcast_ref::<gtk::Revealer>() {
                        out.push((r.has_css_class("accent-hover-actions"), r.reveals_child()));
                    }
                    let mut child = w.first_child();
                    while let Some(c) = child {
                        child = c.next_sibling();
                        todo.push(c);
                    }
                }
                out
            };
            let away = revealers();
            row.set_state_flags(gtk::StateFlags::PRELIGHT, false);
            println!(
                "bench diagram layers hover away={away:?} out={:?}",
                revealers()
            );
            row.unset_state_flags(gtk::StateFlags::PRELIGHT);
        }
        press("1", "Unlock Layer");
        println!(
            "bench diagram layers unlocked rows={:?} pick_bg={:?}",
            rows(),
            tab.pick("bg")
        );
        tab.select(vec!["bg".to_string()]);
        press("1", "Lock Layer");
        println!(
            "bench diagram layers relocked selection={:?}",
            tab.selection()
        );
        // Whether the ring's Add Rectangle can be pressed, and the tool in hand.
        let rect_tool = || {
            let button = find_widget(app.window.upcast_ref(), &|w| {
                w.is::<gtk::ToggleButton>() && w.tooltip_text().as_deref() == Some("Add Rectangle")
            });
            (button.is_some_and(|b| b.is_sensitive()), tab.tool())
        };
        let _ = WidgetExt::activate_action(&app.window, "win.diagram-rect", None);
        tab.select(vec!["c".to_string()]);
        let before = rect_tool();
        press("2", "Hide Layer");
        println!(
            "bench diagram layers hidden rows={:?} selection={:?} pick_c={:?} e={:?} \
             rect_tool={before:?}->{:?}",
            rows(),
            tab.selection(),
            tab.pick("c"),
            tab.frame_of("e"),
            rect_tool()
        );
        press("2", "Show Layer");
        println!("bench diagram layers shown rect_tool={:?}", rect_tool());

        match find(&tab.properties(), "Add Layer") {
            Some(add) => add.emit_clicked(),
            None => println!("bench diagram layers no_button Add Layer"),
        }
        let added = tab.file().pages[0].layers().last().map(|l| l.id.clone());
        let added = added.unwrap_or_default();
        tab.paste_text("Pasted", None);
        let pasted = tab.selection();
        println!(
            "bench diagram layers added rows={:?} paste_parent={:?}",
            rows(),
            pasted.first().and_then(|id| parent(id))
        );
        if let Some(row) = tab.layer_row("2") {
            row.emit_by_name::<()>("entry-activated", &[]);
        }
        tab.paste_text("Again", None);
        let pasted = tab.selection();
        println!(
            "bench diagram layers picked rows={:?} paste_parent={:?}",
            rows(),
            pasted.first().and_then(|id| parent(id))
        );
        press(&added, "Move Layer Down");
        if let Some(row) = tab.layer_row("2") {
            row.set_text("Slide");
            row.emit_by_name::<()>("entry-activated", &[]);
        }
        println!("bench diagram layers moved_renamed rows={:?}", rows());
        press("1", "Delete Layer");
        println!(
            "bench diagram layers deleted rows={:?} cells={:?}",
            rows(),
            cells()
        );
        tab.undo();
        println!(
            "bench diagram layers undo rows={:?} cells={:?}",
            rows(),
            cells()
        );
        tab.redo();
        press("2", "Lock Layer");
        press(&added, "Hide Layer");
        let flushed = app.flush_model(&tab);
        let disk = std::fs::read(tab.path()).expect("the file");
        let back = accent_drawio::File::from_bytes(&disk).expect("it reads back");
        let written: Vec<String> = back.pages[0]
            .layers()
            .iter()
            .map(|l| {
                format!(
                    "{}:{:?} style={} attrs={:?}",
                    l.id,
                    l.label(),
                    l.style,
                    l.attrs
                )
            })
            .collect();
        println!(
            "bench diagram layers flush {:?} written={written:?}",
            flushed.map_err(|e| e.to_string())
        );
        // The pointer's half, through XTEST: a click on the layer that is not current makes it
        // current and leaves its name alone, and a double click edits the name. A name typed
        // and entered renames the layer, one step Undo takes back; Escape puts it back; a click
        // on another layer renames it too, as the edit ends.
        if let Some(row) = tab.layer_row(&added) {
            let scroller = row.ancestor(gtk::ScrolledWindow::static_type());
            if let Some(scroller) = scroller.and_downcast::<gtk::ScrolledWindow>()
                && let Some(at) = scroller
                    .child()
                    .and_then(|c| row.compute_point(&c, &graphene::Point::zero()))
            {
                scroller.vadjustment().set_value(f64::from(at.y()) - 100.0);
            }
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let (dx, dy) = app.window.surface_transform();
        // Just inside the name's left edge: the hover buttons slide in from the right.
        let on_name = |id: &str| {
            let row = tab.layer_row(id)?;
            let name = find_widget(row.upcast_ref(), &|w| w.has_css_class("editable-area"))?;
            let left = graphene::Point::new(8.0, name.height() as f32 / 2.0);
            let at = name.compute_point(&app.window, &left)?;
            let (x, y) = (f64::from(at.x()) + dx, f64::from(at.y()) + dy);
            Some(format!("{x} {y}"))
        };
        let (on_added, on_slide) = (on_name(&added).unwrap_or_default(), on_name("2"));
        let editing = || {
            let row = tab.layer_row(&added);
            row.is_some_and(|r| r.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN))
        };
        let double = format!("move {on_added}; down; up; sleep 0.1; down; up; sleep 0.3");
        // Long enough between steps that no click is part of a double click.
        let pause = || glib::timeout_future(Duration::from_millis(600));
        super::super::git::xtest(&format!("move {on_added}; focus; sleep 0.2; down; up")).await;
        println!(
            "bench diagram layers clicked rows={:?} editing={}",
            rows(),
            editing()
        );
        pause().await;
        super::super::git::xtest(&double).await;
        println!("bench diagram layers double_clicked editing={}", editing());
        super::super::git::xtest("key ctrl+a; type Notes; key Return").await;
        println!(
            "bench diagram layers entered rows={:?} editing={}",
            rows(),
            editing()
        );
        tab.undo();
        println!("bench diagram layers entered_undo rows={:?}", rows());
        pause().await;
        super::super::git::xtest(&format!("{double}; key ctrl+a; type Gone; key Escape")).await;
        println!(
            "bench diagram layers escaped rows={:?} editing={}",
            rows(),
            editing()
        );
        pause().await;
        let on_slide = on_slide.unwrap_or_default();
        let away = format!("{double}; key ctrl+a; type Away; sleep 0.2; move {on_slide}; down; up");
        super::super::git::xtest(&away).await;
        println!(
            "bench diagram layers left rows={:?} editing={}",
            rows(),
            editing()
        );
        tab.undo();
        println!("bench diagram layers left_undo rows={:?}", rows());
        // The page's Name has no apply button: Escape leaves what is typed, a click elsewhere
        // renames the page, as Return does.
        tab.select(Vec::new());
        let name = find_widget(&tab.properties(), &|w| {
            w.downcast_ref::<adw::EntryRow>()
                .is_some_and(|r| r.title() == "Name")
        });
        let at = name.as_ref().and_then(|name| {
            let scroller = name.ancestor(gtk::ScrolledWindow::static_type())?;
            let scroller = scroller.downcast_ref::<gtk::ScrolledWindow>()?;
            let top = name.compute_point(&scroller.child()?, &graphene::Point::zero())?;
            scroller.vadjustment().set_value(f64::from(top.y()) - 100.0);
            Some(name.clone())
        });
        glib::timeout_future(Duration::from_millis(300)).await;
        let on_page_name = at.and_then(|name| {
            let at = name.compute_point(&app.window, &graphene::Point::new(40.0, 20.0))?;
            Some(format!(
                "{} {}",
                f64::from(at.x()) + dx,
                f64::from(at.y()) + dy
            ))
        });
        let named = || {
            let applies = name.as_ref().and_then(|n| {
                find_widget(n, &|w| w.has_css_class("apply-button")).map(|b| b.is_mapped())
            });
            let editing = name
                .as_ref()
                .is_some_and(|n| n.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN));
            format!(
                "pages={:?} apply_button={applies:?} editing={editing}",
                tab.page_names()
            )
        };
        if let (Some(on_name), Some(on_layer)) = (on_page_name, on_name("2")) {
            let edit = format!("move {on_name}; down; up; sleep 0.3; key ctrl+a; type Renamed");
            super::super::git::xtest(&format!("{edit}; sleep 0.2")).await;
            println!("bench diagram page_name typed {}", named());
            super::super::git::xtest("key Escape; sleep 0.2").await;
            println!("bench diagram page_name escaped {}", named());
            super::super::git::xtest(&format!("move {on_layer}; down; up; sleep 0.3")).await;
            println!("bench diagram page_name left {}", named());
            tab.undo();
            println!("bench diagram page_name left_undo {}", named());
            pause().await;
            let edit = format!("move {on_name}; down; up; sleep 0.3; key ctrl+a; type Entered");
            super::super::git::xtest(&format!("{edit}; key Return; sleep 0.3")).await;
            println!("bench diagram page_name entered {}", named());
        }
        // A picture of the group, for looking at rather than asserting on.
        if let Ok(path) = std::env::var("ACCENT_BENCH_DIAGRAM_SHOT") {
            tab.undo();
            glib::timeout_future(Duration::from_millis(400)).await;
            println!(
                "bench diagram shot {}",
                shoot(app.window.upcast_ref(), Path::new(&path))
            );
        }
        bench_quit(&app);
    });
}
