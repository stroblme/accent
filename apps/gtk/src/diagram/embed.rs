//! Render note diagram embeds (`![[x.drawio]]`, `![[x.drawio#Page]]`) as SVG drawing
//! bounds in file colours, which the preview recolours through `look::show_diagram` and an
//! export writes into the page.
//!
//! Read on a worker and draw on the GTK main thread in slices (`work::Slices`). Cache by file
//! version and page, until a picture the diagram names by a path changes, so note renders, theme
//! changes and repeated embeds avoid redundant drawing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use accent_drawio::File;

use super::files::Files;
use super::render::{self, Area, Pictures};
use crate::look::{self, Stamp};

/// An embed: the file on this machine, the page asked for, and whether its pictures on the web
/// are drawn.
type Embed = (PathBuf, Option<String>, bool);

/// An embed's drawing: the version of the file it was drawn from, its SVG, and where it found the
/// pictures it names by a path.
type Drawing = (Stamp, Rc<str>, Option<Files>);

thread_local! {
    /// Every embed drawn.
    static DRAWN: RefCell<HashMap<Embed, Drawing>> = RefCell::default();
}

/// The diagram at `path` as an SVG of its page called `page`, its first without one; `None` for
/// a file that is no diagram or has no such page. Where `web` (the reader said Load for the
/// diagram), its pictures on the web are downloaded first if they are not yet, and drawn, and
/// those it names by a path are read through `files`.
pub async fn svg(
    path: &Path,
    page: Option<&str>,
    web: bool,
    files: Option<Files>,
) -> Option<Rc<str>> {
    let stamp = look::stamp(path)?;
    let key = (path.to_path_buf(), page.map(str::to_string), web);
    let kept = DRAWN.with_borrow(|drawn| {
        drawn
            .get(&key)
            .filter(|(at, ..)| *at == stamp)
            .map(|(_, svg, _)| svg.clone())
    });
    if kept.is_some() {
        return kept;
    }
    let read = path.to_path_buf();
    let file = crate::work::off_thread("diagram embed", move || {
        File::from_bytes(&std::fs::read(read).ok()?).ok()
    })
    .await??;
    let i = match page {
        Some(name) => super::page_named(&file, name)?,
        None => 0,
    };
    if web {
        super::web::fetch(&super::web::urls(&file)).await;
    }
    if let Some(files) = &files {
        files.fetch(&file).await;
    }
    let pictures = Pictures {
        web,
        files: files.as_ref(),
    };
    let slices = crate::work::Slices::new();
    let bytes = render::svg(&file, i, Area::Drawing, pictures, &slices)
        .await
        .ok()?;
    let svg: Rc<str> = String::from_utf8(bytes).ok()?.into();
    DRAWN.with_borrow_mut(|drawn| drawn.insert(key, (stamp, svg.clone(), files)));
    Some(svg)
}

/// Let go of every embed drawn with a picture the file `rel` may be now that it changed, came
/// (`added`) or went (`Files::naming`): the keys of their diagrams, which the preview draws again.
pub fn forget(rel: &str, added: bool) -> Vec<String> {
    DRAWN.with_borrow_mut(|drawn| {
        let mut keys = Vec::new();
        drawn.retain(|_, (.., files)| {
            let named = files.as_ref().filter(|f| !f.naming(rel, added).is_empty());
            keys.extend(named.map(|f| f.key().to_string()));
            named.is_none()
        });
        keys
    })
}
