//! Keyboard and pointer drills through XTEST for box selections, occurrence carets and menus.

use super::*;

/// Box selection through the real pointer: a plain drag, which stays GTK's own selection, a
/// `Shift+Alt` drag, a `Shift+Alt` press that does not move, which leaves one caret there, and a
/// `Shift+Alt` drag resting a second below the view, which scrolls on and grows the box with it.
/// The file at `rel` is given a text of its own, and for each the drill prints
/// `bench box_ready <steps>` for `build-aux/xtest.py :N "<steps>"` to run, in the window's
/// coordinates, which under Xvfb are the screen's, then every caret and the primary selection. It
/// first prints `bench box focus_window`, as [`bench_occurrence_keys`] does.
pub(super) fn bench_box_drag(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        println!("bench box focus_window");
        until("box", "focus", 10_000, || app.window.is_active()).await;
        glib::timeout_future(Duration::from_millis(300)).await;
        let Some(tab) = app.active() else {
            println!("bench box no_tab");
            return bench_quit(&app);
        };
        let own = tab.text();
        tab.set_text("0123456789\n0\n0123\n\n0123456789\n");
        tab.view.grab_focus();
        // Laid out first, or the lines' heights are still estimates.
        glib::timeout_future(Duration::from_millis(300)).await;
        // The middle of the character at `column` of `line`'s first row, in the window.
        let aim = |line: i32, column: i32| {
            let buffer = &tab.buffer;
            let mut at = buffer.iter_at_line(line).unwrap_or(buffer.end_iter());
            at.set_line_offset(column);
            let rect = tab.view.iter_location(&at);
            let (x, y) = tab.view.buffer_to_window_coords(
                gtk::TextWindowType::Widget,
                rect.x() + 2,
                rect.y() + rect.height() / 2,
            );
            let point = graphene::Point::new(x as f32, y as f32);
            let point = tab.view.compute_point(&app.window, &point).unwrap_or(point);
            format!("{:.0} {:.0}", point.x(), point.y())
        };
        let chord = |steps: String| {
            format!("keydown Shift_L; keydown Alt_L; {steps}; keyup Alt_L; keyup Shift_L")
        };
        let parts = [
            ("plain", format!("drag {} {}", aim(0, 2), aim(4, 6))),
            ("box", chord(format!("drag {} {}", aim(0, 2), aim(4, 6)))),
            // Resting first: under Xvfb a press straight after a move reaches no widget, with or
            // without the chord.
            (
                "click",
                chord(format!("move {}; sleep 0.2; down; up", aim(2, 1))),
            ),
        ];
        for (step, steps) in parts {
            println!("bench box_ready {steps}");
            glib::timeout_future(Duration::from_millis(2500)).await;
            let carets = tab.ghost_view().map(|view| view.caret_positions());
            let selection = tab
                .buffer
                .selection_bounds()
                .map(|(start, end)| (start.offset(), end.offset()));
            println!("bench box {step} {carets:?} selection={selection:?}");
        }
        // Down to 40 px below the view and resting there: the box goes on growing as the view
        // scrolls, where it used to stop at the line the last motion reached, a screenful down.
        let lines: Vec<String> = (0..400).map(|i| format!("line {i}")).collect();
        tab.set_text(&lines.join("\n"));
        tab.buffer.place_cursor(&tab.buffer.start_iter());
        glib::timeout_future(Duration::from_millis(300)).await;
        let rect = tab.view.visible_rect();
        let screen = tab.view.line_at_y(rect.y() + rect.height()).0.line();
        let below = graphene::Point::new(20.0, (tab.view.height() + 40) as f32);
        let below = tab.view.compute_point(&app.window, &below).unwrap_or(below);
        let rest = format!(
            "move {}; sleep 0.2; down; move {}; sleep 0.1; move {:.0} {:.0}; sleep 1; up",
            aim(0, 2),
            aim(2, 2),
            below.x(),
            below.y()
        );
        println!("bench box_ready {}", chord(rest));
        glib::timeout_future(Duration::from_millis(3000)).await;
        let last = tab
            .ghost_view()
            .and_then(|view| view.caret_positions().last().copied())
            .map_or(0, |(line, _)| line);
        println!(
            "bench box rest screen={screen} last={last} scrolled_on={}",
            last > screen + 10
        );
        tab.set_text(&own);
        bench_quit(&app);
    });
}

