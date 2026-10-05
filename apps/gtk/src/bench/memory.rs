//! A long session in miniature: every kind of tab opened and closed, comparisons, the preview,
//! shells and windows, each block repeated, printing what outlived its close and how the resident
//! size moved over the block.

use super::*;

/// `ACCENT_BENCH_MEMORY=<note>,<code>,<pdf>[,<rounds>]` first reads the PDF through, hides it,
/// brings it back and closes it, printing what the process holds untrimmed ([`read_through`];
/// point it at a few hundred pages). Then it runs each block `rounds` times (5 by default): a
/// note, a source file, a PDF and a diagram (a sample with a formula, written to
/// `memory.drawio`) opened and closed; the note compared with its disk copy and left; the note in
/// Split view and back; a shell opened and closed; a second window showing the note in Split
/// view, closed. After each block it prints `bench memory <block>` with the resident size in kB
/// before it and after each round (`rss=`), the same once malloc has handed its free pages back
/// (`trimmed=`), the threads by name, the child processes with their resident size, and what
/// outlived its close (`alive=`): the tab's `Rc`, its page and widget, a diagram's WebKit view, a
/// PDF's two views, a text tab's view and buffer, the comparison's other column, the window and
/// its `App`. Writes, so only on a scratch vault.
pub(super) fn bench_memory(app: &Rc<App>, arg: &str) {
    // The windows this opens run the hooks too.
    if app.vault().is_none() {
        return;
    }
    scratch_only(app, "ACCENT_BENCH_MEMORY");
    let parts: Vec<String> = arg.split(',').map(str::to_string).collect();
    let [note, code, pdf, ..] = parts.as_slice() else {
        eprintln!("ACCENT_BENCH_MEMORY=<note>,<code>,<pdf>[,<rounds>]");
        return bench_quit(app);
    };
    let (note, code, pdf) = (note.clone(), code.clone(), pdf.clone());
    let rounds: usize = parts.get(3).and_then(|n| n.parse().ok()).unwrap_or(5);
    let diagram = "memory.drawio".to_string();
    let _ = std::fs::write(app.root().join(&diagram), super::diagram::SAMPLE);
    let app = app.clone();
    glib::spawn_future_local(async move {
        let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
        // The watcher has to have seen the diagram, and the index the vault.
        wait(2000).await;
        println!("bench memory start rss={}", rss());
        // First, before any round below trims: what the process gives back by itself.
        read_through(&app, &pdf, &note).await;
        for (block, rel) in [
            ("note", &note),
            ("code", &code),
            ("pdf", &pdf),
            ("diagram", &diagram),
        ] {
            let mut block = Block::new(block);
            for _ in 0..rounds {
                let Some(doc) = open(&app, rel).await else {
                    println!("bench memory {} not_opened", block.name);
                    break;
                };
                let watch = watch(&doc);
                drop(doc);
                close(&app, rel);
                wait(1000).await;
                block.round(&watch);
            }
            block.report();
        }

        let mut block = Block::new("compare");
        for _ in 0..rounds {
            let Some(Doc::Text(tab)) = open(&app, &note).await else {
                break;
            };
            tab.buffer
                .insert(&mut tab.buffer.end_iter(), "\nchanged for the comparison\n");
            app.compare_with_disk(&tab);
            for _ in 0..40 {
                wait(100).await;
                if tab.comparison().is_some() {
                    break;
                }
            }
            wait(500).await;
            let mut watch = watch(&Doc::Text(tab.clone()));
            if let Some(view) = tab
                .comparison()
                .and_then(|c| compare::pane_view(c.widget(), true))
            {
                watch.push(object("column", &view));
            }
            tab.leave_compare();
            tab.discard();
            drop(tab);
            close(&app, &note);
            wait(1000).await;
            block.round(&watch);
        }
        block.report();

        let mut block = Block::new("preview");
        for _ in 0..rounds {
            let Some(doc) = open(&app, &note).await else {
                break;
            };
            app.set_mode(Mode::Split);
            wait(1500).await;
            app.set_mode(Mode::Editor);
            let watch = watch(&doc);
            drop(doc);
            close(&app, &note);
            wait(1000).await;
            block.round(&watch);
        }
        block.report();

        let mut block = Block::new("terminal");
        for _ in 0..rounds {
            app.open_terminal();
            wait(1500).await;
            let Some(term) = app.terminals().last().cloned() else {
                println!("bench memory terminal not_opened");
                break;
            };
            let (key, watch) = (term.key(), watch(&Doc::Terminal(term)));
            close(&app, &key);
            wait(1000).await;
            block.round(&watch);
        }
        block.report();

        let mut block = Block::new("window");
        let gtk_app = app.window.application().and_downcast::<adw::Application>();
        for _ in 0..rounds {
            let Some(other) =
                app.shell
                    .upgrade()
                    .zip(gtk_app.clone())
                    .and_then(|(shell, gtk_app)| {
                        shell.loose_window(&gtk_app, crate::shell::Loose::Documents)
                    })
            else {
                println!("bench memory window not_opened");
                break;
            };
            other.open_path(&app.root().join(&note).to_string_lossy());
            wait(800).await;
            // With its own preview, which is its own WebKit process.
            other.set_mode(Mode::Split);
            wait(1500).await;
            let watch = vec![object("window", &other.window), rc("app", &other)];
            other.window.close();
            drop(other);
            wait(1000).await;
            block.round(&watch);
        }
        block.report();
        bench_quit(&app);
    });
}

