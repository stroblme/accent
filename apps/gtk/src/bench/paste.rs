//! A drill over formatted paste: `Ctrl+V` writing another application's HTML as the Markdown or
//! org a note, a markdown cell or a diagram reads, and Paste as Plain Text putting in its text;
//! and a drop of the same HTML, dragged in through XTEST.

use super::*;

/// What every case pastes, as a browser offers it: HTML beside its plain text.
const HTML: &str = r#"<meta charset="utf-8"><h2>Pasted</h2><p>Some <b>bold</b> and a <a href="https://example.org/">link</a>.</p><ul><li>one</li><li>two</li></ul>"#;
const PLAIN: &str = "Pasted\nSome bold and a link.\none\ntwo";

/// `ACCENT_BENCH_PASTE=1` on a vault holding `paste.md`, `paste.org`, `paste.py`, `paste.ipynb`,
/// `paste.drawio`, `drop.md` and `drop.org` (`build-aux/drills.sh`): Ctrl+V and Paste as Plain
/// Text in a note, beside its code block, table, a URL over a selection and VS Code's HTML; in an
/// org note and a code tab; a drop in a note, an org note and a code tab; in a note renamed between
/// markdown and org; in a notebook's markdown and code cells; on a diagram's canvas and in its
/// label editor.
/// Prints what each put in.
pub(super) fn bench_paste(app: &Rc<App>) {
    let app = app.clone();
    glib::spawn_future_local(async move {
        let act = |action: &str| {
            let _ = WidgetExt::activate_action(&app.window, action, None);
        };
        let Some(note) = opened(&app, "paste.md").await else {
            println!("bench paste no_tab paste.md");
            return bench_quit(&app);
        };
        let buffer = note.buffer.clone();
        let at = |find: &str| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
            let offset = text
                .find(find)
                .map_or(0, |at| text[..at + find.len()].chars().count());
            buffer.place_cursor(&buffer.iter_at_offset(offset as i32));
        };
        let end = || buffer.place_cursor(&buffer.end_iter());
        for (case, place, html, plain, paste) in [
            ("markdown", None, HTML, PLAIN, None),
            ("plain", None, HTML, PLAIN, Some("win.paste-plain")),
            (
                "ctrl_shift_v",
                None,
                HTML,
                PLAIN,
                Some("win.terminal-paste"),
            ),
            ("code_block", Some("code"), HTML, PLAIN, None),
            ("table", Some("| 1 | 2 |"), HTML, PLAIN, None),
            ("front_matter", Some("title: Note"), HTML, PLAIN, None),
        ] {
            offer(&app, html, plain, false);
            match place {
                Some(find) => at(find),
                None => end(),
            }
            let put = pasted(&buffer, || match paste {
                Some(action) => act(action),
                None => note.view.emit_paste_clipboard(),
            })
            .await;
            println!("bench paste note {case}={put:?}");
            if case == "markdown" {
                let before = text(&buffer);
                buffer.undo();
                println!(
                    "bench paste note undo removed={}",
                    before.len() - text(&buffer).len() == put.len()
                );
            }
        }
        // A URL over a selection links it, whatever HTML comes with it.
        offer(
            &app,
            r#"<a href="https://example.org/x">https://example.org/x</a>"#,
            "https://example.org/x",
            false,
        );
        at("before");
        let caret = buffer.iter_at_mark(&buffer.get_insert());
        buffer.select_range(&buffer.iter_at_offset(caret.offset() - 6), &caret);
        let put = pasted(&buffer, || note.view.emit_paste_clipboard()).await;
        println!("bench paste note link={put:?}");
        // A link copied alone, as a browser's Copy Link copies it, is its address.
        end();
        let put = pasted(&buffer, || note.view.emit_paste_clipboard()).await;
        println!("bench paste note copied_link={put:?}");
        // VS Code's styled spans are code.
        offer(&app, HTML, PLAIN, true);
        end();
        let put = pasted(&buffer, || note.view.emit_paste_clipboard()).await;
        println!("bench paste note vscode={put:?}");

        offer(&app, HTML, PLAIN, false);
        for rel in ["paste.org", "paste.py"] {
            let Some(tab) = opened(&app, rel).await else {
                println!("bench paste no_tab {rel}");
                continue;
            };
            tab.buffer.place_cursor(&tab.buffer.end_iter());
            let put = pasted(&tab.buffer, || tab.view.emit_paste_clipboard()).await;
            println!("bench paste {rel}={put:?}");
        }

        renamed(&app).await;
        notebook(&app, &act).await;
        diagram(&app, &act).await;
        // Last, as the window it drags from takes the keyboard from this one.
        dropped(&app).await;
        bench_quit(&app);
    });
}

