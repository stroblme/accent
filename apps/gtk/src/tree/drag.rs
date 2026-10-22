//! Tree drops move internal rows, import external files or append PDFs
//! to PDF rows. Hovering over a collapsed folder opens it after a delay.

use super::*;

/// Use GtkStringObject payloads for tree moves, alongside pane-opening strings.
///
/// GtkDropTarget matches GTypes; MIME payloads would require asynchronous stream handling.
/// Panes accept only String and AdwTabPage, so folder-only move payloads cannot open panes.
pub(super) fn move_content(rel: &str) -> gdk::ContentProvider {
    gdk::ContentProvider::for_value(&gtk::StringObject::new(rel).to_value())
}

/// What a drag out of the tree hands another application: the files themselves, a `GdkFileList`
/// GTK writes out as `text/uri-list`. `root` is the vault's folder on this machine, `None` on a
/// remote vault, whose files have no path here until they are downloaded. [`import_target`]
/// never takes a drag that began in accent, so inside accent a row stays a row.
pub(super) fn files_content(root: Option<&Path>, rels: &[String]) -> Option<gdk::ContentProvider> {
    let root = root?;
    let files: Vec<gio::File> = rels
        .iter()
        .map(|rel| gio::File::for_path(root.join(rel)))
        .collect();
    let list = gdk::FileList::from_array(&files);
    Some(gdk::ContentProvider::for_value(&list.to_value()))
}

/// What a drop is handed to: each path a drag carried, and the path it goes to.
pub(super) type Move = Rc<dyn Fn(Vec<(String, String)>)>;

/// PDF append callback: dropped vault-relative or external absolute path
/// and target row path.
pub(super) type Append = Rc<dyn Fn(String, String)>;

/// Rows dragged in from another window's tree, which name files in that window's vault: whether
/// the drag in flight is such a one, and what its drop is handed — the paths, the folder they go
/// into, and whether Shift asked for a move.
#[derive(Clone)]
pub(super) struct Carry {
    pub foreign: Rc<dyn Fn() -> bool>,
    pub drop: Rc<dyn Fn(Vec<String>, String, bool)>,
}

/// Find one carried PDF and a target PDF row for page append. Reject
/// self-drops as ordinary no-op moves.
fn appending(target: &gtk::DropTarget, carried: &[String]) -> Option<(String, String)> {
    let row = target_row(target)?.item().as_ref().and_then(decode)?;
    let pdf = |rel: &str| crate::doc::kind_of(rel) == crate::doc::Kind::Pdf;
    let onto = !row.dependency && !row.is_dir() && pdf(&row.rel);
    match carried {
        [one] if onto && pdf(one) && *one != row.rel => Some((one.clone(), row.rel)),
        _ => None,
    }
}

/// Tree-move paths: one row or the marked set as `GtkStringList`, which
/// panes refuse because it identifies no single file to open.
fn dragged(value: &glib::Value) -> Vec<String> {
    if let Ok(one) = value.get::<gtk::StringObject>() {
        return vec![one.string().to_string()];
    }
    value.get::<gtk::StringList>().map_or_else(
        |_| Vec::new(),
        |list| {
            (0..list.n_items())
                .filter_map(|i| list.string(i))
                .map(|rel| rel.to_string())
                .collect()
        },
    )
}

/// What dropping `paths` into `dir` moves: each of them that has somewhere to go there.
fn moves_into(paths: &[String], dir: &str) -> Vec<(String, String)> {
    paths
        .iter()
        .filter_map(|from| Some((from.clone(), crate::fileops::move_dest(from, dir)?)))
        .collect()
}

/// One-second folder-opening delay, matching GTK file chooser/Nautilus
/// and avoiding expansion when a drag merely crosses a folder.
const SPRING_OPEN: Duration = Duration::from_millis(800);

/// A timer waiting to open the folder a drag is resting on.
type Spring = Rc<crate::widgets::Debounce>;

