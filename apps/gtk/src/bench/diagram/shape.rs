//! The Shape tool through real input by XTEST: its ring button, the grid of shapes on its slot,
//! picked by the keys and by a click, and drawn by a drag and by a click (DESIGN.md, Diagram).

use super::*;

/// `ACCENT_BENCH_DIAGRAM=shape:<rel>` takes the Shape tool from the ring of [`SAMPLE`] at `<rel>`,
/// and prints the ring, then the grid its slot opens, each held 2 s for a screenshot from
/// outside; where the arrow keys move to, what `Escape` and `Return` leave, the cell a drag then
/// draws, a Basic shape clicked in the grid, where `Up` and `Down` go from that grid's first, and
/// the cell a click drops at the shape's own size, with whether each cell's style is its entry's.
pub(super) fn shape(app: &Rc<App>, rel: &str) {
    std::fs::write(app.root().join(rel), SAMPLE).expect("write the sample diagram");
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        // A file written a moment ago is only in the vault once the watcher has seen it.
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        let laid_out = || app.active_diagram().is_some_and(|tab| tab.laid_out());
        until("shape", "laid out", files::WAIT, laid_out).await;
        let Some(tab) = app.active_diagram() else {
            println!("bench shape no_tab");
            return bench_quit(&app);
        };
        let page = tab.page.child();
        let ring = find_widget(&page, &|w| w.is::<crate::ring::RingBox>() && w.is_visible());
        let Some(ring) = ring else {
            println!("bench shape no_ring");
            return bench_quit(&app);
        };
        // Page point `(x, y)` on the screen.
        let at = |x: f64, y: f64| {
            let r = tab.to_widget(&accent_drawio::Rect::new(x, y, 0.0, 0.0));
            on_screen(&app, &tab.key_target(), r.x, r.y).unwrap_or_default()
        };
        // The ring's button `label` names.
        let button = |label: &str| {
            find_widget(&ring, &|w| {
                w.is::<gtk::Button>()
                    && w.is_visible()
                    && w.tooltip_text().as_deref() == Some(label)
            })
        };
        let press = |label: &str| {
            let at = button(label).and_then(|b| middle_on_screen(&app, &b));
            async move {
                match at {
                    Some(at) => super::super::git::xtest(&format!("move {at}; down; up")).await,
                    None => println!("bench shape no_button"),
                }
            }
        };
        let keys = |chord: &str| {
            let steps = format!("key {chord}");
            async move { super::super::git::xtest(&steps).await }
        };
        let grid = || find_widget(&ring, &|w| w.is::<gtk::Popover>());
        let open = || grid().is_some_and(|g| g.is_visible());
        let focus = || gtk::prelude::RootExt::focus(&app.window).and_then(|w| w.tooltip_text());
        let keyboard =
            || gtk::prelude::RootExt::focus(&app.window).as_ref() == Some(&tab.key_target());
        let slot = || tab.options().shape.title;
        let state = || {
            format!(
                "open={} slot={} config={} tool={:?} keyboard={}",
                open(),
                slot(),
                app.config.borrow().diagram.shape.title,
                tab.tool(),
                keyboard()
            )
        };
        super::super::git::xtest(&format!("move {}; focus", at(20.0, 480.0))).await;

        press("Add Shape").await;
        let checked = button(slot()).and_downcast::<gtk::ToggleButton>();
        println!(
            "bench shape ring tool={:?} slot={} checked={}",
            tab.tool(),
            slot(),
            checked.is_some_and(|b| b.is_active())
        );
        glib::timeout_future(Duration::from_secs(2)).await;

        press(slot()).await;
        let shown = |palette: usize| {
            let flows = grid()
                .map(|g| children_of::<gtk::FlowBox>(&g))
                .unwrap_or_default();
            let titles = flows
                .get(palette)
                .map(|f| children_of::<gtk::FlowBoxChild>(f.upcast_ref()));
            titles
                .unwrap_or_default()
                .iter()
                .filter_map(|c| c.tooltip_text())
                .map(String::from)
                .collect::<Vec<_>>()
        };
        let (general, basic) = (shown(0), shown(1));
        println!(
            "bench shape grid open={} general={} selected={:?} focus={:?}",
            open(),
            general.len(),
            selected(grid()),
            focus()
        );
        println!(
            "bench shape basic shown={} diagonal={}",
            basic.len(),
            basic.iter().any(|t| t == "Diagonal Rounded Rectangle")
        );
        glib::timeout_future(Duration::from_secs(2)).await;

        keys("Right").await;
        println!(
            "bench shape right focus={:?} selected={:?}",
            focus(),
            selected(grid())
        );
        keys("Escape").await;
        println!("bench shape escape {}", state());

        press(slot()).await;
        for _ in 0..3 {
            keys("Right").await;
        }
        keys("Return").await;
        println!("bench shape return {}", state());

        let before = cells(&tab);
        super::super::git::xtest(&format!("drag {} {}", at(240.0, 180.0), at(320.0, 260.0))).await;
        println!(
            "bench shape drawn {} tool={:?}",
            added(&tab, &before),
            tab.tool()
        );

        press("Add Shape").await;
        press(slot()).await;
        let diagonal = grid().and_then(|g| {
            find_widget(&g, &|w| {
                w.is::<gtk::FlowBoxChild>()
                    && w.tooltip_text().as_deref() == Some("Diagonal Rounded Rectangle")
            })
        });
        // Scrolled into sight first, which a grid holding more shapes than it shows needs.
        if let Some(child) = &diagonal {
            child.grab_focus();
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        match diagonal.and_then(|c| in_popover(&c)) {
            Some(at) => super::super::git::xtest(&format!("move {at}; down; up")).await,
            None => println!("bench shape no_entry"),
        }
        println!("bench shape click {}", state());

        press(slot()).await;
        let palette = |title: &str| match general.iter().any(|t| t == title) {
            true => "General",
            false => "Basic",
        };
        // From the first Basic shape, wherever the pick is among however many are ported.
        for key in ["Home", "Up", "Down"] {
            keys(key).await;
            let at = focus().unwrap_or_default();
            println!("bench shape {key} palette={} focus={at}", palette(&at));
        }
        keys("Escape").await;

        let before = cells(&tab);
        super::super::git::xtest(&format!("move {}; down; up", at(470.0, 260.0))).await;
        println!(
            "bench shape dropped {} tool={:?}",
            added(&tab, &before),
            tab.tool()
        );
        bench_quit(&app);
    });
}