/// A note renamed to the other syntax keeps its tab (`Tab::retarget`) and pastes as one opened in
/// it does: a URL over a selection links in org's way and formatted text goes in as its text in
/// `paste.md` renamed to org, and the reverse in `paste.org` renamed to markdown. Leaves the
/// browser's HTML on the clipboard, which the cases after it paste.
async fn renamed(app: &Rc<App>) {
    let Some(ops) = app.ops().cloned() else {
        println!("bench paste renamed no_vault");
        return;
    };
    until("paste", "reconcile", 30_000, || app.reconciled.get()).await;
    let url = "https://example.org/x";
    for (from, to, word) in [
        ("paste.md", "renamed.org", "end"),
        ("paste.org", "renamed.md", "Org"),
    ] {
        crate::fileops::move_all(&ops, vec![(from.to_string(), to.to_string())]);
        let Some(tab) = until("paste", to, 10_000, || app.tab_for(to)).await else {
            continue;
        };
        let buffer = &tab.buffer;
        let all = text(buffer);
        let start = all.find(word).map_or(0, |at| all[..at].chars().count()) as i32;
        let end = start + word.chars().count() as i32;
        buffer.select_range(&buffer.iter_at_offset(start), &buffer.iter_at_offset(end));
        offer(app, url, url, false);
        let put = pasted(buffer, || tab.view.emit_paste_clipboard()).await;
        println!("bench paste {to} link={put:?}");
        offer(app, HTML, PLAIN, false);
        buffer.place_cursor(&buffer.end_iter());
        let put = pasted(buffer, || tab.view.emit_paste_clipboard()).await;
        println!("bench paste {to} formatted={put:?}");
    }
}

/// A markdown cell converts and a code cell takes the plain text.
async fn notebook(app: &Rc<App>, act: &impl Fn(&str)) {
    app.open_path("paste.ipynb");
    let Some(tab) = until("paste", "notebook", 5000, || {
        let tab = app.active_notebook()?;
        (tab.bench_view(0).is_some() && tab.bench_view(1).is_some()).then_some(tab)
    })
    .await
    else {
        return;
    };
    for (n, case) in [(0, "markdown_cell"), (1, "code_cell")] {
        tab.bench_select(n);
        let Some(view) = tab.bench_view(n) else {
            continue;
        };
        let buffer = view.buffer();
        buffer.place_cursor(&buffer.end_iter());
        let put = pasted(&buffer, || view.emit_paste_clipboard()).await;
        println!("bench paste notebook {case}={put:?}");
    }
    tab.bench_select(0);
    if let Some(view) = tab.bench_view(0) {
        let buffer = view.buffer();
        buffer.place_cursor(&buffer.end_iter());
        let put = pasted(&buffer, || act("win.paste-plain")).await;
        println!("bench paste notebook markdown_cell_plain={put:?}");
    }
}

