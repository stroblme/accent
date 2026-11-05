//! The preview's drills: what a note's page holds, and what rendering, patching and scrolling
//! it cost.

use super::*;
use accent_core::search::Options;

/// The note [`bench_preview`] writes into the vault: one of each thing the page draws.
const NOTE: &str = "preview-drill.md";

const FIXTURE: &str = r#"---
title: Preview drill
---

# Preview drill

A paragraph with *emphasis*, **strong**, ~~struck~~, `code`, a [link](https://example.org), a [[Other note#Part]], #drilltag and a note[^one]. ^para

## Lists

- first
  - nested
- second

1. one
2. two

- [ ] open task
- [x] done task
- [/] doing task

> A quote
> over two lines

> [!note] Callout title
> Callout body

```python
print("hi")
```

```mermaid
graph TD; A-->B
```

```search limit=5
kind:diagram
```

| Name | Value | Note |
|---|---|---|
| a | 1 | first |
| **b** | `2` | [second](https://example.org/2), [[Other note]] and #celltag |

---

Inline $x^2$ and a broken $\frac{$ formula.

$$
E = mc^2
$$

$$
a^2 + b^2 = c^2
$$

<div>Unwrapped <b>bold</b> div</div>

<!-- hidden comment -->

H<sub>2</sub>O and <kbd>Ctrl</kbd><br>next line

<details><summary>More</summary>
Inside details
</details>

<style>p { color: red }</style>

![[preview-drill.png]] ![[preview-drill.drawio]] ![[Attachments/stripes.png]]

Back to [the top](#preview-drill), [lists](#lists) and [the paragraph](#^para).

<<<<<<< ours
Our side
=======
Their side
>>>>>>> theirs

[^one]: The footnote.
"#;

/// `ACCENT_BENCH_PREVIEW=saved`: a note holding a saved search for a tag, its rows printed as
/// the vault first answers, after another note with the tag is written, and after the block's
/// query is made one that cannot be read; the note itself is never one of its rows. Between the
/// last two, its link to a search's results, as Android writes Show Results, is followed: the
/// Search pane's query and rows. Writes its notes: a scratch vault.
async fn saved(app: &Rc<App>) {
    const HOST: &str = "saved-drill.md";
    const LINK: &str = "accent://search/%23drillsaved%20-path%3Afirst";
    let write = |rel: &str, text: &str| std::fs::write(app.root().join(rel), text);
    let host =
        format!("# Saved\n#drillsaved\n\n```search\n#drillsaved\n```\n\n[Results]({LINK})\n");
    if let Err(e) = write("first.md", "# First\n#drillsaved one\n").and(write(HOST, &host)) {
        return println!("bench preview saved wrote=false {e}");
    }
    // Once the index has both.
    glib::timeout_future(Duration::from_millis(2500)).await;
    app.open_path(HOST);
    super::preview::beside(app).await;
    let Some(rendered) = settled(app).await else {
        return println!("bench preview saved none");
    };
    let print = |step: &str| {
        for (query, rows) in rendered
            .preview()
            .as_ref()
            .map(|p| p.saved())
            .unwrap_or_default()
        {
            println!("bench preview saved step={step} query={query:?} rows={rows:?}");
        }
    };
    glib::timeout_future(Duration::from_millis(1000)).await;
    print("first");
    // Another note with the tag: the block asks again once the vault has been still a second.
    if let Err(e) = write("second.md", "# Second\n#drillsaved two\n") {
        return println!("bench preview saved wrote=false {e}");
    }
    glib::timeout_future(Duration::from_millis(3500)).await;
    print("written");
    let followed = (rendered.preview().as_ref()).is_some_and(|p| p.follow_url(LINK));
    glib::timeout_future(Duration::from_millis(1500)).await;
    if let Some(sidebar) = app.sidebar.get() {
        let query = (sidebar.search_boxes()).map_or(String::new(), |[q, _]| q.text().into());
        let (_, rows, _) = sidebar.search_state();
        println!(
            "bench preview saved step=link followed={followed} showing={} query={query:?} \
             rows={rows:?}",
            sidebar.is_showing("search")
        );
    }
    // A kind misspelt, typed over the tag, which says why in place of the rows.
    if let Some(tab) = app.active() {
        let text = tab.text();
        let fence = "```search\n";
        if let Some(at) = text.find("```search\n#drillsaved") {
            let start = text[..at + fence.len()].chars().count() as i32;
            let end = start + "#drillsaved".chars().count() as i32;
            tab.buffer.begin_user_action();
            let (mut from, mut to) = (
                tab.buffer.iter_at_offset(start),
                tab.buffer.iter_at_offset(end),
            );
            tab.buffer.delete(&mut from, &mut to);
            tab.buffer
                .insert(&mut tab.buffer.iter_at_offset(start), "kind:pfd");
            tab.buffer.end_user_action();
        }
    }
    glib::timeout_future(Duration::from_millis(1500)).await;
    print("unread");
    if let Some(tab) = app.active() {
        tab.buffer.undo();
    }
}

/// `ACCENT_BENCH_PREVIEW=render`: writes [`NOTE`] and shows its
/// preview beside it, then prints each block's lines, the tags on its first character and its
/// text, an object as `[label]`; where `scroll_to_line` puts chosen lines; where each link goes;
/// what Copy writes for Select All; and the blocks kept and written after a one-character edit.
/// `apps/gtk/src/bench/preview.expected` holds the lines. `time:<rel>`: [`time`].
pub(super) fn bench_preview(app: &Rc<App>, arg: &str) {
    if arg == "saved" {
        scratch_only(app, "ACCENT_BENCH_PREVIEW=saved");
        let app = app.clone();
        glib::spawn_future_local(async move {
            saved(&app).await;
            bench_quit(&app);
        });
        return;
    }
    if let Some(rel) = arg.strip_prefix("time:") {
        let (app, rel) = (app.clone(), rel.to_string());
        glib::spawn_future_local(async move {
            time(&app, &rel).await;
            bench_quit(&app);
        });
        return;
    }
    let filler: String = (1..=40)
        .map(|i| format!("Filler paragraph {i}, there for the page to scroll.\n\n"))
        .collect();
    // The pictures the fixture embeds: a striped image, at the root and in a folder, and a
    // diagram's first page.
    let stripes: Vec<u8> = (0..96 * 48)
        .flat_map(|i| match i % 96 / 16 % 2 {
            0 => [53u8, 132, 228],
            _ => [246, 211, 45],
        })
        .collect();
    let png = gdk::MemoryTexture::new(
        96,
        48,
        gdk::MemoryFormat::R8g8b8,
        &glib::Bytes::from_owned(stripes),
        96 * 3,
    )
    .save_to_png_bytes();
    let pictures = std::fs::write(app.root().join("preview-drill.png"), &png)
        .and_then(|_| std::fs::create_dir_all(app.root().join("Attachments")))
        .and_then(|_| std::fs::write(app.root().join("Attachments/stripes.png"), &png))
        .and_then(|_| {
            std::fs::write(
                app.root().join("preview-drill.drawio"),
                super::diagram::SAMPLE,
            )
        });
    if let Err(e) =
        pictures.and_then(|_| std::fs::write(app.root().join(NOTE), format!("{FIXTURE}\n{filler}")))
    {
        println!("bench preview wrote=false {e}");
        return bench_quit(app);
    }
    let app = app.clone();
    glib::spawn_future_local(async move {
        // Once the index has the picture in a folder, whose news would otherwise read it again
        // under the page.
        let t = Instant::now();
        while (app.vault()).is_none_or(|v| v.asset("stripes.png").is_none())
            && t.elapsed() < Duration::from_secs(10)
        {
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        app.open_path(NOTE);
        render(&app).await;
        bench_quit(&app);
    });
}

/// The preview tab's page, once its render is in.
async fn settled(app: &Rc<App>) -> Option<Rc<crate::rendered::Rendered>> {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if let Some(rendered) = super::preview::shown(app) {
            let ready = rendered
                .preview()
                .as_ref()
                .is_some_and(|n| n.settled() && n.text_length() > 0);
            if ready {
                return Some(rendered);
            }
        }
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    None
}

async fn render(app: &Rc<App>) {
    super::preview::beside(app).await;
    let (Some(rendered), Some(tab)) = (settled(app).await, app.active()) else {
        return println!("bench preview none");
    };
    glib::timeout_future(Duration::from_millis(300)).await;
    // The pictures read: a picture's size is the file's, up to the text's width.
    let reading = || {
        rendered
            .preview()
            .as_ref()
            .is_some_and(|n| n.figures().0.iter().any(|f| f.pending))
    };
    let t = Instant::now();
    while reading() && t.elapsed() < Duration::from_secs(5) {
        glib::timeout_future(Duration::from_millis(20)).await;
    }
    let short = |s: &str| s.chars().take(90).collect::<String>();
    {
        let preview = rendered.preview();
        let Some(preview) = preview.as_ref() else {
            return;
        };
        // The fixture's blocks, not the filler under them. A code look's number counts the looks
        // met before it, in whichever order the editor and the preview highlighted their blocks.
        for (i, (lines, text, tags)) in preview.blocks().into_iter().take(26).enumerate() {
            let tags: Vec<&str> = (tags.iter())
                .map(|t| match t.rsplit_once(':') {
                    Some((look, _)) if t.starts_with("code:") => look,
                    _ => t,
                })
                .collect();
            println!(
                "bench preview block={i} lines={}-{} tags={tags:?} text={:?}",
                lines.start,
                lines.end,
                short(&text)
            );
        }
        let (figures, reads) = preview.figures();
        // Its own size and whether it is drawn at that shrunk to the text's width: the width
        // itself is the pane's, which differs from run to run, and a diagram's takes in its
        // labels as the fonts at hand lay them out.
        for f in figures {
            let size = if f.uri.contains(".drawio") {
                "fonts".to_string()
            } else {
                format!("{}x{}", f.natural.0, f.natural.1)
            };
            println!(
                "bench preview figure {} key={:?} loaded={} size={size} fits={} reads={reads}",
                f.uri, f.key, f.loaded, f.fits
            );
        }
        for (tex, display, size) in preview.formulas() {
            println!("bench preview formula {tex:?} display={display} size={size:.2}");
        }
        for (text, route) in preview.links() {
            println!("bench preview link {text:?} {route}");
        }
        for (query, rows) in preview.saved() {
            println!("bench preview saved {query:?} rows={rows:?}");
        }
        let copied = preview.copy_all();
        for line in copied.lines().take_while(|l| !l.starts_with("Filler")) {
            println!("bench preview copy {line:?}");
        }
        for (markup, ..) in preview.cells() {
            println!("bench preview cell {markup:?}");
        }
    }
    // Where a line of the note puts the page: the heading, a nested item, inside the code, the
    // table, the conflict's incoming side, and the first line.
    let text = tab.text();
    let line_of = |needle: &str| {
        text.lines()
            .position(|l| l.contains(needle))
            .map(|i| i as u32 + 1)
    };
    for needle in [
        "## Lists",
        "  - nested",
        "print(",
        "| **b**",
        "Their side",
        "---",
    ] {
        let Some(line) = line_of(needle) else {
            continue;
        };
        rendered.scroll_to_line(line);
        glib::timeout_future(Duration::from_millis(300)).await;
        let top = rendered
            .preview()
            .as_ref()
            .map(|n| short(&n.top_line()))
            .unwrap_or_default();
        println!("bench preview scroll line={line} top={top:?}");
    }
    // One character typed into the quote: one block written again.
    if let Some(at) = text.find("A quote") {
        let at = text[..at].chars().count() as i32 + 1;
        let mut iter = tab.buffer.iter_at_offset(at);
        tab.buffer.insert(&mut iter, "x");
        glib::timeout_future(Duration::from_millis(1500)).await;
        let patched = rendered.preview().as_ref().map(|n| n.patched());
        if let Some((kept, written)) = patched {
            println!("bench preview edit kept={kept} written={written}");
        }
        tab.buffer.undo();
    }
    glib::timeout_future(Duration::from_millis(1500)).await;
    // The fences coloured as the editor colours them: the text under each code tag.
    if let Some(preview) = rendered.preview().as_ref() {
        let coloured = tagged(&preview.buffer(), |n| n.starts_with("code:"));
        println!("bench preview code coloured={coloured:?}");
    }
    find(&rendered, &tab).await;
    busy(&rendered).await;
    narrow(&rendered).await;
}

/// A fence no editor has coloured, rendered while the main loop never goes idle below
/// `DEFAULT_IDLE`, as a busy window's watchers and terminals keep it: coloured all the same.
async fn busy(rendered: &crate::rendered::Rendered) {
    let spin = glib::idle_add_local(|| glib::ControlFlow::Continue);
    if let Some(preview) = rendered.preview().as_ref() {
        preview.render("busy.md", "```rust\nfn busy() -> u8 { 1 }\n```\n");
    }
    glib::timeout_future(Duration::from_millis(1000)).await;
    spin.remove();
    if let Some(preview) = rendered.preview().as_ref() {
        let coloured = tagged(&preview.buffer(), |n| n.starts_with("code:"));
        println!("bench preview code busy coloured={coloured:?}");
    }
}

/// A table narrower than the page: whether its rules end at its last column, the top one too.
async fn narrow(rendered: &crate::rendered::Rendered) {
    let Some(root) = rendered.preview().as_ref().map(|p| p.widget().clone()) else {
        return;
    };
    if let Some(preview) = rendered.preview().as_ref() {
        preview.render("narrow.md", "| a | b |\n|---|---|\n| 1 | 2 |\n");
    }
    glib::timeout_future(Duration::from_millis(500)).await;
    let mut widgets = vec![root];
    while let Some(widget) = widgets.pop() {
        let mut child = widget.first_child();
        while let Some(c) = child {
            child = c.next_sibling();
            widgets.push(c);
        }
        if !widget.has_css_class("accent-table") {
            continue;
        }
        let right = |w: &gtk::Widget| w.compute_bounds(&widget).map_or(0.0, |b| b.x() + b.width());
        let mut last = 0.0f32;
        let mut cell = widget.first_child();
        while let Some(c) = cell {
            last = last.max(right(&c));
            cell = c.next_sibling();
        }
        // Within the last column's rule, which a cell's box overhangs by its padding's fraction
        // of a pixel (ISSUES.md).
        let ends = (right(&widget) - last).abs() <= 1.0;
        println!("bench preview table rules end at its last column={ends}");
    }
}

/// Find from the page's top: the readout after a search, a step each way and Match Whole Word,
/// what the search selected and how many matches it painted; then the readout and the selection
/// after a key typed into the block of the match selected, which a render writes anew.
async fn find(rendered: &crate::rendered::Rendered, tab: &Tab) {
    rendered.scroll_to_line(1);
    glib::timeout_future(Duration::from_millis(300)).await;
    let said = Rc::new(RefCell::new(String::new()));
    let buffer = {
        let preview = rendered.preview();
        let Some(preview) = preview.as_ref() else {
            return;
        };
        preview.connect_found(glib::clone!(
            #[strong]
            said,
            move |label| *said.borrow_mut() = label.to_string()
        ));
        let buffer = preview.buffer();
        let mut readouts = Vec::new();
        preview.find("side", Options::default());
        readouts.push(said.borrow().clone());
        let (selected, painted) = (selected(&buffer), tagged(&buffer, |n| n == "find").len());
        preview.find_next();
        readouts.push(said.borrow().clone());
        preview.find_previous();
        readouts.push(said.borrow().clone());
        let word = Options {
            word: true,
            ..Options::default()
        };
        preview.find("side", word);
        readouts.push(said.borrow().clone());
        println!("bench preview find {readouts:?} selected={selected:?} painted={painted}");
        buffer
    };
    let text = tab.text();
    if let Some(at) = text.find("Our side") {
        let mut iter = tab.buffer.iter_at_offset(text[..at].chars().count() as i32);
        tab.buffer.insert(&mut iter, "x");
        glib::timeout_future(Duration::from_millis(1500)).await;
        println!(
            "bench preview find typed {:?} selected={:?}",
            said.borrow(),
            selected(&buffer)
        );
        tab.buffer.undo();
        glib::timeout_future(Duration::from_millis(1500)).await;
    }
    // A word in a list item and in a table's cell: the cell's match counted where the table
    // stands, selected and painted in its label.
    let preview = rendered.preview();
    let Some(preview) = preview.as_ref() else {
        return;
    };
    let mut readouts = Vec::new();
    preview.find("second", Options::default());
    readouts.push(said.borrow().clone());
    preview.find_next();
    readouts.push(said.borrow().clone());
    let cells = preview.cells();
    let cell = cells.iter().find(|(_, selected, _)| !selected.is_empty());
    let painted = cells.iter().filter(|(.., painted)| *painted).count();
    preview.find_next();
    readouts.push(said.borrow().clone());
    println!(
        "bench preview find cells {readouts:?} selected={:?} painted={painted} page={:?}",
        cell.map(|c| &c.1),
        selected(&buffer)
    );
}

/// What `buffer` has selected.
fn selected(buffer: &gtk::TextBuffer) -> String {
    let selection = buffer.selection_bounds();
    selection.map_or_else(String::new, |(s, e)| buffer.slice(&s, &e, true).to_string())
}

/// The text of each run of `buffer` under a tag whose name `named` takes.
fn tagged(buffer: &gtk::TextBuffer, named: impl Fn(&str) -> bool) -> Vec<String> {
    let (mut out, mut start, mut at) = (Vec::new(), None, buffer.start_iter());
    loop {
        let on = (at.tags().iter()).any(|t| t.name().is_some_and(|n| named(&n)));
        match (on, start) {
            (true, None) => start = Some(at),
            (false, Some(s)) => {
                out.push(buffer.slice(&s, &at, true).to_string());
                start = None;
            }
            _ => {}
        }
        if at.is_end() {
            return out;
        }
        at.forward_to_tag_toggle(None::<&gtk::TextTag>);
    }
}

/// The longest the main loop went without running a 1 ms timeout while the guard lives, and how
/// long it was busy in all: every gap past 2 ms, less the 1 ms the timeout waits anyway.
struct Beat {
    worst: Rc<Cell<f64>>,
    busy: Rc<Cell<f64>>,
    source: Option<glib::SourceId>,
}

impl Beat {
    fn start() -> Beat {
        let (worst, busy) = (Rc::new(Cell::new(0.0)), Rc::new(Cell::new(0.0)));
        let last = Cell::new(Instant::now());
        let source = glib::timeout_add_local(Duration::from_millis(1), {
            let (worst, busy) = (worst.clone(), busy.clone());
            move || {
                let gap = ms_since(last.replace(Instant::now()));
                worst.set(f64::max(worst.get(), gap));
                if gap > 2.0 {
                    busy.set(busy.get() + gap - 1.0);
                }
                glib::ControlFlow::Continue
            }
        });
        Beat {
            worst,
            busy,
            source: Some(source),
        }
    }

    /// The worst stall and the busy total, in ms.
    fn stop(mut self) -> (f64, f64) {
        if let Some(source) = self.source.take() {
            source.remove();
        }
        (self.worst.get(), self.busy.get())
    }
}

/// The main thread's CPU time so far and its time waiting for a CPU, in ms, from
/// `/proc/thread-self/schedstat`: what the drill did, told apart from other processes' load.
fn schedstat() -> (f64, f64) {
    let stat = std::fs::read_to_string("/proc/thread-self/schedstat").unwrap_or_default();
    let mut ms = stat
        .split_whitespace()
        .map(|v| v.parse().unwrap_or(0.0) / 1e6);
    (ms.next().unwrap_or(0.0), ms.next().unwrap_or(0.0))
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values.get(values.len() / 2).copied().unwrap_or(0.0)
}

/// `ACCENT_BENCH_PREVIEW=time:<rel>`: the note's page put up whole, then twelve one-character
/// edits around its middle, then scrolled top to bottom in 40 steps. Prints the worker's
/// `page_ms`, the main thread's busy time and worst stall for the first render (`first_ms`,
/// `stall_ms`), the median busy time and worst stall of a patch, the worst stall scrolling, and
/// the main thread's CPU and its wait for a CPU (on a loaded machine the stalls mean little).
/// Stage 2's budget (plan §3.10): a patch ≤16 ms on the 207 KB bench note, ≤30 ms on the 1 MB
/// one; scrolling the 1 MB one, no stall over 50 ms.
async fn time(app: &Rc<App>, rel: &str) {
    for _ in 0..600 {
        if app.reconciled.get() {
            break;
        }
        glib::timeout_future(Duration::from_millis(100)).await;
    }
    let Ok(text) = std::fs::read_to_string(app.root().join(rel)) else {
        return println!("bench preview time no_note {rel}");
    };
    app.open_path(rel);
    super::preview::beside(app).await;
    let Some(rendered) = settled(app).await else {
        return println!("bench preview time none");
    };
    let preview = || std::cell::Ref::filter_map(rendered.preview(), Option::as_ref).ok();
    let landed = || async {
        let t = Instant::now();
        while !preview().is_some_and(|n| n.settled()) && t.elapsed() < Duration::from_secs(30) {
            glib::timeout_future(Duration::from_millis(1)).await;
        }
    };
    let page_ms = {
        let (rel, text) = (rel.to_string(), text.clone());
        let t = Instant::now();
        crate::work::off_thread("page", move || {
            accent_core::note::Syntax::of(&rel).page(&text).len()
        })
        .await;
        ms_since(t)
    };
    // Another note first, so the note goes in whole; and time for the note's editor tab to have
    // measured and coloured its own text, which shares the main thread.
    if let Some(n) = preview() {
        n.render("other.md", "");
    }
    landed().await;
    glib::timeout_future(Duration::from_secs(5)).await;
    let (beat, sched) = (Beat::start(), schedstat());
    if let Some(n) = preview() {
        n.render(rel, &text);
    }
    landed().await;
    glib::timeout_future(Duration::from_millis(300)).await;
    let (stall, first) = beat.stop();
    let (first_cpu, first_wait) = (schedstat().0 - sched.0, schedstat().1 - sched.1);
    let blocks = preview().map_or(0, |n| n.blocks().len());

    let mut edited = text.clone();
    let (mut patches, mut patch_stall) = (Vec::new(), 0.0f64);
    for k in 0..12 {
        let wanted = edited.len() * (40 + 2 * k) / 100;
        let Some(at) = (wanted..edited.len())
            .find(|&i| edited.is_char_boundary(i) && edited.as_bytes()[i].is_ascii_lowercase())
        else {
            continue;
        };
        edited.insert(at, 'x');
        let line = edited[..at].matches('\n').count() as u32 + 1;
        if let Some(n) = preview() {
            n.scroll_to_line(line);
        }
        glib::timeout_future(Duration::from_millis(300)).await;
        let beat = Beat::start();
        if let Some(n) = preview() {
            n.render(rel, &edited);
        }
        landed().await;
        glib::timeout_future(Duration::from_millis(300)).await;
        let (worst, busy) = beat.stop();
        patches.push(busy);
        patch_stall = patch_stall.max(worst);
    }

    let vadj = rendered.preview().as_ref().and_then(|p| {
        let widget = p.focus_target().clone();
        widget.downcast::<gtk::TextView>().ok()?.vadjustment()
    });
    let mut scroll_stall = 0.0;
    if let Some(vadj) = vadj {
        vadj.set_value(0.0);
        glib::timeout_future(Duration::from_millis(500)).await;
        let beat = Beat::start();
        for i in 1..=40 {
            vadj.set_value((vadj.upper() - vadj.page_size()) * f64::from(i) / 40.0);
            glib::timeout_future(Duration::from_millis(50)).await;
        }
        scroll_stall = beat.stop().0;
    }
    println!(
        "bench preview time bytes={} blocks={blocks} page_ms={page_ms:.1} first_ms={first:.0} \
         stall_ms={stall:.1} patch_ms={:.1} patch_stall_ms={patch_stall:.1} \
         scroll_stall_ms={scroll_stall:.1} first_cpu_ms={first_cpu:.0} \
         first_waited_for_cpu_ms={first_wait:.0}",
        text.len(),
        median(patches),
    );
}
