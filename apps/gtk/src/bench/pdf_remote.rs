//! PDF drills on a remote vault for conflicts, failed uploads, a link down, deletes and renames.

use super::pdf::{linked_note, links_read, online, opened, rebuild, vault_pages, written};
use super::*;

/// Press the button saying `label` on the newest toast that has one, as a click on it does.
fn press_toast(app: &Rc<App>, label: &str) -> bool {
    let button = find_widget(app.toasts.widget().upcast_ref(), &|w| {
        w.downcast_ref::<gtk::Button>()
            .is_some_and(|b| b.label().as_deref() == Some(label))
    });
    button
        .and_downcast::<gtk::Button>()
        .map(|b| b.emit_clicked())
        .is_some()
}

/// Check remote etag refusal preserves both host changes and local drawing: save into `<name>
/// (edited).pdf` and move the tab there. Stamp the cache with a nonexistent host etag to avoid
/// watcher/refetch races.
///
/// Add another page after refusal; it must update the same copy without numbering or another
/// toast. A fixture note links to original pages 1–3 and must remain unchanged.
pub(super) fn bench_pdf_stale(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let (Some(pdf), Some(vault)) = (opened(&app, &rel).await, app.vault().cloned()) else {
            println!("bench pdf stale no_tab");
            return bench_quit(&app);
        };
        let (key, Some(remote)) = (pdf.key(), vault.remote()) else {
            println!("bench pdf stale not_remote");
            return bench_quit(&app);
        };
        let note = linked_note(&app, &key).await;
        app.open_path(&key);
        let said = Cell::new(app.toasted.get());
        let stamp = accent_api::ssh::stamp_path(remote.url(), &key);
        // The stamp as the file spells one, written by hand because the app does not link
        // serde_json: the copy's own etag as it is, and a host's it will never report.
        let copy = accent_core::fs::Etag::of(&pdf.path());
        let moved = stamp.as_ref().zip(copy.ok()).map(|(stamp, copy)| {
            let etag = |e: accent_core::fs::Etag| {
                format!(
                    r#"{{"mtime_ns":{},"size":{},"ino":{}}}"#,
                    e.mtime_ns, e.size, e.ino
                )
            };
            let host = accent_core::fs::Etag {
                mtime_ns: 1,
                size: 1,
                ino: 1,
            };
            let stamped = format!(r#"{{"host":{},"copy":{}}}"#, etag(host), etag(copy));
            std::fs::write(stamp, stamped)
        });
        println!(
            "bench pdf stale opened pages={} in_vault {} moved={moved:?} {}",
            pdf.page_count(),
            vault_pages(&app, &key),
            links_read(&app, &note, &said)
        );
        let kept = pdf.path().with_extension("kept.pdf");
        let (first, second) = (
            accent_api::remote::edited_name(&key, 1),
            accent_api::remote::edited_name(&key, 2),
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        written(&app).await;
        println!(
            "bench pdf stale refused key={:?} pages={} in_vault {} edited {} said={} {:?} kept={} \
             {}",
            pdf.key(),
            pdf.page_count(),
            vault_pages(&app, &key),
            vault_pages(&app, &first),
            app.toasted.get(),
            bench_said(&app),
            kept.exists(),
            links_read(&app, &note, &said)
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        written(&app).await;
        println!(
            "bench pdf stale again key={:?} pages={} in_vault {} edited {} said={} numbered={} {}",
            pdf.key(),
            pdf.page_count(),
            vault_pages(&app, &key),
            vault_pages(&app, &first),
            app.toasted.get(),
            vault.exists(&second),
            links_read(&app, &note, &said)
        );
        let _ = std::fs::remove_file(&kept);
        bench_quit(&app);
    });
}

/// Check remote upload failures independently of etag conflicts. Make host file/folder
/// read-only, add two pages and report only the first failure. Restore permissions and press
/// Retry; both pages must upload without another stroke. A later failure is reported again.
///
/// If the host changes while local work is unsent, preserve it in `(edited)` and move the tab
/// there. Folder chmod also triggers indexing toasts; expected toast increments: +2, +0, +1, +2.
pub(super) fn bench_pdf_failed(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let (Some(pdf), Some(vault)) = (opened(&app, &rel).await, app.vault().cloned()) else {
            println!("bench pdf failed no_tab");
            return bench_quit(&app);
        };
        let (key, Some(remote)) = (pdf.key(), vault.remote().cloned()) else {
            println!("bench pdf failed not_remote");
            return bench_quit(&app);
        };
        let host = |mode: &str| chmod_on_host(&vault, &remote, &key, mode);
        let add = || WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        let edited = accent_api::remote::edited_name(&key, 1);
        let step = |name: &str| {
            println!(
                "bench pdf failed {name} key={:?} pages={} unsent={} in_vault {} edited {} said={} \
                 {:?}",
                pdf.key(),
                pdf.page_count(),
                pdf.unsent(),
                vault_pages(&app, &key),
                vault_pages(&app, &edited),
                app.toasted.get(),
                bench_said(&app)
            )
        };
        step("opened");
        println!("bench pdf failed read_only={}", host("a-w"));
        let _ = add();
        written(&app).await;
        step("refused");
        let _ = add();
        written(&app).await;
        step("again");
        println!("bench pdf failed writable={}", host("u+w"));
        println!(
            "bench pdf failed retry pressed={}",
            press_toast(&app, "Retry")
        );
        written(&app).await;
        step("retried");
        println!("bench pdf failed read_only={}", host("a-w"));
        let _ = add();
        written(&app).await;
        step("refused_after");
        println!("bench pdf failed writable={}", host("u+w"));
        let rebuilt = rebuild(&vault, &key);
        // The host's watcher reports the write, and the tab answers it.
        let started = Instant::now();
        while pdf.key() == key
            && pdf.page_count() != 1
            && started.elapsed() < Duration::from_secs(8)
        {
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        written(&app).await;
        println!("bench pdf failed rebuilt={rebuilt:?}");
        step("rebuilt");
        bench_quit(&app);
    });
}

