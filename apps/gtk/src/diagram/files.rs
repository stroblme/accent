//! A diagram's pictures named by a file path (`image=pics/a.png`): read beside the diagram, held
//! to the vault (or to the diagram's own folder outside one) as a note's pictures are, a remote
//! vault's fetched from its host. draw.io's own sidebar pictures (`img/lib/…`) are not in a
//! vault, so they stay their boxes.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use accent_drawio::File;
use gtk::gio;
use gtk::prelude::*;

use crate::assets::{self, Resolve};

/// Whether `src`, an `image=` value, names a file rather than holding the picture or putting it
/// on the web.
pub fn is_file(src: &str) -> bool {
    !src.starts_with("data:") && !super::web::on_web(src)
}

/// Where the diagram `key` (a vault path, or an absolute one outside a vault) reads the pictures
/// it names by a path, each found once until its file changes.
#[derive(Clone)]
pub struct Files {
    resolve: Arc<Resolve>,
    key: Arc<str>,
    /// By the path as written: the file's key and its copy on this machine, `None` for one that
    /// is not there.
    found: Arc<Mutex<Found>>,
}

type Found = HashMap<String, Option<(String, PathBuf)>>;

impl Files {
    /// Read through the vault's `resolve` (`App::asset_resolver`).
    pub fn new(resolve: Arc<Resolve>, key: &str) -> Files {
        Files {
            resolve,
            key: key.into(),
            found: Arc::default(),
        }
    }

    /// The same vault's pictures for the diagram now at `key`.
    pub fn moved(&self, key: &str) -> Files {
        Files::new(self.resolve.clone(), key)
    }

    /// The diagram's key, which tells its pictures from another diagram's of the same name.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The type and bytes of the picture at `src`, `None` for one that is not there or not held
    /// to the vault. The first time, this waits on a remote vault's host: call it on a worker,
    /// or after [`Files::fetch`].
    pub fn read(&self, src: &str) -> Option<(String, Vec<u8>)> {
        let path = self.find(src)?;
        let bytes = std::fs::read(&path).ok()?;
        let kind = gio::content_type_guess(Some(&path), Some(bytes.as_slice())).0;
        let mime = gio::content_type_get_mime_type(&kind).map_or(kind.to_string(), |m| m.into());
        Some((mime, bytes))
    }

    /// Find on a worker each picture `file`'s cells name by a path, so an output drawn at once
    /// reads them without waiting on a host.
    pub async fn fetch(&self, file: &File) {
        let srcs = named(file);
        let files = self.clone();
        crate::work::off_thread("diagram pictures", move || {
            for src in &srcs {
                files.find(src);
            }
        })
        .await;
    }

    /// The pictures, by their paths as written, that the file `rel` may be now that it changed,
    /// came (`added`) or went: those found at it, or under it for a folder, and when it came
    /// those of its name, which may be found there now.
    pub fn naming(&self, rel: &str, added: bool) -> Vec<String> {
        let name = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
        let found = self.found();
        let named = found.iter().filter(|(src, found)| match found {
            Some((key, _)) if accent_core::path::is_under(key, rel) => true,
            _ => added && name(src) == name(rel),
        });
        named.map(|(src, _)| src.clone()).collect()
    }

    /// Find the pictures at `srcs` again the next time they are read.
    pub fn forget(&self, srcs: &[String]) {
        let mut found = self.found();
        for src in srcs {
            found.remove(src);
        }
    }

    fn find(&self, src: &str) -> Option<PathBuf> {
        if let Some(found) = self.found().get(src) {
            return found.as_ref().map(|(_, path)| path.clone());
        }
        let rel = beside(&self.key, src);
        let found = assets::resolve_asset(&*self.resolve, &self.key, &rel);
        let path = found.as_ref().map(|(_, path)| path.clone());
        self.found().insert(src.to_string(), found);
        path
    }

    fn found(&self) -> MutexGuard<'_, Found> {
        self.found.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// The pictures `file`'s cells name by a path, as written.
fn named(file: &File) -> Vec<String> {
    let cells = file.pages.iter().flat_map(|p| &p.cells);
    let srcs = cells.filter_map(|c| c.style.get("image").filter(|src| is_file(src)));
    srcs.map(str::to_string).collect()
}

/// The folders a diagram outside a vault reads its pictures from, watched while it is open, since
/// no vault's watcher sees them: each picture's path is told as it changes, comes (`true`) or
/// goes (`false`), as the vault's watcher tells a vault's.
pub struct Watch {
    told: Told,
    folders: BTreeSet<PathBuf>,
    monitors: Vec<gio::FileMonitor>,
}

/// Told of a picture's path and whether it came.
type Told = Rc<dyn Fn(&str, bool)>;

impl Watch {
    pub fn new(told: impl Fn(&str, bool) + 'static) -> Watch {
        Watch {
            told: Rc::new(told),
            folders: BTreeSet::new(),
            monitors: Vec::new(),
        }
    }

