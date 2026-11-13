//! Reading through a diagram's pages with real input, through XTEST.

use super::diagram::{middle_on_screen, on_screen};
use super::*;

/// Four pages small enough to be fitted, so each is at both its edges.
const PAGES: &str = r#"<mxfile host="accent">
  <diagram name="One" id="nav-1"><mxGraphModel pageWidth="400" pageHeight="300"><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram>
  <diagram name="Two" id="nav-2"><mxGraphModel pageWidth="400" pageHeight="300"><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram>
  <diagram name="Three" id="nav-3"><mxGraphModel pageWidth="400" pageHeight="300"><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram>
  <diagram name="Four" id="nav-4"><mxGraphModel pageWidth="400" pageHeight="300"><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram>
</mxfile>
"#;

/// Labels over three pages, two of them matching `alpha` and one `^b`.
const LABELS: &str = r#"<mxfile host="accent">
  <diagram name="One" id="find-1"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
    <mxCell id="a" value="Alpha" style="rounded=0;" parent="1" vertex="1"><mxGeometry x="40" y="40" width="120" height="60" as="geometry"/></mxCell>
    <mxCell id="b" value="Beta" style="rounded=0;" parent="1" vertex="1"><mxGeometry x="240" y="40" width="120" height="60" as="geometry"/></mxCell>
  </root></mxGraphModel></diagram>
  <diagram name="Two" id="find-2"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
    <mxCell id="c" value="alpha two" style="rounded=0;" parent="1" vertex="1"><mxGeometry x="40" y="40" width="120" height="60" as="geometry"/></mxCell>
  </root></mxGraphModel></diagram>
  <diagram name="Three" id="find-3"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
    <mxCell id="d" value="Gamma" style="rounded=0;" parent="1" vertex="1"><mxGeometry x="40" y="40" width="120" height="60" as="geometry"/></mxCell>
  </root></mxGraphModel></diagram>
</mxfile>
"#;

/// Two pages whose shapes link to each other, and one to the web.
const LINKS: &str = r#"<mxfile host="accent">
  <diagram name="One" id="link-1"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
    <UserObject label="Go" link="data:page/id,link-2" id="go"><mxCell style="rounded=0;" parent="1" vertex="1"><mxGeometry x="40" y="40" width="120" height="60" as="geometry"/></mxCell></UserObject>
    <UserObject label="Web" link="https://example.org/" id="web"><mxCell style="rounded=0;" parent="1" vertex="1"><mxGeometry x="240" y="40" width="120" height="60" as="geometry"/></mxCell></UserObject>
  </root></mxGraphModel></diagram>
  <diagram name="Two" id="link-2"><mxGraphModel><root><mxCell id="0"/><mxCell id="1" parent="0"/>
    <UserObject label="Back" link="data:page/id,link-1" id="back"><mxCell style="rounded=0;" parent="1" vertex="1"><mxGeometry x="40" y="40" width="120" height="60" as="geometry"/></mxCell></UserObject>
  </root></mxGraphModel></diagram>
</mxfile>
"#;

/// `ACCENT_BENCH_DIAGRAM_NAV=spring:<rel>` writes four small pages to `<rel>` and turns them with
/// the wheel (DESIGN.md, Diagram): each step prints the page it left the reader on, the furthest
/// the page was pulled off its edge meanwhile and the pull once it has rested, presented last.
/// `find:<rel>` writes three pages of labels and searches them in the pane's find bar, then goes
/// to the last page through Go to Diagram Page…, printing the readout, the page and the selection.
/// `links:<rel>` writes two pages whose shapes link to each other and to the web, and with real
/// input shows a link with Ctrl held, selects a linked shape by Ctrl+click while editing, follows
/// a link by a click on its popover and one by a click on a presented page, a plain click while
/// editing selecting.
pub(super) fn bench_diagram_nav(app: &Rc<App>, arg: &str) {
    match arg.split_once(':') {
        Some(("spring", rel)) => spring(app, rel),
        Some(("find", rel)) => find(app, rel),
        Some(("links", rel)) => links(app, rel),
        _ => {
            println!("bench nav unknown {arg}");
            bench_quit(app);
        }
    }
}

/// Write `text` to `rel`, open it, and hand over its tab once its page is laid out.
async fn opened(app: &Rc<App>, rel: &str, text: &str) -> Option<Rc<crate::diagram::DiagramTab>> {
    std::fs::write(app.root().join(rel), text).expect("write the diagram");
    // A file written a moment ago is only in the vault once the watcher has seen it.
    glib::timeout_future(Duration::from_millis(1500)).await;
    app.open_path(rel);
    until("nav", "laid out", files::WAIT, || {
        app.active_diagram().is_some_and(|tab| tab.laid_out())
    })
    .await;
    let tab = app.active_diagram();
    if tab.is_none() {
        println!("bench nav no_tab");
    }
    tab
}

