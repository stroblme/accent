//! Render note diagram embeds (`![[x.drawio]]`, `![[x.drawio#Page]]`) as SVG drawing
//! bounds in file colours, then serve through `look::serve_svg`.
//!
//! Read on a worker and draw on the GTK main thread. Cache by file version and page
//! so note renders, theme changes and repeated embeds avoid redundant drawing.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use accent_drawio::File;

use super::render::{self, Area};
use crate::look::{self, Stamp};

/// An embed: the file on this machine, the page asked for, and whether its pictures on the web
/// are drawn.
type Embed = (PathBuf, Option<String>, bool);

thread_local! {
    /// Every embed drawn, with the version of the file it was drawn from.
    static DRAWN: RefCell<HashMap<Embed, (Stamp, Rc<str>)>> = RefCell::default();
}

/// The diagram at `path` as an SVG of its page called `page`, its first without one; `None` for
/// a file that is no diagram or has no such page. Where `web` (the reader said Load for the
/// diagram), its pictures on the web are downloaded first if they are not yet, and drawn.
pub async fn svg(path: &Path, page: Option<&str>, web: bool) -> Option<Rc<str>> {
    let stamp = look::stamp(path)?;
    let key = (path.to_path_buf(), page.map(str::to_string), web);
    let kept = DRAWN.with_borrow(|drawn| {
        drawn
            .get(&key)
            .filter(|(at, _)| *at == stamp)
            .map(|(_, svg)| svg.clone())
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
    let math = file.pages.get(i)?.model_attr("math") == Some("1");
    let typesetter = math.then(super::math::shared).flatten();
    if web {
        super::web::fetch(&super::web::urls(&file)).await;
    }
    let bytes = render::svg(&file, i, Area::Drawing, typesetter.as_ref(), web)
        .await
        .ok()?;
    let svg: Rc<str> = String::from_utf8(bytes).ok()?.into();
    DRAWN.with_borrow_mut(|drawn| drawn.insert(key, (stamp, svg.clone())));
    Some(svg)
}
