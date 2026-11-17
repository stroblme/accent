//! The Properties pane on the sample: its rows, copy and paste, the canvas's menu, live
//! geometry and the style rows of shapes, arrows, pictures, flex arrows and swimlanes.

use super::*;

/// Probe Properties on the sample: position/size after nudging, burst edits as one
/// undo, hover-revealed buttons, Copy/Paste Position and Style, wrong-type paste toasts, the
/// line rows an arrow and a shape share, the rows of a flex arrow and a swimlane, a free line's
/// ends dragged and typed, Flip of an arrow and of a box placed by fractions, and Custom picked
/// and typed by XTEST, as saved.
/// Create the sample at `rel` if absent.
pub(super) fn props(app: &Rc<App>, rel: &str) {
    let path = app.root().join(rel);
    if !path.exists() {
        std::fs::write(&path, SAMPLE).expect("write the sample diagram");
    }
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        glib::timeout_future(Duration::from_millis(1000)).await;
        // Once the session restore has put Files back.
        app.show_pane("properties");
        let Some(tab) = app.active_diagram() else {
            println!("bench diagram no_tab");
            return bench_quit(&app);
        };
        let pane = tab.properties();
        let group = |title: &str| {
            find_widget(&pane, &|w| {
                w.downcast_ref::<adw::PreferencesGroup>()
                    .is_some_and(|g| g.title() == title)
            })
        };
        // In its group: the Page group has a Width and a Height of its own.
        let spin = |title: &str| {
            let within = group(if title.len() == 1 { "Position" } else { "Size" })?;
            find_widget(&within, &|w| {
                w.downcast_ref::<adw::SpinRow>()
                    .is_some_and(|r| r.title() == title)
            })
            .and_downcast::<adw::SpinRow>()
        };
        let rows = || ["X", "Y", "Width", "Height"].map(|t| spin(t).map(|r| r.text().to_string()));
        // The groups' box, whole however far the pane is scrolled, for the pictures below.
        let content = group("Shape").and_then(|g| g.parent());
        let click = |tooltip: &str| {
            let button = find_widget(&pane, &|w| {
                w.is::<gtk::Button>() && w.tooltip_text().as_deref() == Some(tooltip)
            });
            match button.and_downcast::<gtk::Button>() {
                Some(button) => button.emit_clicked(),
                None => println!("bench diagram props no_button {tooltip}"),
            }
        };
        let style = |id: &str| tab.file().pages[0].cell(id).map(|c| c.style.to_string());
        tab.select(vec!["a".to_string()]);
        println!("bench diagram props a rows={:?}", rows());
        tab.nudge(30.0, 0.0);
        println!("bench diagram props nudged rows={:?}", rows());
        if let (Some(x), Some(w)) = (spin("X"), spin("Width")) {
            x.set_value(250.0);
            w.set_value(200.0);
        }
        glib::timeout_future(Duration::from_millis(600)).await;
        println!(
            "bench diagram props burst frame={:?} rows={:?}",
            tab.frame_of("a"),
            rows()
        );
        tab.undo();
        println!("bench diagram props undo frame={:?}", tab.frame_of("a"));
        tab.redo();

        // PRELIGHT by hand, Xvfb having no pointer: the flag GTK puts on what it is over.
        let group = group("Position");
        let revealed = || {
            let group = group.as_ref()?;
            let revealer = find_widget(group, &|w| w.is::<gtk::Revealer>());
            Some(revealer.and_downcast::<gtk::Revealer>()?.reveals_child())
        };
        let away = revealed();
        if let Some(group) = &group {
            group.set_state_flags(gtk::StateFlags::PRELIGHT, false);
        }
        println!(
            "bench diagram props hover away={away:?} out={:?}",
            revealed()
        );
        // A picture of the pane with the buttons out, for looking at rather than asserting on.
        if let Ok(path) = std::env::var("ACCENT_BENCH_DIAGRAM_SHOT") {
            glib::timeout_future(Duration::from_millis(400)).await;
            println!(
                "bench diagram shot {}",
                shoot(app.window.upcast_ref(), Path::new(&path))
            );
        }
        if let Some(group) = &group {
            group.unset_state_flags(gtk::StateFlags::PRELIGHT);
        }

        click("Copy Position");
        let said = super::super::export::toast(&app, "Copied position").await;
        let clipboard = tab.key_target().clipboard().read_text_future().await;
        println!(
            "bench diagram props copy said={said:?} clipboard={:?}",
            clipboard.ok().flatten()
        );
        tab.select(vec!["b".to_string()]);
        click("Paste Position");
        glib::timeout_future(Duration::from_millis(300)).await;
        println!(
            "bench diagram props paste_position b={:?}",
            tab.frame_of("b")
        );

        tab.select(vec!["a".to_string()]);
        click("Copy Style");
        let said = super::super::export::toast(&app, "Copied style").await;
        tab.select(vec!["b".to_string()]);
        click("Paste Style");
        glib::timeout_future(Duration::from_millis(300)).await;
        println!(
            "bench diagram props paste_style said={said:?} a={:?} b={:?}",
            style("a"),
            style("b")
        );
        click("Paste Size");
        let said = super::super::export::toast(&app, "No size").await;
        println!(
            "bench diagram props paste_size said={said:?} b={:?} history={:?}",
            tab.frame_of("b"),
            tab.history()
        );

        // The canvas's menu, right-clicked by XTEST: the one paste it offers is for what the
        // clipboard holds, the shapes, a size or a style, none for a position, and over empty
        // page with a style held there is no menu.
        let canvas = tab.key_target();
        let screen = |r: accent_drawio::Rect| {
            let r = tab.to_widget(&r);
            on_screen(&app, &canvas, r.x + r.w / 2.0, r.y + r.h / 2.0)
        };
        // a's right end, which b, pasted to a's position above, leaves uncovered.
        let a = (tab.frame_of("a"))
            .map(|r| accent_drawio::Rect::new(r.x + r.w - 20.0, r.y, 0.0, r.h))
            .and_then(screen);
        let m = tab.frame_of("m").and_then(screen);
        let blank = screen(accent_drawio::Rect::new(750.0, 50.0, 0.0, 0.0));
        let run = |action: &str| {
            let _ = WidgetExt::activate_action(&app.window, action, None);
        };
        tab.select(vec!["a".to_string()]);
        tab.copy();
        println!(
            "bench diagram props menu shapes {:?}",
            menu_labels(&app, a.as_deref()).await
        );
        run("win.diagram-copy-size");
        let said = super::super::export::toast(&app, "Copied size").await;
        println!(
            "bench diagram props menu size said={said:?} {:?}",
            menu_labels(&app, m.as_deref()).await
        );
        run("win.diagram-paste-size");
        glib::timeout_future(Duration::from_millis(300)).await;
        println!(
            "bench diagram props menu paste_size selection={:?} a={:?} m={:?}",
            tab.selection(),
            tab.frame_of("a"),
            tab.frame_of("m")
        );
        tab.undo();
        tab.select(vec!["a".to_string()]);
        click("Copy Position");
        println!(
            "bench diagram props menu position {:?}",
            menu_labels(&app, a.as_deref()).await
        );
        run("win.diagram-copy-style");
        println!(
            "bench diagram props menu style {:?} blank={:?}",
            menu_labels(&app, a.as_deref()).await,
            menu_labels(&app, blank.as_deref()).await
        );

        // Show Grid is the Page group's switch, which follows the preference either way.
        let grid = || {
            let row = find_widget(&pane, &|w| {
                w.downcast_ref::<adw::PreferencesRow>()
                    .is_some_and(|r| r.title() == "Show Grid")
            })?;
            find_widget(&row, &|w| w.is::<gtk::Switch>()).and_downcast::<gtk::Switch>()
        };
        let grid_state = || {
            (
                grid().filter(|s| s.is_mapped()).map(|s| s.is_active()),
                app.config.borrow().diagram.grid,
            )
        };
        println!("bench diagram props grid off={:?}", grid_state());
        run("win.diagram-grid");
        println!("bench diagram props grid palette={:?}", grid_state());
        if let Ok(path) = std::env::var("ACCENT_BENCH_DIAGRAM_SHOT") {
            glib::timeout_future(Duration::from_millis(400)).await;
            let path = Path::new(&path).with_extension("grid.png");
            println!(
                "bench diagram props shot grid {}",
                shoot(app.window.upcast_ref(), &path)
            );
        }
        let switch = grid().and_then(|s| middle_on_screen(&app, &s));
        if let Some(at) = switch {
            super::super::git::xtest(&format!("move {at}; down; up; sleep 0.4")).await;
        }
        println!("bench diagram props grid clicked={:?}", grid_state());

        // Live geometry. A move and a resize by XTEST show in the rows while the button is still
        // down, the model the same until the drop, which is one step; X stepped shows on the
        // canvas at once and lands when the burst settles, and digits typed into it as they are
        // typed, landing with Return as one step.
        tab.select(vec!["m".to_string()]);
        let started = tab.frame_of("m");
        let point = |at: Option<String>, dx: f64, dy: f64| {
            let at = at?;
            let (x, y) = at.split_once(' ')?;
            let (x, y) = (x.parse::<f64>().ok()?, y.parse::<f64>().ok()?);
            Some(format!("{} {}", x + dx, y + dy))
        };
        for name in ["move", "resize"] {
            // The middle of m, then its right side's handle, where the move left it.
            let at = (tab.frame_of("m"))
                .map(|r| match name {
                    "move" => r,
                    _ => accent_drawio::Rect::new(r.x + r.w, r.y, 0.0, r.h),
                })
                .and_then(screen);
            let (Some(from), Some(on), Some(to)) = (
                point(at.clone(), 0.0, 0.0),
                point(at.clone(), 20.0, 10.0),
                point(at, 40.0, 20.0),
            ) else {
                continue;
            };
            super::super::git::xtest(&format!(
                "move {from}; down; move {on}; sleep 0.1; move {to}"
            ))
            .await;
            println!(
                "bench diagram props live {name} rows={:?} model={:?}",
                rows(),
                tab.frame_of("m")
            );
            super::super::git::xtest("up").await;
            println!(
                "bench diagram props live {name} dropped rows={:?} model={:?}",
                rows(),
                tab.frame_of("m")
            );
        }
        tab.undo();
        tab.undo();
        println!(
            "bench diagram props live undone={}",
            tab.frame_of("m") == started
        );
        if let Some(x) = spin("X") {
            x.set_value(x.value() + 30.0);
        }
        println!(
            "bench diagram props live stepped held={:?} model={:?}",
            tab.held(),
            tab.frame_of("m")
        );
        glib::timeout_future(Duration::from_millis(600)).await;
        println!(
            "bench diagram props live settled held={:?} model={:?}",
            tab.held(),
            tab.frame_of("m")
        );
        let settled = tab.frame_of("m");
        let text = spin("X").and_then(|x| {
            let text = find_widget(x.upcast_ref(), &|w| w.is::<gtk::Text>())?;
            middle_on_screen(&app, &text)
        });
        if let Some(at) = text {
            super::super::git::xtest(&format!("move {at}; focus; down; up; key ctrl+a; type 333"))
                .await;
            println!(
                "bench diagram props live typed held={:?} model={:?}",
                tab.held(),
                tab.frame_of("m")
            );
            if let Ok(path) = std::env::var("ACCENT_BENCH_DIAGRAM_SHOT") {
                let path = Path::new(&path).with_extension("typed.png");
                println!(
                    "bench diagram props shot typed {}",
                    shoot(app.window.upcast_ref(), &path)
                );
            }
            super::super::git::xtest("key Return").await;
            println!(
                "bench diagram props live entered held={:?} model={:?}",
                tab.held(),
                tab.frame_of("m")
            );
            tab.undo();
            println!(
                "bench diagram props live typed_undone={}",
                tab.frame_of("m") == settled
            );
        }
        tab.key_target().grab_focus();

        // An arrow's line takes the outline's rows, and the file keeps what they write. A spin
        // row's held change lands before a switch's right after it, and on the cells it was
        // made on when the selection changes before it settles.
        let looks = [
            "Fill",
            "Gradient",
            "Rounded",
            "Shadow",
            "Fill Opacity",
            "Rotation",
            "Flip",
            "Line Colour",
            "Line Opacity",
            "Line Width",
            "Pattern",
            "Waypoints",
            "Line Style",
            "Start",
            "Start Size",
            "End",
            "End Size",
            "Font",
            "Position",
            "Word Wrap",
            "Label Background",
            "Label Border",
            "Dash Pattern",
            "Perimeter",
            "Arc Size",
            "Fixed Corner Size",
            "Fixed Size",
            "Divider",
            "Sketch",
            "Vertical Text",
            "Text Opacity",
            "Spacing",
            "Start Spacing",
            "End Spacing",
            "Arrow Width",
            "Start Width",
            "End Width",
        ];
        let showing = || {
            looks
                .into_iter()
                .filter_map(|t| Some(format!("{}:{t}", shown(&pane, t)?)))
                .collect::<Vec<_>>()
        };
        let row = |title: &str| {
            find_widget(&pane, &|w| {
                w.downcast_ref::<adw::PreferencesRow>()
                    .is_some_and(|r| r.title() == title)
            })
        };
        tab.select(vec!["e".to_string()]);
        println!("bench diagram props edge shown={:?}", showing());
        if let Some(width) = row("Line Width").and_downcast::<adw::SpinRow>() {
            width.set_value(3.0);
        }
        let choose = |title: &str, label: &str| {
            let combo = row(title).and_downcast::<adw::ComboRow>();
            let list = combo
                .as_ref()
                .and_then(|c| c.model())
                .and_downcast::<gtk::StringList>();
            let at = list
                .and_then(|l| (0..l.n_items()).find(|&i| l.string(i).as_deref() == Some(label)));
            match (combo, at) {
                (Some(combo), Some(i)) => combo.set_selected(i),
                _ => println!("bench diagram props no_choice {title} {label}"),
            }
        };
        choose("Pattern", "Dashed");
        tab.select(vec!["a".to_string(), "e".to_string()]);
        println!("bench diagram props mixed shown={:?}", showing());
        tab.select(vec!["a".to_string()]);
        println!("bench diagram props vertex shown={:?}", showing());
        if let Some(width) = row("Line Width").and_downcast::<adw::SpinRow>() {
            width.set_value(2.5);
        }
        tab.select(vec!["e".to_string()]);
        // draw.io's pattern, heads and route menus; an arrow's rows leave a shape selected with
        // it alone.
        choose("Pattern", "Dotted (2)");
        choose("Start", "Circle");
        choose("End", "Diamond Outline");
        choose("Waypoints", "Vertical Elbow");
        println!("bench diagram props line elbow={:?}", style("e"));
        // draw.io's Curved waypoints are orthogonal and curved; its line style then takes the
        // curve back for rounded corners.
        choose("Waypoints", "Curved");
        println!("bench diagram props line curved={:?}", style("e"));
        choose("Line Style", "Rounded");
        if let Some(size) = row("End Size").and_downcast::<adw::SpinRow>() {
            size.set_value(10.0);
        }
        tab.select(vec!["a".to_string(), "e".to_string()]);
        if let Some(size) = row("Start Size").and_downcast::<adw::SpinRow>() {
            size.set_value(9.0);
        }
        tab.select(vec!["e".to_string()]);
        let picked = ["Pattern", "Start", "End", "Waypoints", "Line Style"].map(|t| {
            let item = row(t).and_downcast::<adw::ComboRow>()?.selected_item();
            Some(
                item.and_downcast::<gtk::StringObject>()?
                    .string()
                    .to_string(),
            )
        });
        println!("bench diagram props line picked={picked:?}");
        // draw.io's fonts, label positions and word wrap on a shape; a label's colours.
        let switch_of = |title: &str| {
            find_widget(&row(title)?, &|w| w.is::<gtk::Switch>()).and_downcast::<gtk::Switch>()
        };
        tab.select(vec!["a".to_string()]);
        choose("Font", "Georgia");
        choose("Position", "Top");
        if let Some(wrap) = row("Word Wrap").and_downcast::<adw::SwitchRow>() {
            wrap.set_active(false);
        }
        if let Some(border) = switch_of("Label Border") {
            border.set_active(true);
        }
        tab.select(vec!["e".to_string()]);
        if let Some(background) = switch_of("Label Background") {
            background.set_active(false);
        }
        println!(
            "bench diagram props text a={:?} e={:?}",
            style("a"),
            style("e")
        );

        // draw.io's Flip, and Constrain Proportions scaling the height with the width.
        tab.select(vec!["a".to_string()]);
        click("Flip Horizontally");
        if let Some(aspect) = row("Constrain Proportions").and_downcast::<adw::SwitchRow>() {
            aspect.set_active(true);
        }
        if let Some(width) = spin("Width") {
            width.set_value(300.0);
        }
        glib::timeout_future(Duration::from_millis(600)).await;
        println!(
            "bench diagram props arrange frame={:?} rows={:?} a={:?}",
            tab.frame_of("a"),
            rows(),
            style("a")
        );
        // A picture's fill and line are its background and border.
        let picture = "shape=image;image=data:image/png,x;";
        tab.edit(|e, page| {
            let rect = accent_drawio::Rect::new(600.0, 300.0, 40.0, 40.0);
            e.add_vertex(page, None, rect, picture, "").map(drop)
        });
        let id = tab.file().pages[0].cells.last().map(|c| c.id.clone());
        tab.select(id.iter().cloned().collect());
        for title in ["Fill", "Line Colour"] {
            if let Some(on) = switch_of(title) {
                on.set_active(true);
            }
        }
        println!(
            "bench diagram props image shown={:?} style={:?}",
            showing(),
            id.and_then(|id| style(&id))
        );

        // What is drawn and draw.io offers: a pattern and a font its menus do not list, a
        // shape's perimeter and corners, a label's turn, opacity and spacing, an arrow's
        // spacing, a flex arrow's widths and heads, a swimlane's divider, and Flip of an arrow
        // on its own and of a box placed by fractions.
        let shot = std::env::var("ACCENT_BENCH_DIAGRAM_SHOT").ok();
        let pane_shot = |name: &str| {
            let (Some(path), Some(content)) = (&shot, &content) else {
                return;
            };
            let path = Path::new(path).with_extension(format!("{name}.png"));
            println!("bench diagram props shot {name} {}", shoot(content, &path));
        };
        let set = |title: &str, value: f64| match row(title).and_downcast::<adw::SpinRow>() {
            Some(row) => row.set_value(value),
            None => println!("bench diagram props no_spin {title}"),
        };
        let picked = |title: &str| {
            let item = row(title).and_downcast::<adw::ComboRow>()?.selected_item();
            Some(
                item.and_downcast::<gtk::StringObject>()?
                    .string()
                    .to_string(),
            )
        };
        tab.select(vec!["a".to_string()]);
        choose("Pattern", "Custom");
        let dash = row("Dash Pattern").and_downcast::<adw::EntryRow>();
        println!(
            "bench diagram props custom shown={:?} text={:?}",
            shown(&pane, "Dash Pattern"),
            dash.as_ref().map(|d| d.text().to_string())
        );
        if let Some(dash) = &dash {
            dash.set_text("6 2 1 2");
            dash.emit_by_name::<()>("entry-activated", &[]);
        }
        set("Perimeter", 4.0);
        set("Arc Size", 30.0);
        set("Text Opacity", 60.0);
        set("Top", 5.0);
        if let Some(vertical) = row("Vertical Text").and_downcast::<adw::SwitchRow>() {
            vertical.set_active(true);
        }
        let ids = vec!["a".to_string()];
        tab.edit(|e, page| e.set_styles(page, &ids, &[("fontFamily", Some("Inter"))]));
        if let Some(spacing) = row("Spacing").and_downcast::<adw::ExpanderRow>() {
            spacing.set_expanded(true);
        }
        println!(
            "bench diagram props drawn a={:?} picked={:?}",
            style("a"),
            [picked("Pattern"), picked("Font")]
        );
        glib::timeout_future(Duration::from_millis(400)).await;
        pane_shot("shape");
        // Fixed Corner Size writes the corner a is drawn with as a size of its own, and back.
        for on in [true, false] {
            if let Some(fixed) = row("Fixed Corner Size").and_downcast::<adw::SwitchRow>() {
                fixed.set_active(on);
            }
            let arc = row("Arc Size").and_downcast::<adw::SpinRow>();
            println!(
                "bench diagram props corners fixed={on} a={:?} arc={:?}",
                style("a"),
                arc.map(|r| r.text().to_string())
            );
        }
        // Sketch writes draw.io's comic drawing onto a and takes it, and `comic`, off again.
        for on in [true, false] {
            if let Some(sketch) = row("Sketch").and_downcast::<adw::SwitchRow>() {
                sketch.set_active(on);
            }
            println!("bench diagram props sketch on={on} a={:?}", style("a"));
        }
        // Fill Opacity and Line Opacity write the fill's and the line's own opacity, each its own
        // step, and never the whole cell's, which would fade the label too.
        tab.select(vec!["b".to_string()]);
        set("Fill Opacity", 40.0);
        set("Line Opacity", 70.0);
        glib::timeout_future(Duration::from_millis(600)).await;
        let faded = style("b");
        tab.undo();
        println!(
            "bench diagram props opacity b={faded:?} undone={:?}",
            style("b")
        );
        tab.undo();
        tab.select(vec!["e".to_string()]);
        set("Start Spacing", 3.0);
        set("End Spacing", -2.0);
        glib::timeout_future(Duration::from_millis(600)).await;
        println!(
            "bench diagram props drawn e={:?} shown={:?}",
            style("e"),
            showing()
        );
        pane_shot("arrow");

        let model = r#"<mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
            <mxCell id="s" value="Lane" style="swimlane;startSize=23;" parent="1" vertex="1">
              <mxGeometry x="600" y="380" width="160" height="100" as="geometry"/></mxCell>
            <mxCell id="p" style="ellipse;" parent="s" vertex="1">
              <mxGeometry x="1" y="0.5" width="20" height="20" relative="1" as="geometry">
                <mxPoint x="-10" y="-10" as="offset"/></mxGeometry></mxCell>
            <mxCell id="f" style="shape=flexArrow;endArrow=classic;html=1;" parent="1" edge="1">
              <mxGeometry relative="1" as="geometry"><mxPoint x="600" y="100" as="sourcePoint"/>
                <mxPoint x="700" y="150" as="targetPoint"/></mxGeometry></mxCell>
            </root></mxGraphModel>"#;
        tab.paste_text(model, None);
        let pasted = tab.file().pages[0].clone();
        let find = |kind: &str| {
            let cell = pasted
                .cells
                .iter()
                .rev()
                .find(|c| c.style.to_string().contains(kind));
            cell.map(|c| c.id.clone()).unwrap_or_default()
        };
        let (lane, flex) = (find("swimlane"), find("flexArrow"));
        let port = pasted.children(&lane).next().map(|c| c.id.clone());
        let geometry = |id: &str| tab.file().pages[0].cell(id)?.geometry.clone();
        let ends = |id: &str| geometry(id).map(|g| (g.source_point, g.target_point));
        tab.select(vec![flex.clone()]);
        let heads = |title: &str| {
            let combo = row(title).and_downcast::<adw::ComboRow>()?;
            let list = combo.model().and_downcast::<gtk::StringList>()?;
            Some((0..list.n_items()).filter_map(|i| list.string(i)).count())
        };
        println!(
            "bench diagram props flex shown={:?} heads={:?} picked={:?}",
            showing(),
            [heads("Start"), heads("End")],
            [picked("Start"), picked("End")]
        );
        set("Arrow Width", 14.0);
        set("Start Width", 30.0);
        set("End Width", 26.0);
        choose("Start", "Block");
        glib::timeout_future(Duration::from_millis(600)).await;
        println!("bench diagram props flex style={:?}", style(&flex));
        pane_shot("flex");

        // A free line's ends as numbers, draw.io's Line Start and Line End, a line on shapes
        // having none: the rows follow its end dragged by XTEST, and End X typed shows on the
        // canvas, lands with Return as one step and is the file's targetPoint once saved.
        let end_row = |group: &str, title: &str| {
            let group = find_widget(&pane, &|w| {
                w.downcast_ref::<adw::PreferencesGroup>()
                    .is_some_and(|g| g.title() == group)
            })?;
            find_widget(&group, &|w| {
                w.downcast_ref::<adw::SpinRow>()
                    .is_some_and(|r| r.title() == title)
            })
            .and_downcast::<adw::SpinRow>()
        };
        let end_rows = || {
            let rows = [("Start", "X"), ("Start", "Y"), ("End", "X"), ("End", "Y")];
            rows.map(|(end, axis)| {
                let row = end_row(&format!("Line {end}"), axis).filter(|r| r.is_mapped())?;
                Some(row.text().to_string())
            })
        };
        tab.select(vec!["e".to_string()]);
        println!("bench diagram props ends joined={:?}", end_rows());
        tab.select(vec![flex.clone()]);
        println!(
            "bench diagram props ends free rows={:?} model={:?}",
            end_rows(),
            ends(&flex)
        );
        let handle = ends(&flex)
            .and_then(|(_, end)| end)
            .and_then(|at| screen(accent_drawio::Rect::new(at.x, at.y, 0.0, 0.0)));
        if let (Some(from), Some(on), Some(to)) = (
            point(handle.clone(), 0.0, 0.0),
            point(handle.clone(), 15.0, 10.0),
            point(handle, 30.0, 20.0),
        ) {
            super::super::git::xtest(&format!(
                "move {from}; down; move {on}; sleep 0.1; move {to}"
            ))
            .await;
            println!(
                "bench diagram props ends dragged rows={:?} model={:?}",
                end_rows(),
                ends(&flex)
            );
            super::super::git::xtest("up").await;
            println!(
                "bench diagram props ends dropped rows={:?} model={:?}",
                end_rows(),
                ends(&flex)
            );
            tab.undo();
        }
        let typing = ends(&flex);
        // The page's own scroller, which earlier steps left scrolled down, back to its top.
        let scroller = end_row("Line Start", "X")
            .and_then(|row| row.ancestor(gtk::ScrolledWindow::static_type()))
            .and_downcast::<gtk::ScrolledWindow>();
        if let Some(scroller) = scroller {
            scroller.vadjustment().set_value(0.0);
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let end_x = end_row("Line End", "X").and_then(|x| {
            let text = find_widget(x.upcast_ref(), &|w| w.is::<gtk::Text>())?;
            middle_on_screen(&app, &text)
        });
        if let Some(at) = end_x {
            super::super::git::xtest(&format!("move {at}; focus; down; up; key ctrl+a; type 720"))
                .await;
            println!(
                "bench diagram props ends typed held={:?} model={:?}",
                tab.held(),
                ends(&flex)
            );
            super::super::git::xtest("key Return").await;
            glib::timeout_future(Duration::from_millis(600)).await;
            println!(
                "bench diagram props ends entered rows={:?} model={:?}",
                end_rows(),
                ends(&flex)
            );
            glib::timeout_future(Duration::from_millis(1600)).await;
            let saved = std::fs::read(&path).ok().and_then(|xml| {
                let file = accent_drawio::File::from_bytes(&xml).ok()?;
                file.pages[0].cell(&flex)?.geometry.as_ref()?.target_point
            });
            println!("bench diagram props ends saved target={saved:?}");
            tab.undo();
            println!("bench diagram props ends undone={}", ends(&flex) == typing);
        }
        tab.key_target().grab_focus();
        let before = ends(&flex);
        click("Flip Horizontally");
        println!(
            "bench diagram props flip arrow before={before:?} after={:?}",
            ends(&flex)
        );
        tab.select(vec![lane.clone()]);
        println!("bench diagram props lane shown={:?}", showing());
        if let Some(divider) = row("Divider").and_downcast::<adw::SwitchRow>() {
            divider.set_active(false);
        }
        glib::timeout_future(Duration::from_millis(400)).await;
        pane_shot("swimlane");
        let port_at = || {
            let g = geometry(port.as_deref()?)?;
            Some((g.x, g.relative, g.offset))
        };
        let before = port_at();
        click("Flip Horizontally");
        println!(
            "bench diagram props flip lane={:?} port before={before:?} after={:?}",
            style(&lane),
            port_at()
        );
        // The pointer and the keys: Pattern's menu to its last entry, Custom, and numbers typed
        // into the row it opens.
        tab.select(vec!["b".to_string()]);
        glib::timeout_future(Duration::from_millis(300)).await;
        let (dx, dy) = app.window.surface_transform();
        let on_row = |title: &str| {
            let row = row(title)?;
            let scroller = row.ancestor(gtk::ScrolledWindow::static_type())?;
            let scroller = scroller.downcast_ref::<gtk::ScrolledWindow>()?;
            // From where the scroller shows it to 100 px below its top.
            let top = row.compute_point(&scroller.child()?, &graphene::Point::zero())?;
            let scroll = scroller.vadjustment();
            scroll.set_value(scroll.value() + f64::from(top.y()) - 100.0);
            let at = row.compute_point(&app.window, &graphene::Point::new(40.0, 20.0))?;
            Some(format!(
                "{} {}",
                f64::from(at.x()) + dx,
                f64::from(at.y()) + dy
            ))
        };
        if let Some(at) = on_row("Pattern") {
            glib::timeout_future(Duration::from_millis(300)).await;
            let at = on_row("Pattern").unwrap_or(at);
            super::super::git::xtest(&format!("move {at}; focus; sleep 0.2; down; up; sleep 0.4"))
                .await;
            super::super::git::xtest("key End; key Return; sleep 0.3").await;
            println!(
                "bench diagram props typed picked={:?} focus={:?}",
                picked("Pattern"),
                dash.as_ref()
                    .map(|d| d.state_flags().contains(gtk::StateFlags::FOCUS_WITHIN))
            );
            super::super::git::xtest("key ctrl+a; type 3 3; key Return").await;
        }
        println!("bench diagram props typed b={:?}", style("b"));

        // Saved a second after the last edit.
        glib::timeout_future(Duration::from_millis(1600)).await;
        let saved = std::fs::read(&path).ok().and_then(|xml| {
            let file = accent_drawio::File::from_bytes(&xml).ok()?;
            let style = |id: &str| file.pages[0].cell(id).map(|c| c.style.to_string());
            Some([style("a"), style("e"), style(&flex), style(&lane)])
        });
        println!("bench diagram props line saved={saved:?}");
        // A style typed and not entered lands on the cell it was typed for, before another is
        // selected.
        tab.select(vec!["a".to_string()]);
        // The entry, not the Text group's row of the same title.
        let raw = find_widget(&pane, &|w| {
            w.downcast_ref::<adw::EntryRow>()
                .is_some_and(|r| r.title() == "Style")
        });
        if let Some(raw) = raw.and_downcast::<adw::EntryRow>() {
            raw.set_text("rounded=1;");
        }
        tab.select(vec!["b".to_string()]);
        println!(
            "bench diagram props unentered a={:?} b={:?}",
            style("a"),
            style("b")
        );
        // Fixed Size writes how far a hexagon's corners reach in as a length of its own, and
        // back, shown only where there is such a shape, Sketch only where it wobbles a line.
        let rect = accent_drawio::Rect::new(600.0, 200.0, 120.0, 60.0);
        tab.edit(|e, page| {
            e.add_vertex(page, None, rect, "shape=hexagon;", "")
                .map(drop)
        });
        let hexagon = tab.file().pages[0].cells.last().map(|c| c.id.clone());
        tab.select(hexagon.iter().cloned().collect());
        for on in [true, false] {
            if let Some(sized) = row("Fixed Size").and_downcast::<adw::SwitchRow>() {
                sized.set_active(on);
            }
            println!(
                "bench diagram props sized fixed={on} hexagon={:?} shown={:?}",
                hexagon.as_deref().and_then(style),
                shown(&pane, "Fixed Size")
            );
        }
        let rows = ["Fixed Size", "Sketch"];
        for cells in [vec!["a"], vec!["m"], vec!["m", "e"]] {
            tab.select(cells.iter().map(|id| id.to_string()).collect());
            let showing = rows.map(|title| shown(&pane, title).is_some());
            println!("bench diagram props sized shown {cells:?} {showing:?}");
        }
        bench_quit(&app);
    });
}