/// Open a collapsed folder after a drag rests on its row.
///
/// `None` disarms the per-row timer on leaving or dropping. Leave opened folders expanded,
/// matching a manual chevron click.
fn spring_open(timer: &Spring, row: Option<gtk::TreeListRow>) {
    timer.cancel();
    let Some(row) = row.filter(|row| row.is_expandable() && !row.is_expanded()) else {
        return;
    };
    timer.call(move || row.set_expanded(true));
}

/// Tree row under an expander drop target, or `None` for the vault label
/// and blank list area.
fn target_row(target: &gtk::DropTarget) -> Option<gtk::TreeListRow> {
    target
        .widget()?
        .downcast::<gtk::TreeExpander>()
        .ok()?
        .list_row()
}

/// Accept moves to `dir` at the pointer, where `Some("")` is root and `None` refuses.
///
/// Validate while hovering so self, descendant and no-op drops never highlight as targets.
/// GTK's active-drop outline provides all feedback. Rows from another window's tree are copied
/// into any folder instead, or moved with Shift ([`Carry`]).
pub(super) fn move_target(
    on_move: &Move,
    on_append: &Append,
    carry: &Carry,
    dir: impl Fn(&gtk::DropTarget, f64, f64) -> Option<String> + 'static,
) -> gtk::DropTarget {
    // Copy too, or a drag from another window offering copy alone (Ctrl) would never enter; each
    // answer narrows it again to what that drag may do.
    let target = gtk::DropTarget::new(
        glib::Type::INVALID,
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
    target.set_types(&[
        gtk::StringObject::static_type(),
        gtk::StringList::static_type(),
    ]);
    // The dragged paths have to be readable while the drag is still in flight, or the decision
    // could only be taken once the drop had already happened.
    target.set_preload(true);
    let dir = Rc::new(dir);
    // Whether a drop here does anything: a PDF's pages put after another's, or a move.
    let planned = {
        let dir = dir.clone();
        move |target: &gtk::DropTarget, x, y| {
            let carried = dragged(&target.value()?);
            if appending(target, &carried).is_some() {
                return Some(());
            }
            let moves = moves_into(&carried, &dir(target, x, y)?);
            (!moves.is_empty()).then_some(())
        }
    };
    let planned = Rc::new(planned);
    let spring = Spring::new(crate::widgets::Debounce::new(SPRING_OPEN));
    // Both, because `enter` is what decides whether the row highlights at all and `motion` is
    // what corrects it once the preloaded value has arrived.
    let answer = {
        let (planned, spring, dir, carry) =
            (planned.clone(), spring.clone(), dir.clone(), carry.clone());
        move |target: &gtk::DropTarget, x, y| {
            // Another vault's rows can be neither the folder they land in nor inside it, so any
            // folder takes them. Only they may be copied: a row of this tree only ever moves.
            let foreign = (carry.foreign)();
            let action = match foreign {
                true => dir(target, x, y).map(|_| wanted(target)),
                false => planned(target, x, y).map(|()| gdk::DragAction::MOVE),
            };
            target.set_actions(match foreign {
                true => gdk::DragAction::COPY | gdk::DragAction::MOVE,
                false => gdk::DragAction::MOVE,
            });
            match action {
                Some(action) => {
                    spring_open(&spring, target_row(target));
                    action
                }
                // A folder that cannot take what is over it has no reason to open either.
                None => {
                    spring_open(&spring, None);
                    gdk::DragAction::empty()
                }
            }
        }
    };
    target.connect_enter({
        let answer = answer.clone();
        move |target, x, y| answer(target, x, y)
    });
    target.connect_motion(answer);
    target.connect_leave({
        let spring = spring.clone();
        move |_| spring_open(&spring, None)
    });
    let (on_move, on_append, carry) = (on_move.clone(), on_append.clone(), carry.clone());
    target.connect_drop(move |target, value, x, y| {
        spring_open(&spring, None);
        // The value is handed over here rather than read back off the target, which is the one
        // place it is certain to have arrived.
        let carried = dragged(value);
        if (carry.foreign)() {
            let Some(dir) = dir(target, x, y).filter(|_| !carried.is_empty()) else {
                return false;
            };
            (carry.drop)(carried, dir, wanted(target) == gdk::DragAction::MOVE);
            return true;
        }
        if let Some((one, onto)) = appending(target, &carried) {
            on_append(one, onto);
            return true;
        }
        let Some(dir) = dir(target, x, y) else {
            return false;
        };
        let moves = moves_into(&carried, &dir);
        if moves.is_empty() {
            return false;
        }
        on_move(moves);
        true
    });
    target
}

/// External import callback: local files, vault-relative folder (`""`
/// for root) and move flag requesting original removal.
pub(super) type Import = Rc<dyn Fn(Vec<PathBuf>, String, bool)>;

/// Import external file drops onto folder rows, file-parent rows or the vault root.
///
/// `dir` uses the same zones as [`move_target`]. GDK deserialises URI lists into GdkFileList.
/// Move-only offers move; other offers copy. A copied PDF on a PDF row appends pages instead;
/// a moved PDF remains a file move. A drag that began inside accent is never taken.
pub(super) fn import_target(
    on_import: &Import,
    on_append: &Append,
    dir: impl Fn(&gtk::DropTarget, f64, f64) -> Option<String> + 'static,
) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(
        gdk::FileList::static_type(),
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
    // GTK's own test, an action and a format in common, and never a drag that began in accent: a
    // tree row offers its files to other applications, and this target, sharing the row with
    // the move's, would otherwise darken the row the move lit and take the drop from it.
    target.connect_accept(|target, drop| {
        drop.drag().is_none()
            && drop.actions().intersects(target.actions())
            && target
                .formats()
                .is_some_and(|ours| ours.match_(&drop.formats()))
    });
    let dir = Rc::new(dir);
    let spring = Spring::new(crate::widgets::Debounce::new(SPRING_OPEN));
    let answer = {
        let (dir, spring) = (dir.clone(), spring.clone());
        move |target: &gtk::DropTarget, x, y| match dir(target, x, y) {
            Some(_) => {
                spring_open(&spring, target_row(target));
                wanted(target)
            }
            None => {
                spring_open(&spring, None);
                gdk::DragAction::empty()
            }
        }
    };
    target.connect_enter({
        let answer = answer.clone();
        move |target, x, y| answer(target, x, y)
    });
    target.connect_motion(answer);
    target.connect_leave({
        let spring = spring.clone();
        move |_| spring_open(&spring, None)
    });
    let (on_import, on_append) = (on_import.clone(), on_append.clone());
    target.connect_drop(move |target, value, x, y| {
        spring_open(&spring, None);
        let (Some(into), Some(files)) = (dir(target, x, y), dropped_paths(value)) else {
            return false;
        };
        let moving = wanted(target) == gdk::DragAction::MOVE;
        let carried: Vec<String> = files.iter().map(|f| f.to_string_lossy().into()).collect();
        if let Some((one, onto)) = appending(target, &carried).filter(|_| !moving) {
            on_append(one, onto);
            return true;
        }
        on_import(files, into, moving);
        true
    });
    target
}

/// Extract local paths from an external file drop, or `None` when none are local.
///
/// FTP and trash URIs cannot be copied as local files. Expose this extraction so
/// `ACCENT_BENCH_DROP` can supply a GdkFileList without cross-process Xvfb dragging.
pub fn dropped_paths(value: &glib::Value) -> Option<Vec<PathBuf>> {
    let files: Vec<PathBuf> = value
        .get::<gdk::FileList>()
        .ok()?
        .files()
        .iter()
        .filter_map(|f| f.path())
        .collect();
    (!files.is_empty()).then_some(files)
}

/// Choose move only for move-only offers, which is what Shift narrows a drag from a file manager
/// or another window's tree to; otherwise copy.
fn wanted(target: &gtk::DropTarget) -> gdk::DragAction {
    let offered = target
        .current_drop()
        .map(|drop| drop.actions())
        .unwrap_or(gdk::DragAction::COPY);
    match offered == gdk::DragAction::MOVE {
        true => gdk::DragAction::MOVE,
        false => gdk::DragAction::COPY,
    }
}