/// The canvas pastes the HTML as a formatted text cell and Paste as Plain Text as a plain one,
/// `Ctrl+Shift+V` being its chord there; the label editor converts as a label's Markdown.
async fn diagram(app: &Rc<App>, act: &impl Fn(&str)) {
    app.open_path("paste.drawio");
    let Some(tab) = until("paste", "diagram", 5000, || app.active_diagram()).await else {
        return;
    };
    let mut cells = Vec::new();
    for (case, action) in [
        ("canvas", "win.diagram-paste"),
        ("canvas_plain", "win.paste-plain"),
        ("canvas_ctrl_shift_v", "win.terminal-paste"),
    ] {
        let before = tab.selection();
        act(action);
        let Some(id) = until("paste", case, 3000, || {
            let now = tab.selection();
            (now != before).then(|| now.first().cloned()).flatten()
        })
        .await
        else {
            continue;
        };
        println!(
            "bench paste diagram {case}={:?}",
            tab.label_markdown(&id).unwrap_or_default()
        );
        cells.push(id);
    }
    // With the keyboard on the canvas, where the editing chords are the window's.
    tab.key_target().grab_focus();
    pumped().await;
    let accels = (app.window.application()).map(|a| a.accels_for_action("win.terminal-paste"));
    let enabled = (app.window.lookup_action("terminal-paste")).is_some_and(|a| a.is_enabled());
    println!("bench paste diagram ctrl_shift_v accels={accels:?} enabled={enabled}");
    // The plain cell's label, which the formatted paste changes.
    let Some(id) = cells.get(1).cloned() else {
        return;
    };
    tab.select(vec![id]);
    tab.edit_label();
    let Some(view) = until("paste", "label editor", 3000, || {
        tab.label_view().filter(|v| v.has_focus())
    })
    .await
    else {
        return;
    };
    let buffer = view.buffer();
    for (case, paste) in [("label", None), ("label_plain", Some("win.paste-plain"))] {
        buffer.select_range(&buffer.start_iter(), &buffer.end_iter());
        pasted(&buffer, || match paste {
            Some(action) => act(action),
            None => view.emit_paste_clipboard(),
        })
        .await;
        println!("bench paste diagram {case}={:?}", text(&buffer));
    }
}