/// Add Caret at Next Occurrence, Select All Occurrences and Add Caret Above / Below through the
/// real key path. The note at `rel` is given a text of its own for each part, and the drill prints
/// `bench occur_ready <chord>` for an XTEST press of each chord in turn
/// (`build-aux/xtest.py :N "key <chord>"`), then every caret's selection and the buffer. It first
/// prints `bench occur focus_window` and waits for the window to have the X input focus, which
/// under Xvfb is `xtest.py :N "move 700 400; focus"`.
///
/// Before any key it prints what claims the two chords: the application's accelerators, and the
/// view's own shortcuts, GtkTextView's and GtkSourceView's class bindings among them. `Tab::set_text`
/// leaves the tab clean and the note's own text goes back at the end, so nothing is written.
pub(super) fn bench_occurrence_keys(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        println!("bench occur focus_window");
        until("occur", "focus", 10_000, || app.window.is_active()).await;
        glib::timeout_future(Duration::from_millis(300)).await;
        let Some(tab) = app.active() else {
            return bench_quit(&app);
        };
        bench_occurrence_claims(&app, &tab);
        let own = tab.text();
        tab.view.clipboard().set_text("Q");
        let parts: [(&str, i32, &[&str]); 5] = [
            // Three presses: the word under the caret, then the two after it; then typing,
            // Backspace and Escape at every one of them.
            (
                "let foo = 1;\nfoo += foo;\nbar(fo);\n",
                5,
                &["alt+j", "alt+j", "alt+j", "x", "BackSpace", "y", "Escape"],
            ),
            // From the last occurrence, round to the top and on, and a fourth press with none left.
            (
                "foo 1\nfoo 2\nfoo 3\n",
                13,
                &["alt+j", "alt+j", "alt+j", "alt+j"],
            ),
            // Every occurrence at once, literally: the `foo` inside `foo_bar` too.
            (
                "a.foo b.foo\nfoo_bar foo\n",
                3,
                &["ctrl+shift+l", "z", "Escape"],
            ),
            // A paste lands at every one of them too.
            ("foo foo\nfoo\n", 1, &["alt+j", "alt+j", "ctrl+v", "Escape"]),
            // Add Caret Below twice, then Above takes both back and adds one above.
            (
                "a\nb\nc\nd\ne\n",
                4,
                &[
                    "shift+alt+Down",
                    "shift+alt+Down",
                    "shift+alt+Up",
                    "shift+alt+Up",
                    "shift+alt+Up",
                    "Escape",
                ],
            ),
        ];
        for (text, at, chords) in parts {
            tab.set_text(text);
            tab.buffer.place_cursor(&tab.buffer.iter_at_offset(at));
            tab.view.grab_focus();
            for chord in chords {
                glib::timeout_future(Duration::from_millis(200)).await;
                println!("bench occur_ready {chord}");
                glib::timeout_future(Duration::from_millis(800)).await;
                let selected: Vec<(i32, i32)> = tab
                    .ghost_view()
                    .map(|view| view.selections())
                    .unwrap_or_default()
                    .iter()
                    .map(|(start, end)| (start.offset(), end.offset()))
                    .collect();
                println!("bench occur {chord} {selected:?} {:?}", tab.text());
            }
        }
        tab.set_text(&own);
        bench_quit(&app);
    });
}

/// One step of [`bench_menu_caret`]: its name, the character to press on, and how the note is set
/// up for it.
type MenuStep<'a> = (&'static str, i32, Box<dyn Fn() + 'a>);

