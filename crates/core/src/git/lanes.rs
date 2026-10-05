//! The history: one page of `git log` across every ref, and the graph its rows are drawn on.

use super::proc::run;
use super::{Commit, Error, LogRow, Ref, RefKind, Repo};

/// One page of history across every ref, children before parents.
///
/// A repository with no commits is not a special case: `--all` simply has no refs to walk and git
/// exits cleanly with no output.
pub fn log(repo: &Repo, skip: usize, n: usize) -> Result<Vec<Commit>, Error> {
    let (skip, n) = (skip.to_string(), n.to_string());
    let out = run(
        &repo.root,
        &[
            "log",
            "--all",
            "--topo-order",
            "-n",
            &n,
            "--skip",
            &skip,
            // Whole ref names in `%D`, which is what tells a branch from a tag of the same name.
            "--decorate=full",
            // Unit and record separators: a summary line can hold anything else, including tabs,
            // and a body holds newlines, so the record separator has to be neither.
            "--format=%H%x1f%P%x1f%D%x1f%aN%x1f%aE%x1f%at%x1f%s%x1f%b%x1e",
        ],
        true,
    )?;
    Ok(parse_log(&out))
}

fn parse_log(bytes: &[u8]) -> Vec<Commit> {
    String::from_utf8_lossy(bytes)
        .split('\x1e')
        // git ends every record with a newline of its own, which lands before the next record.
        .map(str::trim)
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let mut fields = record.split('\x1f');
            let id = fields.next()?.to_string();
            let parents = fields
                .next()?
                .split_whitespace()
                .map(String::from)
                .collect();
            let refs = parse_refs(fields.next()?);
            Some(Commit {
                id,
                parents,
                refs,
                author: fields.next()?.to_string(),
                email: fields.next()?.to_string(),
                time: fields.next()?.parse().unwrap_or(0),
                summary: fields.next().unwrap_or_default().to_string(),
                // Last, so its newlines are the record's own trailing whitespace and the trim
                // above has already taken them.
                body: fields.next().unwrap_or_default().to_string(),
            })
        })
        .collect()
}

/// Read `%D` under `--decorate=full`: `HEAD -> refs/heads/main, tag: refs/tags/v1,
/// refs/remotes/origin/main`, or `HEAD` alone where it is detached.
///
/// Only branches, tags, HEAD and the stash are kept: `origin/HEAD` points at a branch rather than
/// being one, and notes are refs no row has anything to say about. The stash is, its commits being
/// in the history with nothing else saying what they are. Sorted HEAD's first, then by
/// [`RefKind`], git's own order kept within each kind.
pub(super) fn parse_refs(decorations: &str) -> Vec<Ref> {
    let mut refs: Vec<Ref> = decorations
        .split(", ")
        .filter_map(|decoration| {
            let (head, full) = match decoration.strip_prefix("HEAD -> ") {
                Some(full) => (true, full),
                None => (decoration == "HEAD", decoration),
            };
            let (kind, name) = if full == "HEAD" {
                (RefKind::Head, full)
            } else if let Some(name) = full.strip_prefix("refs/heads/") {
                (RefKind::LocalBranch, name)
            } else if let Some(name) = full.strip_prefix("tag: refs/tags/") {
                (RefKind::Tag, name)
            } else if full == "refs/stash" {
                (RefKind::Stash, "stash")
            } else {
                let name = full
                    .strip_prefix("refs/remotes/")
                    .filter(|name| !name.ends_with("/HEAD"))?;
                (RefKind::RemoteBranch, name)
            };
            Some(Ref {
                name: name.to_string(),
                kind,
                head,
            })
        })
        .collect();
    refs.sort_by_key(|r| (!r.head, r.kind));
    refs
}

/// Lay commits out on a graph, one column per line of history.
///
/// The state is one slot per column holding the oid that column is *waiting* for. A commit claims
/// the column already waiting for it (or a free one if nothing is), its parents claim columns for
/// the rows below, and everything else passes straight through. Because `--topo-order` guarantees
/// children come before parents, one forward pass is enough — no lookahead, no second walk.
///
/// Freed columns are reused, so a history of two branches never drifts rightwards: the width of
/// the drawing stays the number of lines of history actually open at that row.
///
/// Each column also carries the name of the branch it draws, taken from the first decorated
/// commit on it and handed down its first parents, so a branch merged into it by fast-forward
/// does not rename it. Where several columns end at one commit, the commit carries its own column
/// on and the others are the branches that forked from it: the graph's own reading, one line
/// going straight on and the rest joining it.
pub fn lanes(commits: Vec<Commit>) -> Vec<LogRow> {
    let mut lanes: Vec<Option<Lane>> = Vec::new();
    let mut rows = Vec::with_capacity(commits.len());

    for commit in commits {
        let above: Vec<usize> = waiting_for(&lanes, &commit.id).collect();
        let column = match above.first() {
            Some(&column) => column,
            None => free_slot(&mut lanes),
        };
        // Every column drawn into this commit ends here, its own first.
        let ending: Vec<Option<String>> = above
            .iter()
            .map(|&i| lanes[i].take().and_then(|l| l.name))
            .collect();
        let lane = ending
            .first()
            .cloned()
            .flatten()
            .or_else(|| lane_name(&commit.refs));
        let forks: Vec<String> = ending.into_iter().skip(1).flatten().collect();

        let through: Vec<usize> = (0..lanes.len())
            .filter(|&i| i != column && lanes[i].is_some())
            .collect();

        let mut below = Vec::with_capacity(commit.parents.len());
        for (nth, parent) in commit.parents.iter().enumerate() {
            // The first parent stays in this commit's own column, and carries its name on; the
            // others join a column already waiting for them, or open one no name has reached yet.
            let waiting = waiting_for(&lanes, parent).next();
            let (i, name) = match (nth, waiting) {
                (0, _) => (column, lane.clone()),
                (_, Some(i)) => (i, lanes[i].take().and_then(|l| l.name)),
                (_, None) => (free_slot(&mut lanes), None),
            };
            lanes[i] = Some(Lane {
                waiting: parent.clone(),
                name,
            });
            below.push(i);
        }

        while lanes.last().is_some_and(Option::is_none) {
            lanes.pop();
        }
        rows.push(LogRow {
            commit,
            column,
            above,
            below,
            through,
            lane,
            forks,
        });
    }
    rows
}

/// One open column of the graph: the oid it is waiting for, and the branch it draws.
struct Lane {
    waiting: String,
    name: Option<String>,
}

/// What a commit's decorations would name a column after: its first branch, or failing that its
/// first tag. A detached HEAD and the stash name no line of history.
fn lane_name(refs: &[Ref]) -> Option<String> {
    refs.iter()
        .find(|r| !matches!(r.kind, RefKind::Head | RefKind::Stash))
        .map(|r| r.name.clone())
}

fn waiting_for<'a>(lanes: &'a [Option<Lane>], oid: &'a str) -> impl Iterator<Item = usize> + 'a {
    lanes
        .iter()
        .enumerate()
        .filter(move |(_, lane)| lane.as_ref().is_some_and(|l| l.waiting == oid))
        .map(|(i, _)| i)
}

fn free_slot(lanes: &mut Vec<Option<Lane>>) -> usize {
    match lanes.iter().position(Option::is_none) {
        Some(i) => i,
        None => {
            lanes.push(None);
            lanes.len() - 1
        }
    }
}