/// Put `html` and `plain` on the clipboard, as a browser copies formatted text, and VS Code's own
/// data beside them where `vscode` says so.
fn offer(app: &Rc<App>, html: &str, plain: &str, vscode: bool) {
    let mut providers = vec![
        gdk::ContentProvider::for_bytes("text/html", &glib::Bytes::from(html.as_bytes())),
        gdk::ContentProvider::for_value(&plain.to_value()),
    ];
    if vscode {
        let data = glib::Bytes::from_static(br#"{"version":1,"mode":"rust"}"#);
        providers.push(gdk::ContentProvider::for_bytes("vscode-editor-data", &data));
    }
    let union = gdk::ContentProvider::new_union(&providers);
    if let Err(e) = app.window.clipboard().set_content(Some(&union)) {
        println!("bench paste offer failed {e}");
    }
}

/// HTML dragged in from another window and let go at the end of a note, an org note and a code
/// tab, and plain text at the end of the note: the note's and the org note's own drop writes their
/// markup in one undo step, the others are GTK's, which put the text in. A browser's drag is
/// another process's, which Xvfb cannot start, so a window of the drill's own offers the same
/// HTML and text, dragged by XTEST.
async fn dropped(app: &Rc<App>) {
    let source = gtk::Window::builder()
        .decorated(false)
        .default_width(160)
        .default_height(48)
        .build();
    let label = gtk::Label::new(Some("drag me"));
    source.set_child(Some(&label));
    let drag = gtk::DragSource::new();
    drag.set_actions(gdk::DragAction::COPY);
    let content = Rc::new(RefCell::new(None::<gdk::ContentProvider>));
    let offered = content.clone();
    drag.connect_prepare(move |_, _, _| offered.borrow().clone());
    // Whether the last drag ended in a drop, rather than cancelled.
    let ended = Rc::new(Cell::new(None));
    let end = ended.clone();
    drag.connect_drag_end(move |_, _, _| end.set(end.get().or(Some(true))));
    let cancel = ended.clone();
    drag.connect_drag_cancel(move |_, _, _| {
        cancel.set(Some(false));
        false
    });
    label.add_controller(drag);
    source.present();
    pumped().await;
    for (rel, html) in [
        ("drop.md", true),
        ("drop.md", false),
        ("drop.org", true),
        ("paste.py", true),
    ] {
        let Some(tab) = opened(app, rel).await else {
            println!("bench paste no_tab {rel}");
            continue;
        };
        let plain = gdk::ContentProvider::for_value(&PLAIN.to_value());
        content.replace(Some(match html {
            true => gdk::ContentProvider::new_union(&[
                gdk::ContentProvider::for_bytes("text/html", &glib::Bytes::from(HTML.as_bytes())),
                plain,
            ]),
            false => plain,
        }));
        let buffer = &tab.buffer;
        buffer.place_cursor(&buffer.start_iter());
        glib::timeout_future(Duration::from_millis(300)).await;
        // From the label's middle to just past the end of the text, both on screen, where a
        // window sits at the screen's corner with no window manager.
        let screen = |widget: &gtk::Widget, root: &gtk::Window, x: f32, y: f32| {
            let (sx, sy) = root.surface_transform();
            (widget.compute_point(root, &graphene::Point::new(x, y)))
                .map(|p| (f64::from(p.x()) + sx, f64::from(p.y()) + sy))
        };
        let end = tab.view.iter_location(&buffer.end_iter());
        let (x, y) = tab.view.buffer_to_window_coords(
            gtk::TextWindowType::Widget,
            end.x() + 4,
            end.y() + end.height() / 2,
        );
        let from = screen(label.upcast_ref(), &source, 80.0, 24.0);
        let to = screen(
            tab.view.upcast_ref(),
            app.window.upcast_ref(),
            x as f32,
            y as f32,
        );
        let (Some((x0, y0)), Some((x1, y1))) = (from, to) else {
            println!("bench paste drop {rel} no_point");
            continue;
        };
        ended.set(None);
        let before = text(buffer);
        let steps = format!(
            "move {x0} {y0}; sleep 0.2; down; sleep 0.1; move {} {}; sleep 0.1; move {} {}; \
             sleep 0.3; move {x1} {y1}; sleep 0.5; move {} {y1}; sleep 0.5; up",
            x0 + 10.0,
            y0 + 10.0,
            (x0 + x1) / 2.0,
            (y0 + y1) / 2.0,
            x1 + 2.0,
        );
        super::git::xtest(&steps).await;
        until("paste", "the drop", 3000, || text(buffer) != before).await;
        let put = changed(&before, &text(buffer));
        let kind = if html { "html" } else { "plain" };
        println!(
            "bench paste drop {rel} {kind}={put:?} ended={:?}",
            ended.get()
        );
        if rel == "drop.md" && html {
            let all = text(buffer);
            buffer.undo();
            println!(
                "bench paste drop undo removed={}",
                all.len() - text(buffer).len() == put.len()
            );
        }
    }
    source.destroy();
}

/// What `paste` put into `buffer`, once it has changed, the text the paste replaced aside.
async fn pasted(buffer: &impl IsA<gtk::TextBuffer>, paste: impl Fn()) -> String {
    let before = text(buffer);
    paste();
    until("paste", "the paste", 3000, || text(buffer) != before).await;
    changed(&before, &text(buffer))
}

/// What is in `after` that was not in `before`, the text it replaced aside.
fn changed(before: &str, after: &str) -> String {
    let lead = (before.chars().zip(after.chars()))
        .take_while(|(a, b)| a == b)
        .map(|(a, _)| a.len_utf8())
        .sum::<usize>();
    let tail = (before[lead..]
        .chars()
        .rev()
        .zip(after[lead..].chars().rev()))
    .take_while(|(a, b)| a == b)
    .map(|(a, _)| a.len_utf8())
    .sum::<usize>();
    after[lead..after.len() - tail].to_string()
}

fn text(buffer: &impl IsA<gtk::TextBuffer>) -> String {
    buffer
        .text(&buffer.start_iter(), &buffer.end_iter(), true)
        .to_string()
}

/// The text tab of `rel` once it has opened, or `None` after five seconds.
async fn opened(app: &Rc<App>, rel: &str) -> Option<Rc<Tab>> {
    app.open_path(rel);
    until("paste", rel, 5000, || {
        app.open_tabs().into_iter().find(|t| t.rel() == rel)
    })
    .await
}