    /// Watch the folders `files` reads `file`'s pictures from, unless they are those watched.
    pub fn follow(&mut self, files: &Files, file: &File) {
        let folders: BTreeSet<PathBuf> = (named(file).iter())
            .filter_map(|src| {
                let path = PathBuf::from(beside(files.key(), src));
                path.parent()?.canonicalize().ok()
            })
            .collect();
        if folders != self.folders {
            self.monitors = folders.iter().filter_map(|dir| self.monitor(dir)).collect();
            self.folders = folders;
        }
    }

    /// A monitor on `dir`, canonical as a picture's key is, telling of each file in it.
    fn monitor(&self, dir: &Path) -> Option<gio::FileMonitor> {
        let monitor = gio::File::for_path(dir)
            .monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
            .ok()?;
        let told = self.told.clone();
        monitor.connect_changed(move |_, file, _, event| {
            // `ChangesDoneHint` is a settled write, as for a loose file's own monitor.
            let came = match event {
                gio::FileMonitorEvent::Created => true,
                gio::FileMonitorEvent::ChangesDoneHint | gio::FileMonitorEvent::Deleted => false,
                _ => return,
            };
            if let Some(path) = file.path() {
                told(&path.to_string_lossy(), came);
            }
        });
        Some(monitor)
    }
}

/// Where `src` is from the folder of the diagram `key`: a vault path, `..` stopping at the root
/// and a leading `/` meaning it, or for a diagram outside a vault the absolute path that
/// `assets::resolve_asset` holds to the diagram's folder.
fn beside(key: &str, src: &str) -> String {
    let dir = accent_core::path::parent_dir(key);
    match Path::new(key).is_absolute() {
        true => format!("{dir}/{src}"),
        false => accent_core::path::resolve(dir, src),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_picture_is_read_beside_its_diagram_and_never_past_the_vault() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault.path().join("d/pics")).unwrap();
        std::fs::write(vault.path().join("d/pics/a.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        std::fs::write(vault.path().join("top.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        // The vault's resolver, which keeps to its root.
        let root = vault.path().to_path_buf();
        let resolve: Arc<Resolve> = Arc::new(move |rel: &str| {
            Some((rel.to_string(), root.join(rel))).filter(|f| f.1.is_file())
        });
        let files = Files::new(resolve, "d/x.drawio");
        let mime = |src: &str| files.read(src).map(|(mime, _)| mime);
        assert_eq!(mime("pics/a.png").as_deref(), Some("image/png"));
        assert_eq!(mime("./pics/../pics/a.png").as_deref(), Some("image/png"));
        assert_eq!(mime("../top.png").as_deref(), Some("image/png"));
        // `..` past the root stops at it, and a draw.io sidebar picture is not in the vault.
        assert_eq!(beside("d/x.drawio", "../../../top.png"), "top.png");
        assert_eq!(mime("img/lib/clip_art/a.png"), None);
        // Outside a vault, the diagram's own folder is as far as it reads.
        let loose = vault.path().join("d/x.drawio");
        let files = Files::new(Arc::new(|_: &str| None), loose.to_str().unwrap());
        assert_eq!(
            files.read("pics/a.png").map(|r| r.0).as_deref(),
            Some("image/png")
        );
        assert_eq!(files.read("../top.png"), None);
    }

    #[test]
    fn a_picture_changed_came_or_went_is_named_by_its_file() {
        let vault = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(vault.path().join("d/pics")).unwrap();
        std::fs::write(vault.path().join("d/pics/a.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let root = vault.path().to_path_buf();
        let resolve: Arc<Resolve> = Arc::new(move |rel: &str| {
            Some((rel.to_string(), root.join(rel))).filter(|f| f.1.is_file())
        });
        let files = Files::new(resolve, "d/x.drawio");
        assert!(files.read("pics/a.png").is_some() && files.read("pics/b.png").is_none());
        let named = |rel, added| files.naming(rel, added);
        // A file changed or gone, or its folder gone, names the picture found at it.
        assert_eq!(named("d/pics/a.png", false), ["pics/a.png"]);
        assert_eq!(named("d/pics", false), ["pics/a.png"]);
        assert!(named("d/pics/b.png", false).is_empty() && named("d/a.png", false).is_empty());
        // One that came names those of its name, found or not, which may be found there now.
        assert_eq!(named("d/pics/b.png", true), ["pics/b.png"]);
        assert_eq!(named("elsewhere/a.png", true), ["pics/a.png"]);
        // Forgotten, it is found again: there now.
        std::fs::write(vault.path().join("d/pics/b.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(files.read("pics/b.png").is_none());
        files.forget(&named("d/pics/b.png", true));
        assert!(files.read("pics/b.png").is_some());
    }
}