/// `chmod <mode>` on the host, of the document `key` and its folder, behind the app's back.
fn chmod_on_host(
    vault: &accent_api::Vault,
    remote: &accent_api::remote::Remote,
    key: &str,
    mode: &str,
) -> bool {
    let path = vault.resolve(key).unwrap_or_default();
    let (file, dir) = (
        accent_api::ssh::quote(&path.to_string_lossy()),
        accent_api::ssh::quote(&path.parent().unwrap_or(&path).to_string_lossy()),
    );
    on_host(remote, &format!("chmod {mode} {file} {dir}"))
}

/// Run `command` on the host over the window's master: `true` when it succeeded.
fn on_host(remote: &accent_api::remote::Remote, command: &str) -> bool {
    let argv = accent_api::ssh::run(remote.url(), remote.control_path(), command);
    std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .is_ok_and(|s| s.success())
}

/// End the window's SSH master and hold the link down, with no automatic reconnect: `true` when
/// the master ended.
pub(super) async fn link_down(app: &Rc<App>, remote: &accent_api::remote::Remote) -> bool {
    let argv = accent_api::ssh::exit(remote.url(), remote.control_path());
    let ended = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .status()
        .is_ok_and(|s| s.success());
    // The banner is up once the window has heard; that is when its countdown can go.
    until("pdf", "link banner", 5000, || app.connection.is_revealed()).await;
    app.connection_refused("the drill holds the link down");
    ended
}

/// Drop the SSH master immediately after adding a page and suppress automatic reconnect. Saves
/// and link rewrites land offline without extra errors beyond the link banner. Reconnect Now
/// must upload and rewrite without another stroke. Use at least three pages; links `1,2,3,2,1`
/// become `1,3,4,3,1`.
///
/// Repeat while closing the tab and forgetting window edit state. Cached work survives; links
/// stay unchanged through reconnect and become `1,4,5,4,1` when reopening uploads the next
/// insertion.
pub(super) fn bench_pdf_dropped(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        // Opened first, which waits for the host; the note's tab then comes in front of it.
        let (Some(pdf), Some(vault)) = (opened(&app, &rel).await, app.vault().cloned()) else {
            println!("bench pdf dropped no_tab");
            return bench_quit(&app);
        };
        let (key, Some(remote)) = (pdf.key(), vault.remote().cloned()) else {
            println!("bench pdf dropped not_remote");
            return bench_quit(&app);
        };
        let note = linked_note(&app, &key).await;
        app.open_path(&key);
        let said = Cell::new(app.toasted.get());
        println!(
            "bench pdf dropped opened pages={} in_vault {} said={} {}",
            pdf.page_count(),
            vault_pages(&app, &key),
            app.toasted.get(),
            links_read(&app, &note, &said)
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        let ended = link_down(&app, &remote).await;
        written(&app).await;
        println!(
            "bench pdf dropped down ended={ended} offline={} pages={} unsent={} said={} {:?}",
            app.offline(),
            pdf.page_count(),
            pdf.unsent(),
            app.toasted.get(),
            bench_said(&app)
        );
        said.set(app.toasted.get());
        app.reconnect_now();
        for _ in 0..200 {
            if !app.offline() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        written(&app).await;
        println!(
            "bench pdf dropped back offline={} pages={} unsent={} in_vault {} said={} {}",
            app.offline(),
            pdf.page_count(),
            pdf.unsent(),
            vault_pages(&app, &key),
            app.toasted.get(),
            links_read(&app, &note, &said)
        );

        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        let ended = link_down(&app, &remote).await;
        written(&app).await;
        let page = pdf.page.clone();
        drop(pdf);
        app.close_page(&page);
        written(&app).await;
        let kept = accent_api::ssh::relink_path(remote.url(), &key).unwrap_or_default();
        app.relinks.borrow_mut().remove(&key);
        println!(
            "bench pdf dropped closed_down ended={ended} offline={} open={} kept={}",
            app.offline(),
            app.doc_for(&key).is_some(),
            kept.exists()
        );
        said.set(app.toasted.get());
        app.reconnect_now();
        online(&app).await;
        written(&app).await;
        println!(
            "bench pdf dropped closed_back offline={} in_vault {} kept={} {}",
            app.offline(),
            vault_pages(&app, &key),
            kept.exists(),
            links_read(&app, &note, &said)
        );
        let pdf = opened(&app, &key).await;
        written(&app).await;
        println!(
            "bench pdf dropped reopened pages={:?} in_vault {} kept={} {}",
            pdf.map(|pdf| pdf.page_count()),
            vault_pages(&app, &key),
            kept.exists(),
            links_read(&app, &note, &said)
        );
        bench_quit(&app);
    });
}

