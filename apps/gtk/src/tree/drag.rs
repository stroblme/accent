//! Dropping onto the tree: a row dragged within it moves, files from another application are
//! copied in, and a shut folder a drag rests on springs open.

use super::*;

/// What a tree-to-tree move travels as, beside the plain string a pane opens.
///
/// ponytail: a `GtkStringObject` rather than the `application/x-accent-path` mime the design note
/// named, because `GtkDropTarget` matches on GType and never on a mime type — a mime would mean
/// `GtkDropTargetAsync` and reading the drop's stream by hand. What the decision asks for is a
/// type the panes do not take, and their target takes `AdwTabPage` and `String` only, so a folder
/// offering this and nothing else cannot be dropped into a pane at all.
pub(super) fn move_content(rel: &str) -> gdk::ContentProvider {
    gdk::ContentProvider::for_value(&gtk::StringObject::new(rel).to_value())
}

/// What a drop is handed to: each path a drag carried, and the path it goes to.
pub(super) type Move = Rc<dyn Fn(Vec<(String, String)>)>;

/// The paths a tree drag is carrying: one row, or the marked set a marked row carries along
/// (a `GtkStringList`, which no pane takes either — there is no one note in it to open).
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

/// How long a drag rests over a shut folder before it opens. Long enough that crossing one on the
/// way somewhere else never opens it, short enough to read as part of the drag — the second
/// GTK's own file chooser and Nautilus both wait.
const SPRING_OPEN: Duration = Duration::from_millis(800);

/// A timer waiting to open the folder a drag is resting on.
type Spring = Rc<crate::widgets::Debounce>;

/// Open the shut folder a drag has come to rest on, so a file can be dropped into something that
/// was not on screen when the drag began. `row` is the row under the pointer, `None` when the drag
/// has left the target or has been dropped, which disarms the timer.
///
/// Per drop target, which is per row: a drag crossing three folders arms and disarms three timers,
/// one at a time. Nothing closes the folder again — a drag that opened one and went elsewhere
/// leaves the tree as the reader would have left it by clicking the chevron.
fn spring_open(timer: &Spring, row: Option<gtk::TreeListRow>) {
    timer.cancel();
    let Some(row) = row.filter(|row| row.is_expandable() && !row.is_expanded()) else {
        return;
    };
    timer.call(move || row.set_expanded(true));
}

/// The `GtkTreeListRow` a drop target on a row expander is over, and `None` for a target that is
/// not on one — the vault row above the tree, and the list's own blank area.
fn target_row(target: &gtk::DropTarget) -> Option<gtk::TreeListRow> {
    target
        .widget()?
        .downcast::<gtk::TreeExpander>()
        .ok()?
        .list_row()
}

/// A drop target that moves the dragged files into the directory `dir` answers with for the
/// pointer position — `Some("")` being the vault root — and refuses the drop where it answers
/// `None`, or where none of them has anywhere to go there.
///
/// The refusal happens while the pointer is still moving rather than after the drop, so a row
/// that cannot take what is over it never lights up: a folder onto itself, into what is under it,
/// or into the folder it is already in are simply not targets. GTK's own `:drop(active)` outline
/// on the row is then the whole of the feedback, and there is nothing else to draw.
pub(super) fn move_target(
    on_move: &Move,
    dir: impl Fn(&gtk::DropTarget, f64, f64) -> Option<String> + 'static,
) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::MOVE);
    target.set_types(&[
        gtk::StringObject::static_type(),
        gtk::StringList::static_type(),
    ]);
    // The dragged paths have to be readable while the drag is still in flight, or the decision
    // could only be taken once the drop had already happened.
    target.set_preload(true);
    let dir = Rc::new(dir);
    let planned = {
        let dir = dir.clone();
        move |target: &gtk::DropTarget, x, y| {
            let moves = moves_into(&dragged(&target.value()?), &dir(target, x, y)?);
            (!moves.is_empty()).then_some(moves)
        }
    };
    let planned = Rc::new(planned);
    let spring = Spring::new(crate::widgets::Debounce::new(SPRING_OPEN));
    // Both, because `enter` is what decides whether the row highlights at all and `motion` is
    // what corrects it once the preloaded value has arrived.
    let answer = {
        let (planned, spring) = (planned.clone(), spring.clone());
        move |target: &gtk::DropTarget, x, y| match planned(target, x, y) {
            Some(_) => {
                spring_open(&spring, target_row(target));
                gdk::DragAction::MOVE
            }
            // A folder that cannot take what is over it has no reason to open either.
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
    let on_move = on_move.clone();
    target.connect_drop(move |target, value, x, y| {
        spring_open(&spring, None);
        // The value is handed over here rather than read back off the target, which is the one
        // place it is certain to have arrived.
        let Some(dir) = dir(target, x, y) else {
            return false;
        };
        let moves = moves_into(&dragged(value), &dir);
        if moves.is_empty() {
            return false;
        }
        on_move(moves);
        true
    });
    target
}

/// What a drop of files from outside accent is handed to: the files, the vault-relative folder
/// they go into ("" being the root) and whether the drag was a move, which takes the originals
/// away.
pub(super) type Import = Rc<dyn Fn(Vec<PathBuf>, String, bool)>;

/// A drop target for files dragged in from another application — GNOME Files, a browser's
/// downloads — onto the same three zones a tree-to-tree move has: a folder row, a file row (its
/// folder) and the blank area or the vault row (the root). `dir` answers with the folder for a
/// pointer position, exactly as [`move_target`]'s does.
///
/// `GdkFileList` is the type rather than `text/uri-list`: GDK deserialises the one into the other,
/// so this takes what every file manager offers without reading a stream by hand. A drop that
/// offers **only** move is moved — that is Shift held in the file manager — and anything else is
/// copied, which is what a plain drag between applications means.
pub(super) fn import_target(
    on_import: &Import,
    dir: impl Fn(&gtk::DropTarget, f64, f64) -> Option<String> + 'static,
) -> gtk::DropTarget {
    let target = gtk::DropTarget::new(
        gdk::FileList::static_type(),
        gdk::DragAction::COPY | gdk::DragAction::MOVE,
    );
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
    let on_import = on_import.clone();
    target.connect_drop(move |target, value, x, y| {
        spring_open(&spring, None);
        let (Some(into), Some(files)) = (dir(target, x, y), dropped_paths(value)) else {
            return false;
        };
        on_import(files, into, wanted(target) == gdk::DragAction::MOVE);
        true
    });
    target
}

/// The files a drop from another application carries, as paths on this machine, or `None` where
/// it named none that way — an `ftp://` or a `trash://` URI has nothing here to copy from.
///
/// Public because it is the half of a cross-application drop a drill can drive: Xvfb carries a
/// drag inside one process and not between two, so `ACCENT_BENCH_DROP` builds the `GdkFileList`
/// itself and takes it from here.
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

/// What a drop from another application is asking for: a move only where move is the one action
/// it offers, which is how a file manager reports Shift being held.
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