/// Right-click the screen at `at` by XTEST: the labels of the menu that comes up, closed again,
/// or `None` for no menu.
async fn menu_labels(app: &Rc<App>, at: Option<&str>) -> Option<Vec<String>> {
    super::super::git::xtest(&format!("move {}; down 3; up 3", at?)).await;
    let menu = find_widget(app.window.upcast_ref(), &|w| {
        w.is::<gtk::PopoverMenu>() && w.is_visible()
    })
    .and_downcast::<gtk::PopoverMenu>()?;
    let labels = menu.menu_model().map(|m| fileops::labels(&m));
    menu.popdown();
    glib::timeout_future(Duration::from_millis(300)).await;
    labels
}

/// The group the row titled `title` shows in within `pane`, if it and everything above it is
/// visible.
fn shown(pane: &gtk::Widget, title: &str) -> Option<String> {
    let mut at = find_widget(pane, &|w| {
        w.downcast_ref::<adw::PreferencesRow>()
            .is_some_and(|r| r.title() == title)
    });
    let mut group = None;
    while let Some(w) = at {
        if !w.is_visible() {
            return None;
        }
        if let Some(g) = w.downcast_ref::<adw::PreferencesGroup>() {
            group.get_or_insert_with(|| g.title().to_string());
        }
        if &w == pane {
            return group;
        }
        at = w.parent();
    }
    None
}
