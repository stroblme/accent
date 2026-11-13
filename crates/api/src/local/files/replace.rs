//! Replace All over the indexed files, through the etag gate a file at a time, with what it
//! rewrote kept within a budget for one undo.

use accent_core::{notebook, search};

use super::{Before, Local, Msg};
use crate::{Error, Options, Regex, ReplaceReport, Result, UndoReport, fs, locked};

/// Memory budget for Replace All originals. Replacements still run beyond
/// the cap, but no undo is offered.
const UNDO_CAP: usize = 64 * 1024 * 1024;

impl Local {
    /// Replace `query` under `options` in indexed matching files, using the
    /// same compilation and `include_ignored` as [`grep`](Self::grep). With
    /// `literal`, write `$1` literally rather than expanding captures. Report
    /// per-file failures so partial replacements remain visible.
    /// Read, substitute and fsync on the calling thread; UI callers must use a
    /// worker. [`crate::vault::REPLACE_BOUND`] bounds host work and remote waits.
    /// Keep originals up to [`UNDO_CAP`] for [`undo_replace`](Self::undo_replace),
    /// replacing the previous undo record. Remote originals stay on the host.
    /// Progress is indeterminate; no progress callback is provided.
    pub fn replace_all(
        &self,
        query: &str,
        options: Options,
        replacement: &str,
        literal: bool,
        include_ignored: bool,
    ) -> Result<ReplaceReport> {
        self.replace_within(
            &search::pattern(query, options)?,
            replacement,
            literal,
            include_ignored,
            crate::vault::REPLACE_BOUND,
        )
    }

    /// Run [`replace_all`](Self::replace_all) within a budget, stopping between
    /// whole-file writes. Report unprocessed files; another run finishes remaining
    /// matches without leaving a partially written file.
    pub(super) fn replace_within(
        &self,
        re: &Regex,
        replacement: &str,
        literal: bool,
        include_ignored: bool,
        budget: std::time::Duration,
    ) -> Result<ReplaceReport> {
        let deadline = std::time::Instant::now() + budget;
        let mut report = ReplaceReport::default();
        *locked(&self.undo) = None;
        // `None` once the pre-images have passed the cap, and never kept again for this rewrite.
        let mut undo = Some(Vec::new());
        let mut kept = 0usize;
        // Bound to a name before the loop, not iterated straight out of the call: the guard
        // `searcher()` returns is a temporary of the `for` *statement*, so writing it that way
        // held the search connection for the whole rewrite — a query that reached the vault
        // meanwhile waited out every file. The worker reindexes them as they land.
        let paths = self.searcher().grep_paths(re, include_ignored)?;
        for rel in paths {
            if std::time::Instant::now() >= deadline {
                report
                    .failed
                    .push((rel, format!("the rewrite stopped after {budget:?}")));
                continue;
            }
            match self.replace_one(&rel, re, replacement, literal) {
                Ok(None) => {}
                Ok(Some((n, before))) => {
                    report.rewritten.push(rel);
                    report.matches += n;
                    kept += before.text.len();
                    if kept > UNDO_CAP {
                        undo = None;
                    } else if let Some(undo) = &mut undo {
                        undo.push(before);
                    }
                }
                Err(e) => {
                    tracing::warn!("replacing in {rel}: {e:#}");
                    report.failed.push((rel, format!("{e:#}")));
                }
            }
        }
        // The rewrites are on disk; the index is a worker batch behind them. The caller is the
        // Search pane, which asks its question again the moment this returns and asks it of the
        // index — so it returns once the index agrees rather than a moment before.
        self.settle_index();
        report.undoable = undo.as_ref().is_some_and(|undo| !undo.is_empty());
        *locked(&self.undo) = undo;
        Ok(report)
    }

    /// Count removed matches from current file text and retain its original;
    /// `None` if no matches. Avoid stale indexed counts. Rewrite diagrams
    /// through their labels only, preserving XML keys, ids and markup in one write,
    /// and an `.ipynb` through its cells' sources, its outputs, metadata and every
    /// other cell keeping their bytes.
    fn replace_one(
        &self,
        rel: &str,
        re: &Regex,
        replacement: &str,
        literal: bool,
    ) -> Result<Option<(usize, Before)>> {
        let path = self.resolve_inside(rel)?;
        let (text, etag) = fs::read_note(&path)?;
        let substitute = |text: &str| match literal {
            true => re
                .replace_all(text, search::NoExpand(replacement))
                .into_owned(),
            false => re.replace_all(text, replacement).into_owned(),
        };
        // A marimo notebook is the Python it is, and an `.ipynb` that does not read is its text.
        let notebook = match accent_core::path::is_notebook(rel) {
            true => notebook::read(rel, &text).ok(),
            false => None,
        };
        let (matches, rewritten) = if accent_core::path::holds_diagram(rel, &text) {
            let mut file = accent_drawio::File::from_bytes(text.as_bytes())
                .map_err(|e| accent_core::Error::Invalid(format!("{rel}: {e}")))?;
            let mut matches = 0;
            accent_drawio::text::edit_labels(&mut file, |label| {
                let found = re.find_iter(label).count();
                matches += found;
                (found > 0).then(|| substitute(label))
            });
            (matches, file.to_xml())
        } else if let Some(mut nb) = notebook {
            let mut matches = 0;
            let edits: Vec<notebook::Edit> = nb
                .cells()
                .iter()
                .filter_map(|cell| {
                    let found = re.find_iter(cell.source()).count();
                    matches += found;
                    (found > 0).then(|| notebook::Edit::Source {
                        id: cell.id(),
                        text: substitute(cell.source()),
                    })
                })
                .collect();
            nb.apply_all(edits);
            (matches, nb.to_text())
        } else {
            (re.find_iter(&text).count(), substitute(&text))
        };
        if matches == 0 {
            return Ok(None);
        }
        let etag = fs::write_note(&path, &rewritten, Some(etag))?;
        self.post(Msg::Update {
            rel: rel.to_string(),
            own: true,
        });
        let rel = rel.to_string();
        Ok(Some((matches, Before { rel, text, etag })))
    }

    /// Undo the last [`replace_all`](Self::replace_all) through the etag gate.
    /// Leave changed files untouched and report them. Consume the undo record
    /// even on failure, so a second call cannot restore it again.
    pub fn undo_replace(&self) -> Result<UndoReport> {
        let before = locked(&self.undo).take().unwrap_or_default();
        let mut report = UndoReport::default();
        for Before { rel, text, etag } in before {
            match self.save(&rel, &text, Some(etag)) {
                Ok(_) => report.restored.push(rel),
                Err(Error::Core(accent_core::Error::ChangedOnDisk { .. })) => {
                    report.skipped.push(rel)
                }
                Err(e) => report.failed.push((rel, e.to_string())),
            }
        }
        // As after the rewrite: the pane asks its question again of the index once this returns.
        self.settle_index();
        Ok(report)
    }
}
