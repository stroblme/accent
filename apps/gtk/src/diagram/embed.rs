//! A diagram embedded in a note (`![[x.drawio]]`, `![[x.drawio#Page]]`): the page drawn as an
//! SVG by [`render`], its drawing alone in the file's own colours, which the preview then serves
//! as it serves an SVG image (`look::serve_svg`).
//!
//! Drawn on the main thread, where GTK and the typesetter live; the file is read on a worker. A
//! drawing is kept by the file and page until the file changes, so a render of the note, a theme
//! change and a second note embedding it draw nothing again.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use accent_drawio::File;

use super::render::{self, Area};
use crate::look::{self, Stamp};

/// An embed: the file on this machine and the page asked for.
type Embed = (PathBuf, Option<String>);

thread_local! {
    /// Every embed drawn, with the version of the file it was drawn from.
    static DRAWN: RefCell<HashMap<Embed, (Stamp, Rc<str>)>> = RefCell::default();
}

/// The diagram at `path` as an SVG of its page called `page`, its first without one; `None` for
/// a file that is no diagram or has no such page.
pub async fn svg(path: &Path, page: Option<&str>) -> Option<Rc<str>> {
    let stamp = look::stamp(path)?;
    let key = (path.to_path_buf(), page.map(str::to_string));
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
    let typesetter = math.then(super::math::shared);
    let bytes = render::svg(&file, i, Area::Drawing, typesetter.as_ref())
        .await
        .ok()?;
    let svg: Rc<str> = String::from_utf8(bytes).ok()?.into();
    DRAWN.with_borrow_mut(|drawn| drawn.insert(key, (stamp, svg.clone())));
    Some(svg)
}