/// The children of `root` of type `T`, at any depth, in order.
fn children_of<T: IsA<gtk::Widget>>(root: &gtk::Widget) -> Vec<T> {
    let mut found = Vec::new();
    let mut child = root.first_child();
    while let Some(c) = child {
        match c.clone().downcast::<T>() {
            Ok(t) => found.push(t),
            Err(c) => found.extend(children_of::<T>(&c)),
        }
        child = c.next_sibling();
    }
    found
}

/// The titles selected in the grid, across its palettes.
fn selected(grid: Option<gtk::Widget>) -> Vec<String> {
    let flows = grid
        .map(|g| children_of::<gtk::FlowBox>(&g))
        .unwrap_or_default();
    (flows.iter())
        .flat_map(|f| f.selected_children())
        .filter_map(|c| c.tooltip_text())
        .map(String::from)
        .collect()
}

/// The ids of the page's cells.
fn cells(tab: &crate::diagram::DiagramTab) -> Vec<String> {
    let file = accent_drawio::File::from_bytes(tab.text().as_bytes()).expect("our own XML");
    file.pages[0].cells.iter().map(|c| c.id.clone()).collect()
}

/// The cell added since `before`: its style, its size, and whether the style is the palette
/// entry's the tab draws.
fn added(tab: &crate::diagram::DiagramTab, before: &[String]) -> String {
    let file = accent_drawio::File::from_bytes(tab.text().as_bytes()).expect("our own XML");
    let new: Vec<_> = (file.pages[0].cells.iter())
        .filter(|c| !before.contains(&c.id))
        .collect();
    let [cell] = new.as_slice() else {
        return format!("cells={}", new.len());
    };
    let size = cell.geometry.as_ref().map(|g| (g.width, g.height));
    let size = size.map_or("none".to_string(), |(w, h)| format!("{w}x{h}"));
    let entry = tab.options().shape;
    format!(
        "title={} size={size} match={} style={}",
        entry.title,
        cell.style.to_string() == entry.style,
        cell.style
    )
}

/// Where the middle of `widget`, inside a popover, is on the screen: the popover's surface placed
/// against the window's, which sits at 0,0 with no window manager.
fn in_popover(widget: &gtk::Widget) -> Option<String> {
    let popover = widget.ancestor(gtk::Popover::static_type())?;
    let popover = popover.downcast::<gtk::Popover>().ok()?;
    let popup = popover.surface()?.dynamic_cast::<gdk::Popup>().ok()?;
    let (w, h) = (widget.width() as f32 / 2.0, widget.height() as f32 / 2.0);
    let p = widget.compute_point(&popover, &graphene::Point::new(w, h))?;
    let (sx, sy) = popover.surface_transform();
    Some(format!(
        "{} {}",
        f64::from(popup.position_x()) + sx + f64::from(p.x()),
        f64::from(popup.position_y()) + sy + f64::from(p.y())
    ))
}