/// A drawn-on PDF closed at once: a page is added and the tab closed in the same turn, so the
/// write the close flushes lands after the tab has gone. It must still reach the vault, which on a
/// remote vault is an upload nobody is left to ask for. On a remote vault the same again with
/// the host's folder read-only, so the page cannot go: it must go up when the document is next
/// opened, the host taking writes again by then. Then once more with the link down: the page
/// and the link rewrite waiting on it (`kept`) must go once the link is back, the tab still
/// closed.
pub(super) fn bench_pdf_closed(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let Some(pdf) = opened(&app, &rel).await else {
            println!("bench pdf closed no_tab");
            return bench_quit(&app);
        };
        let (key, page) = (pdf.key(), pdf.page.clone());
        println!(
            "bench pdf closed opened pages={} in_vault {}",
            pdf.page_count(),
            vault_pages(&app, &key)
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        // The drill's own hold on the tab would keep it alive past the close.
        drop(pdf);
        app.close_page(&page);
        written(&app).await;
        println!(
            "bench pdf closed gone open={} in_vault {} said={} {:?}",
            app.doc_for(&key).is_some(),
            vault_pages(&app, &key),
            app.toasted.get(),
            bench_said(&app)
        );
        let Some(remote) = app.vault().and_then(|v| v.remote()).cloned() else {
            return bench_quit(&app);
        };
        let vault = app.vault().cloned().expect("a remote vault");
        let Some(pdf) = opened(&app, &key).await else {
            println!("bench pdf closed no_tab");
            return bench_quit(&app);
        };
        let page = pdf.page.clone();
        println!(
            "bench pdf closed read_only={}",
            chmod_on_host(&vault, &remote, &key, "a-w")
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        drop(pdf);
        app.close_page(&page);
        written(&app).await;
        println!(
            "bench pdf closed refused in_vault {} said={} {:?}",
            vault_pages(&app, &key),
            app.toasted.get(),
            bench_said(&app)
        );
        println!(
            "bench pdf closed writable={}",
            chmod_on_host(&vault, &remote, &key, "u+w")
        );
        let pdf = opened(&app, &key).await;
        written(&app).await;
        println!(
            "bench pdf closed reopened pages={:?} in_vault {} said={}",
            pdf.as_ref().map(|pdf| pdf.page_count()),
            vault_pages(&app, &key),
            app.toasted.get()
        );
        let Some(pdf) = pdf else {
            return bench_quit(&app);
        };
        let page = pdf.page.clone();
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        let ended = link_down(&app, &remote).await;
        drop(pdf);
        app.close_page(&page);
        written(&app).await;
        let kept = accent_api::ssh::relink_path(remote.url(), &key).unwrap_or_default();
        println!(
            "bench pdf closed closed_down ended={ended} offline={} open={} kept={}",
            app.offline(),
            app.doc_for(&key).is_some(),
            kept.exists()
        );
        app.reconnect_now();
        online(&app).await;
        written(&app).await;
        println!(
            "bench pdf closed closed_back offline={} open={} in_vault {} kept={} said={} {:?}",
            app.offline(),
            app.doc_for(&key).is_some(),
            vault_pages(&app, &key),
            kept.exists(),
            app.toasted.get(),
            bench_said(&app)
        );
        bench_quit(&app);
    });
}