/// Open `rel` and wait for its tab, and a little more for what it loads after.
async fn open(app: &Rc<App>, rel: &str) -> Option<Doc> {
    app.open_path(rel);
    for _ in 0..30 {
        glib::timeout_future(Duration::from_millis(100)).await;
        if app.doc_for(rel).is_some() {
            break;
        }
    }
    glib::timeout_future(Duration::from_millis(800)).await;
    app.doc_for(rel)
}

fn close(app: &Rc<App>, key: &str) {
    if let Some(doc) = app.doc_for(key) {
        app.close_page(doc.page());
    }
}

/// `pdf` read from end to end, a view at a time with that view's tiles landed, then hidden behind
/// `note`, brought back and closed: `bench memory pdf-read` with [`heap`] once open, once read, a
/// second after it was hidden and two and a half after it closed.
async fn read_through(app: &Rc<App>, pdf: &str, note: &str) {
    let wait = |ms| glib::timeout_future(Duration::from_millis(ms));
    let Some(Doc::Pdf(tab)) = open(app, pdf).await else {
        return println!("bench memory pdf-read not_opened");
    };
    let (pages, opened) = (tab.page_count(), heap());
    for _ in 0..pages * 4 {
        if tab.current_page() + 1 >= pages {
            break;
        }
        tab.scroll_by(1.0);
        for _ in 0..100 {
            wait(20).await;
            if tab.unrendered().0.is_empty() {
                break;
            }
        }
    }
    let read = heap();
    open(app, note).await;
    wait(1000).await;
    let hidden = heap();
    // And back: the tiles that land for it, when the last of them did, and how many on screen
    // are still not sharp after a second.
    let _ = tab.tiles();
    app.open_path(pdf);
    let (t0, mut landed, mut last) = (Instant::now(), 0, 0);
    for _ in 0..50 {
        wait(20).await;
        let (_, _, rendered) = tab.tiles();
        if !rendered.is_empty() {
            (landed, last) = (landed + rendered.len(), ms_since(t0) as u64);
        }
    }
    let unsharp = tab.tiles().1;
    drop(tab);
    close(app, pdf);
    wait(2500).await;
    println!(
        "bench memory pdf-read pages={pages} opened=[{opened}] read=[{read}] hidden=[{hidden}] \
         back=[landed={landed} last_ms={last} unsharp={unsharp}] closed=[{}]",
        heap()
    );
    close(app, note);
}

/// Something that should be gone, and how to tell whether it still is not.
type Watch = Vec<(&'static str, Box<dyn Fn() -> bool>)>;

fn object(
    name: &'static str,
    object: &impl IsA<glib::Object>,
) -> (&'static str, Box<dyn Fn() -> bool>) {
    let weak = object.upcast_ref::<glib::Object>().downgrade();
    (name, Box::new(move || weak.upgrade().is_some()))
}

fn rc<T: 'static>(name: &'static str, rc: &Rc<T>) -> (&'static str, Box<dyn Fn() -> bool>) {
    let weak = Rc::downgrade(rc);
    (name, Box::new(move || weak.upgrade().is_some()))
}

/// What should be gone once `doc`'s tab has closed.
fn watch(doc: &Doc) -> Watch {
    let page = doc.page();
    let mut watch = vec![object("page", page), object("widget", &page.child())];
    watch.push(match doc {
        Doc::Text(tab) => rc("tab", tab),
        Doc::Image(v) | Doc::Status(v) => rc("tab", v),
        Doc::Pdf(pdf) => rc("tab", pdf),
        Doc::Diff(diff) => rc("tab", diff),
        Doc::Terminal(term) => rc("tab", term),
        Doc::Diagram(diagram) => rc("tab", diagram),
    });
    // A diagram's formulas are typeset by a WebKit view of its own.
    if let Some(web) = find_widget(&page.child(), &|w| w.is::<webkit6::WebView>()) {
        watch.push(object("webview", &web));
    }
    if let Doc::Pdf(pdf) = doc {
        watch.extend(pdf.views().iter().map(|view| object("pdfview", view)));
    }
    if let Doc::Text(tab) = doc {
        watch.push(object("view", &tab.view));
        watch.push(object("buffer", &tab.buffer));
    }
    watch
}

fn living(watch: &Watch) -> Vec<String> {
    watch
        .iter()
        .filter(|(_, alive)| alive())
        .map(|(name, _)| name.to_string())
        .collect()
}