fn find(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(tab) = opened(&app, &rel, LABELS).await else {
            return bench_quit(&app);
        };
        let pane = app.pane();
        let run = |action: &str| {
            let _ = WidgetExt::activate_action(&app.window, action, None);
        };
        let say = |step: &str| {
            println!(
                "bench nav find {step} readout={:?} page={} selected={:?}",
                pane.find.readout().0,
                tab.page_index() + 1,
                tab.selection()
            );
        };
        // Past the query box's own delay before it says what changed.
        let typed = |text: &str| pane.find.query_box().set_text(text);
        run("win.find");
        typed("alpha");
        glib::timeout_future(Duration::from_millis(600)).await;
        say("alpha");
        run("win.find-next");
        glib::timeout_future(Duration::from_millis(300)).await;
        say("next");
        run("win.find-next");
        glib::timeout_future(Duration::from_millis(300)).await;
        say("round");
        pane.find.toggles()[2].set_active(true);
        typed("^b");
        glib::timeout_future(Duration::from_millis(600)).await;
        say("regex");
        typed("(");
        glib::timeout_future(Duration::from_millis(600)).await;
        say("invalid");
        pane.find.close();
        run("win.diagram-go-to-page");
        glib::timeout_future(Duration::from_millis(300)).await;
        pane.find.line_box().set_text("3");
        pane.find.line_box().emit_activate();
        glib::timeout_future(Duration::from_millis(300)).await;
        say("go to page 3");
        bench_quit(&app);
    });
}

fn links(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(tab) = opened(&app, &rel, LINKS).await else {
            return bench_quit(&app);
        };
        // `up` pixels above the middle of cell `id` on the screen, as the page is laid out now.
        let above = |id: &str, up: f64| {
            let r = tab.frame_of(id).map(|r| tab.to_widget(&r))?;
            on_screen(
                &app,
                &tab.key_target(),
                r.x + r.w / 2.0,
                r.y + r.h / 2.0 - up,
            )
        };
        let at = |id: &str| above(id, 0.0).unwrap_or_default();
        let (go, web) = (at("go"), at("web"));
        super::git::xtest(&format!(
            "move {go}; focus; keydown ctrl; move {web}; sleep 0.3"
        ))
        .await;
        let popover = || {
            find_widget(app.window.upcast_ref(), &|w| {
                w.is::<gtk::Popover>() && w.is_visible()
            })
        };
        let preview = popover()
            .and_then(|popover| find_widget(&popover, &|w| w.is::<gtk::Label>()))
            .and_downcast::<gtk::Label>()
            .map(|label| label.label().to_string());
        println!("bench nav link preview={preview:?}");
        super::git::xtest(&format!("move {go}; down; up; keyup ctrl")).await;
        let say = |step: &str| {
            println!(
                "bench nav link {step} page={} selected={:?}",
                tab.page_index() + 1,
                tab.selection()
            );
        };
        say("ctrl-click");
        // Ctrl over the shape and let go, then over to the middle of its popover, a surface of its
        // own: where its parent's puts it, its shadow and the content in it.
        let near = above("go", 1.0).unwrap_or_default();
        super::git::xtest(&format!("keydown ctrl; move {near}; sleep 0.3; keyup ctrl")).await;
        let over = popover().and_then(|popover| {
            let content = popover.first_child()?;
            let bounds = content.compute_bounds(&popover)?;
            let native = popover.native()?;
            let (sx, sy) = native.surface_transform();
            let surface = native.surface()?;
            let popup = surface.dynamic_cast_ref::<gdk::Popup>()?;
            Some(format!(
                "{} {}",
                f64::from(popup.position_x()) + sx + f64::from(bounds.center().x()),
                f64::from(popup.position_y()) + sy + f64::from(bounds.center().y())
            ))
        });
        let over = over.unwrap_or_default();
        super::git::xtest(&format!("move {over}; sleep 0.2; down; up")).await;
        say("popover");
        let back = at("back");
        super::git::xtest(&format!("move {back}; down; up")).await;
        say("click");
        app.set_presenting(true);
        glib::timeout_future(Duration::from_millis(500)).await;
        let back = at("back");
        super::git::xtest(&format!("move {back}; down; up")).await;
        say("presented click");
        app.set_presenting(false);
        bench_quit(&app);
    });
}

fn spring(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(tab) = opened(&app, &rel, PAGES).await else {
            return bench_quit(&app);
        };
        // The furthest pull since the step began, sampled every 5 ms: a notch holds one 50 ms.
        let furthest = Rc::new(Cell::new(0.0_f64));
        let sampler = glib::timeout_add_local(
            Duration::from_millis(5),
            glib::clone!(
                #[weak]
                tab,
                #[strong]
                furthest,
                #[upgrade_or]
                glib::ControlFlow::Break,
                move || {
                    if tab.pull().abs() > furthest.get().abs() {
                        furthest.set(tab.pull());
                    }
                    glib::ControlFlow::Continue
                }
            ),
        );
        let at = middle_on_screen(&app, &tab.key_target()).unwrap_or_default();
        super::git::xtest(&format!("move {at}; focus")).await;
        let steps = [
            ("one notch", "scroll -1"),
            ("two notches", "scroll -2"),
            ("three notches", "scroll -3"),
            ("a spin of six", "scroll -6"),
            ("one notch up", "scroll 1"),
        ];
        for (name, scroll) in steps {
            furthest.set(0.0);
            super::git::xtest(scroll).await;
            // Past the rest and the spring back, which the 600 ms after the steps may not be.
            glib::timeout_future(Duration::from_millis(400)).await;
            println!(
                "bench nav spring {name} page={} pull={:.0} rested={:.0}",
                tab.page_index() + 1,
                furthest.get(),
                tab.pull()
            );
        }
        // A presented page is a slide: a notch turns it, and nothing is pulled.
        app.set_presenting(true);
        glib::timeout_future(Duration::from_millis(500)).await;
        furthest.set(0.0);
        super::git::xtest("scroll -1").await;
        println!(
            "bench nav spring presented one notch page={} pull={:.0}",
            tab.page_index() + 1,
            furthest.get()
        );
        app.set_presenting(false);
        sampler.remove();
        bench_quit(&app);
    });
}