/// A remote PDF deleted on the host while its cached copy holds a page the host refused (its
/// folder read-only, the tab closed): the copy goes with the file, and a toast says the page went
/// too. A PDF the host is then given under the name (a copy of `with`) opens as itself, not as
/// the drawn-on copy, and nothing goes up beside it as `(edited)`.
pub(super) fn bench_pdf_deleted(app: &Rc<App>, arg: &str) {
    let (app, arg) = (app.clone(), arg.to_string());
    glib::spawn_future_local(async move {
        let Some((rel, with)) = arg.split_once(',') else {
            println!("bench pdf deleted usage=deleted:<rel_path>,<replacement>");
            return bench_quit(&app);
        };
        let Some(pdf) = opened(&app, rel).await else {
            println!("bench pdf deleted no_tab");
            return bench_quit(&app);
        };
        let (Some(vault), Some(remote)) = (
            app.vault().cloned(),
            app.vault().and_then(|v| v.remote()).cloned(),
        ) else {
            println!("bench pdf deleted not_remote");
            return bench_quit(&app);
        };
        let (key, page) = (pdf.key(), pdf.page.clone());
        let read_only = chmod_on_host(&vault, &remote, &key, "a-w");
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        drop(pdf);
        app.close_page(&page);
        written(&app).await;
        let cache = accent_api::ssh::cache_path(remote.url(), &key).unwrap_or_default();
        println!(
            "bench pdf deleted refused read_only={read_only} cached={} {:?}",
            cache.exists(),
            bench_said(&app)
        );
        let on_disk = |rel: &str| {
            let path = vault.resolve(rel).unwrap_or_default();
            accent_api::ssh::quote(&path.to_string_lossy())
        };
        let deleted = chmod_on_host(&vault, &remote, &key, "u+w")
            && on_host(&remote, &format!("rm {}", on_disk(&key)));
        for _ in 0..100 {
            if !cache.exists() {
                break;
            }
            glib::timeout_future(Duration::from_millis(100)).await;
        }
        let said = find_widget(app.window.upcast_ref(), &|w| {
            (w.downcast_ref::<gtk::Label>())
                .is_some_and(|l| l.label().contains("before your changes reached it"))
        });
        println!(
            "bench pdf deleted removed deleted={deleted} cached={} said={:?}",
            cache.exists(),
            said.and_then(|w| Some(w.downcast::<gtk::Label>().ok()?.label().to_string()))
        );
        let replaced = on_host(&remote, &format!("cp {} {}", on_disk(with), on_disk(&key)));
        let pdf = opened(&app, &key).await;
        written(&app).await;
        println!(
            "bench pdf deleted reopened replaced={replaced} pages={:?} in_vault {} edited={}",
            pdf.map(|pdf| pdf.page_count()),
            vault_pages(&app, &key),
            vault.exists(&accent_api::remote::edited_name(&key, 1))
        );
        bench_quit(&app);
    });
}

/// A remote PDF renamed while its upload is still on the way: a page is added, and the file is
/// renamed two seconds later, which is inside the upload for a document of a few megabytes. The
/// upload must not bring the old name back on the host, and the page must end up in the renamed
/// file, not beside it as `(edited)`.
pub(super) fn bench_pdf_renaming(app: &Rc<App>, rel: &str) {
    let (app, rel) = (app.clone(), rel.to_string());
    glib::spawn_future_local(async move {
        let (Some(pdf), Some(vault), Some(ops)) = (
            opened(&app, &rel).await,
            app.vault().cloned(),
            app.ops().cloned(),
        ) else {
            println!("bench pdf renaming no_tab");
            return bench_quit(&app);
        };
        let from = pdf.key();
        let Some(to) = from
            .strip_suffix(".pdf")
            .map(|stem| format!("{stem}-renamed.pdf"))
        else {
            println!("bench pdf renaming not_a_pdf {from}");
            return bench_quit(&app);
        };
        println!(
            "bench pdf renaming opened pages={} in_vault {}",
            pdf.page_count(),
            vault_pages(&app, &from)
        );
        let _ = WidgetExt::activate_action(&app.window, "win.pdf-add-page-after", None);
        glib::timeout_future(Duration::from_secs(2)).await;
        crate::fileops::move_all(&ops, vec![(from.clone(), to.clone())]);
        glib::timeout_future(Duration::from_secs(20)).await;
        let edited = [&from, &to]
            .into_iter()
            .any(|key| vault.exists(&accent_api::remote::edited_name(key, 1)));
        println!(
            "bench pdf renaming settled key={:?} pages={} in_vault {} old_back={} edited={edited} \
             unsent={} said={}",
            pdf.key(),
            pdf.page_count(),
            vault_pages(&app, &to),
            vault.exists(&from),
            pdf.unsent(),
            app.toasted.get()
        );
        bench_quit(&app);
    });
}