/// What a secondary click does to the caret and the selection before its menu opens, and what the
/// menu then acts on: the steps print `menu_ready <step> <x>,<y>` for an XTEST press there
/// (`xtest.py :N "move X Y; down 3; up 3; sleep 0.6; key Escape"`), and the state a second
/// later: the caret, the selection, how many carets, the spelling suggestions offered (none over
/// a selection of more than the word, and a correction then written over the word selected), and
/// after `copy` what `Ctrl+C` put on the clipboard. The note's own text goes back at the end; the
/// last step is a Ctrl+click on a wikilink, printing the tab it opened.
pub(super) fn bench_menu_caret(app: &Rc<App>, rel: &str) {
    app.open_path(rel);
    let app = app.clone();
    glib::spawn_future_local(async move {
        println!("bench menu focus_window");
        until("menu", "focus", 10_000, || app.window.is_active()).await;
        glib::timeout_future(Duration::from_millis(300)).await;
        let Some(tab) = app.active() else {
            return bench_quit(&app);
        };
        let own = tab.text();
        tab.set_text("first line here\nsecond line with a wrod\nthird line\nsee [[2024-01-02]]\n");
        tab.set_spellcheck(true);
        let buffer = tab.buffer.clone();
        let at = |offset: i32| buffer.iter_at_offset(offset);
        // The middle of the character at `offset`, in the window's coordinates.
        let aim = |offset: i32| {
            let rect = tab.view.iter_location(&at(offset));
            let (x, y) = tab.view.buffer_to_window_coords(
                gtk::TextWindowType::Widget,
                rect.x() + rect.width() / 2,
                rect.y() + rect.height() / 2,
            );
            tab.view
                .compute_point(&app.window, &graphene::Point::new(x as f32, y as f32))
                .map(|p| format!("{:.0},{:.0}", p.x(), p.y()))
                .unwrap_or_default()
        };
        let select = |from: i32, to: i32| buffer.select_range(&at(to), &at(from));
        let column = || tab.view.downcast_ref::<multicaret::View>().cloned();
        let steps: [MenuStep; 7] = [
            // On the selection: it stays, and so does the caret.
            ("inside", 2, Box::new(|| select(0, 5))),
            // Off it, on a misspelt word: the caret goes there and the menu is that word's.
            ("word", 37, Box::new(|| select(0, 5))),
            // On a selection of more than that word: no suggestions, they are one word's.
            ("words", 36, Box::new(|| select(33, 39))),
            // On the misspelt word selected: its suggestions, one going over the selection.
            ("selected", 37, Box::new(|| select(35, 39))),
            // With nothing selected, on another line: Copy is that line's.
            ("copy", 42, Box::new(|| buffer.place_cursor(&at(0)))),
            // A column of carets, clicked off every selection: one caret, at the click.
            (
                "column",
                47,
                Box::new(|| {
                    buffer.place_cursor(&at(2));
                    if let Some(column) = column() {
                        column.add_caret(true);
                    }
                }),
            ),
            // Ctrl+click on a wikilink, the primary button: Go to Definition.
            ("follow", 60, Box::new(|| buffer.place_cursor(&at(0)))),
        ];
        for (step, offset, set) in steps {
            set();
            tab.view.grab_focus();
            glib::timeout_future(Duration::from_millis(300)).await;
            println!("bench menu_ready {step} {}", aim(offset));
            glib::timeout_future(Duration::from_millis(1500)).await;
            let caret = buffer.iter_at_mark(&buffer.get_insert()).offset();
            let selected = buffer
                .selection_bounds()
                .map(|(start, end)| (start.offset(), end.offset()));
            let carets = column().map_or(1, |c| c.caret_positions().len());
            let spelling = tab
                .spell_menu()
                .map(|menu| crate::fileops::labels(&menu))
                .unwrap_or_default();
            let clip = match step {
                "copy" => tab.view.clipboard().read_text_future().await.ok().flatten(),
                _ => None,
            };
            println!(
                "bench menu {step} caret={caret} selected={selected:?} carets={carets} \
                 spelling={spelling:?} clipboard={clip:?} front={:?}",
                app.active().map(|t| t.rel())
            );
            if step == "selected" {
                let word = "word".to_variant();
                let done = tab.view.activate_action("spell.correct", Some(&word));
                let line = tab.text().lines().nth(1).map(str::to_string);
                println!("bench menu corrected {:?} {line:?}", done.is_ok());
            }
        }
        tab.set_text(&own);
        bench_quit(&app);
    });
}

/// Whatever else answers to `Alt+J` and `Ctrl+Shift+L`, and to `Ctrl+Shift+W`, which Outline Pane
/// took from `Ctrl+Shift+L`: the actions the application binds them to, and any shortcut on the
/// view itself.
fn bench_occurrence_claims(app: &Rc<App>, tab: &Rc<Tab>) {
    let chords = ["<Alt>j", "<Primary><Shift>l", "<Primary><Shift>w"];
    if let Some(gtk_app) = app.window.application() {
        for chord in chords {
            println!(
                "bench occur_claims app {chord} {:?}",
                gtk_app.actions_for_accel(chord)
            );
        }
    }
    // The class bindings are a shortcut controller of their own on every instance.
    let controllers = tab.view.observe_controllers();
    let triggers: Vec<String> = (0..controllers.n_items())
        .filter_map(|i| {
            controllers
                .item(i)
                .and_downcast::<gtk::ShortcutController>()
        })
        .flat_map(|controller| {
            (0..controller.n_items())
                .filter_map(|i| controller.item(i).and_downcast::<gtk::Shortcut>())
                .filter_map(|shortcut| shortcut.trigger())
                .map(|trigger| trigger.to_str().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    let claimed: Vec<&String> = triggers
        .iter()
        .filter(|trigger| {
            chords.iter().any(|chord| {
                gtk::accelerator_parse(*chord) == gtk::accelerator_parse(trigger.as_str())
            })
        })
        .collect();
    println!(
        "bench occur_claims view shortcuts={} claiming={claimed:?}",
        triggers.len()
    );
}