/// One block's rounds: what outlived each close, and the resident size after each.
struct Block {
    name: &'static str,
    before: u64,
    after: Vec<u64>,
    trimmed: Vec<u64>,
    alive: Vec<String>,
}

impl Block {
    fn new(name: &'static str) -> Block {
        Block {
            name,
            before: rss(),
            after: Vec::new(),
            trimmed: Vec::new(),
            alive: Vec::new(),
        }
    }

    fn round(&mut self, watch: &Watch) {
        self.alive.extend(living(watch));
        self.after.push(rss());
        // SAFETY: glibc's own call, which only hands free pages back to the kernel.
        unsafe { malloc_trim(0) };
        self.trimmed.push(rss());
    }

    fn report(self) {
        let mut counts = std::collections::BTreeMap::new();
        for name in &self.alive {
            *counts.entry(name.as_str()).or_insert(0) += 1;
        }
        let alive: Vec<String> = counts.iter().map(|(k, n)| format!("{k}:{n}")).collect();
        let after: Vec<String> = self.after.iter().map(u64::to_string).collect();
        let trimmed: Vec<String> = self.trimmed.iter().map(u64::to_string).collect();
        println!(
            "bench memory {} rss={}->[{}] trimmed=[{}] threads=[{}] children=[{}] alive=[{}]",
            self.name,
            self.before,
            after.join(","),
            trimmed.join(","),
            threads(),
            children(),
            alive.join(",")
        );
    }
}

unsafe extern "C" {
    /// Hand the free pages of every malloc arena back to the kernel: what is left is what is
    /// still allocated, or pages it is scattered over.
    fn malloc_trim(pad: usize) -> i32;
}

/// This process's threads by name, with how many there are of each.
fn threads() -> String {
    let mut counts = std::collections::BTreeMap::new();
    for task in std::fs::read_dir("/proc/self/task")
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
        *counts.entry(name.trim().to_string()).or_insert(0) += 1;
    }
    let counts: Vec<String> = counts.iter().map(|(k, n)| format!("{k}:{n}")).collect();
    counts.join(",")
}

/// The processes under this one by name, with how many there are of each and their resident size
/// together in kB: WebKit's, the shells'.
fn children() -> String {
    let me = std::process::id();
    let mut parents = std::collections::HashMap::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let stat = std::fs::read_to_string(entry.path().join("stat")).unwrap_or_default();
        // `pid (comm) state ppid …`, where the name may hold spaces and parentheses.
        let Some((name, rest)) = stat.split_once(" (").and_then(|(_, r)| r.rsplit_once(") "))
        else {
            continue;
        };
        let ppid = rest
            .split(' ')
            .nth(1)
            .and_then(|p| p.parse::<u32>().ok())
            .unwrap_or(0);
        parents.insert(pid, (ppid, name.to_string()));
    }
    let under_me = |mut pid: u32| {
        while let Some((ppid, _)) = parents.get(&pid) {
            if *ppid == me {
                return true;
            }
            pid = *ppid;
        }
        false
    };
    let mut counts = std::collections::BTreeMap::new();
    for (pid, (_, name)) in &parents {
        if under_me(*pid) {
            let rss = status(&pid.to_string(), "VmRSS:");
            let entry = counts.entry(name.clone()).or_insert((0, 0));
            *entry = (entry.0 + 1, entry.1 + rss);
        }
    }
    let counts: Vec<String> = counts
        .iter()
        .map(|(k, (n, rss))| format!("{k}:{n}/{rss}"))
        .collect();
    counts.join(",")
}

/// The resident size in kB.
fn rss() -> u64 {
    status("self", "VmRSS:")
}

/// The resident size, and what malloc holds from the kernel and how much of it is handed out, in
/// kB: the gap between the last two is what it keeps for later rather than giving back.
fn heap() -> String {
    // SAFETY: glibc's own call, which only reads its arenas' counters.
    let m = unsafe { mallinfo2() };
    let (held, used) = (m.arena + m.hblkhd, m.uordblks + m.hblkhd);
    format!("rss={} held={} used={}", rss(), held >> 10, used >> 10)
}

/// glibc's `struct mallinfo2`, summed over every arena; only the counts [`heap`] reads are named.
#[repr(C)]
struct Mallinfo2 {
    /// Taken from the kernel by the arenas' heaps.
    arena: usize,
    _blocks: [usize; 3],
    /// Mapped on its own, one block each, and in use.
    hblkhd: usize,
    _unused: [usize; 2],
    /// Handed out of the arenas' heaps.
    uordblks: usize,
    _free: [usize; 2],
}

unsafe extern "C" {
    fn mallinfo2() -> Mallinfo2;
}

/// A number from `/proc/<pid>/status`, its unit left off.
fn status(pid: &str, field: &str) -> u64 {
    std::fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix(field))
                .and_then(|v| v.trim().trim_end_matches(" kB").trim().parse().ok())
        })
        .unwrap_or(0)
}
