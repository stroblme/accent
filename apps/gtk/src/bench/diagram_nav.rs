//! Reading through a diagram's pages with real input, through XTEST.

use super::diagram::middle_on_screen;
use super::*;

/// Four pages small enough to be fitted, so each is at both its edges.
const PAGES: &str = r#"<mxfile host="accent">
  <diagram name="One" id="nav-1"><mxGraphModel pageWidth="400" pageHeight="300"><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram>
  <diagram name="Two" id="nav-2"><mxGraphModel pageWidth="400" pageHeight="300"><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram>
  <diagram name="Three" id="nav-3"><mxGraphModel pageWidth="400" pageHeight="300"><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram>
  <diagram name="Four" id="nav-4"><mxGraphModel pageWidth="400" pageHeight="300"><root><mxCell id="0"/><mxCell id="1" parent="0"/></root></mxGraphModel></diagram>
</mxfile>
"#;

/// `ACCENT_BENCH_DIAGRAM_NAV=spring:<rel>` writes four small pages to `<rel>` and turns them with
/// the wheel (DESIGN.md, Diagram): each step prints the page it left the reader on, the furthest
/// the page was pulled off its edge meanwhile and the pull once it has rested, presented last.
pub(super) fn bench_diagram_nav(app: &Rc<App>, arg: &str) {
    match arg.split_once(':') {
        Some(("spring", rel)) => spring(app, rel),
        _ => {
            println!("bench nav unknown {arg}");
            bench_quit(app);
        }
    }
}

fn spring(app: &Rc<App>, rel: &str) {
    std::fs::write(app.root().join(rel), PAGES).expect("write the pages");
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        // A file written a moment ago is only in the vault once the watcher has seen it.
        glib::timeout_future(Duration::from_millis(1500)).await;
        app.open_path(&rel);
        until("nav", "laid out", files::WAIT, || {
            app.active_diagram().is_some_and(|tab| tab.laid_out())
        })
        .await;
        let Some(tab) = app.active_diagram() else {
            println!("bench nav no_tab");
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
