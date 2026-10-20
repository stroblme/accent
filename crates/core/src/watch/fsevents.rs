//! The watch set on macOS: one FSEvents stream over the vault, filtered to the directories asked for.
//!
//! notify's FSEvents backend stops and restarts its stream on every `watch` call, losing what was
//! in flight, so the root is watched recursively once and a directory under it is only recorded.
//! An event stands when its parent is recorded, which is what a non-recursive inotify watch on that
//! parent reports. FSEvents names a path by where it really is, so the record maps a linked-in
//! folder back to its vault name; and it flags a path with what happened to it over the last
//! moments, not just now, so the disk decides what an event still means.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use notify::event::{CreateKind, EventKind, ModifyKind};
use notify::{Config, Event, EventHandler, FsEventWatcher, RecursiveMode, WatcherKind};

/// Each watched directory's real path, and the name it was watched under.
type Dirs = Arc<Mutex<HashMap<PathBuf, PathBuf>>>;

pub(super) struct Native {
    stream: FsEventWatcher,
    dirs: Dirs,
    /// What the stream covers, recursively: the root, watched first, and any watched directory
    /// outside it (a linked-in folder's target, a repository's git directory above the vault).
    /// Kept for the watcher's life: the record alone decides what is reported.
    roots: Vec<PathBuf>,
}

impl notify::Watcher for Native {
    fn new<F: EventHandler>(mut handler: F, config: Config) -> notify::Result<Self> {
        let dirs = Dirs::default();
        let record = dirs.clone();
        let stream = FsEventWatcher::new(
            move |result: notify::Result<Event>| match result {
                Ok(ev) => {
                    let events = vault_events(ev, &record.lock().unwrap());
                    for ev in events {
                        handler.handle_event(Ok(ev));
                    }
                }
                Err(e) => handler.handle_event(Err(e)),
            },
            config,
        )?;
        Ok(Native {
            stream,
            dirs,
            roots: Vec::new(),
        })
    }

    fn watch(&mut self, path: &Path, _: RecursiveMode) -> notify::Result<()> {
        let real = path.canonicalize()?;
        if !self.roots.iter().any(|root| real.starts_with(root)) {
            self.stream.watch(&real, RecursiveMode::Recursive)?;
            self.roots.push(real.clone());
        }
        self.dirs.lock().unwrap().insert(real, path.to_path_buf());
        Ok(())
    }

    fn unwatch(&mut self, path: &Path) -> notify::Result<()> {
        let mut dirs = self.dirs.lock().unwrap();
        let before = dirs.len();
        dirs.retain(|_, named| named != path);
        match dirs.len() < before {
            true => Ok(()),
            false => Err(notify::Error::watch_not_found()),
        }
    }

    fn kind() -> WatcherKind {
        WatcherKind::Fsevent
    }
}

/// `ev` named as the vault names it, or nothing when its parent is not watched (the vault root
/// itself included, which inotify never reports either).
fn vault_events(ev: Event, dirs: &HashMap<PathBuf, PathBuf>) -> Vec<Event> {
    if ev.need_rescan() {
        return vec![ev];
    }
    let Some(path) = ev.paths.first() else {
        return Vec::new();
    };
    let Some(named) = (path.parent().and_then(|dir| dirs.get(dir)))
        .zip(path.file_name())
        .map(|(dir, name)| dir.join(name))
    else {
        return Vec::new();
    };
    let gone = named.symlink_metadata().is_err();
    let ev = Event {
        paths: vec![named.clone()],
        ..ev
    };
    match ev.kind {
        // A path that is gone carries its making and its edits as history: its removal is the news.
        EventKind::Create(_) | EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_))
            if gone =>
        {
            Vec::new()
        }
        // One still there was removed and made again, a folder's old contents gone with it. The
        // debouncer takes back a creation a removal follows, and FSEvents hands the creation it
        // flagged first, so the first removal may only cancel that: the second is the news.
        EventKind::Remove(_) if !gone => vec![
            ev.clone(),
            ev,
            Event::new(EventKind::Create(CreateKind::Any)).add_path(named),
        ],
        _ => vec![ev],
    }
}
