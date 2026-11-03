//! Per-vault session state: the tabs, panes and places a window is put back with.

use super::file::{replace, state_dir, state_path, unreadable};
use crate::Result;
use crate::git::Comparison;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// PDF sizing mode, stored per document in core session state.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PdfZoom {
    /// The widest page fills the width. What a reader wants for text, so it is the default.
    #[default]
    FitWidth,
    /// The tallest page fits entirely, so one page is one screen. Fit Height in the interface,
    /// and what a reset goes back to; `fit-page` here because sessions have saved it as that.
    FitPage,
    /// A fixed multiple of the page's natural size.
    Scale(f64),
}

/// Where a PDF was last being read.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PdfPlace {
    pub page: usize,
    pub zoom: PdfZoom,
    /// Whether its drawing tools were out, which the header's Drawing toggle says per tab.
    pub drawing: bool,
}

/// Where a diagram was last being looked at.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DiagramPlace {
    pub page: usize,
    /// `None` while the page is fitted to the window, which is how a diagram opens.
    pub zoom: Option<f64>,
    /// The scroll position, in pixels of the zoomed canvas.
    pub x: f64,
    pub y: f64,
}

/// Where a shell was, so it can be started there again when nothing kept it running.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ShellPlace {
    /// An absolute directory on this machine, or `ssh://[user@]host[:port]/path` on another.
    pub at: String,
    /// The name of the user's command (`[[commands]]`) it ran, whose tab is put back only while
    /// the command still runs: never started again.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

/// The Info pane's sections as the reader left them: which are open, and where each divider
/// between them sits, as the share of its split above it (`None` for an even split).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InfoPane {
    pub references: bool,
    pub tags: bool,
    pub details: bool,
    pub dividers: [Option<f64>; 2],
}

impl Default for InfoPane {
    fn default() -> Self {
        InfoPane {
            references: true,
            tags: true,
            details: false,
            dividers: [None, None],
        }
    }
}

/// The Code pane's sections as the reader left them: which are open, and where each divider
/// between them sits, as the share of its split above it (`None` for an even split), top first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CodePane {
    pub goals: bool,
    pub expected: bool,
    /// The debugger's Call Stack, Variables, Watch and Breakpoints.
    pub stack: bool,
    pub variables: bool,
    pub watch: bool,
    pub breakpoints: bool,
    pub dividers: Vec<Option<f64>>,
}

impl Default for CodePane {
    fn default() -> Self {
        CodePane {
            goals: true,
            expected: true,
            stack: true,
            variables: true,
            watch: true,
            breakpoints: true,
            dividers: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Vault-relative paths of the open tabs.
    pub open: Vec<String>,
    pub active: Option<String>,
    /// How the tabs were split into panes. `open` still says which tabs there are, and
    /// [`Session::panes`] places them in this; `None` in a file written before layouts were.
    /// Not called `pane`: that key is the retired sidebar pane every older file still carries.
    pub layout: Option<Layout>,
    pub sidebar: bool,
    pub sidebar_width: i32,
    pub info: InfoPane,
    pub code: CodePane,
    /// Document zoom, 1.0 being the font as GNOME sets it.
    pub zoom: f64,
    /// Recently opened files, most recent first. The window records reads because filesystem
    /// mtime tracks edits. Called `recent_notes` in older state files.
    #[serde(alias = "recent_notes")]
    pub recent_files: Vec<String>,
    /// Full action names of the commands run from the palette or a menu, most recent first.
    pub recent_commands: Vec<String>,
    /// Saved reading positions keyed by vault-relative PDF path. Entries are not pruned.
    pub pdf: BTreeMap<String, PdfPlace>,
    /// Saved diagram positions keyed by vault-relative path; entries are not pruned.
    pub diagram: BTreeMap<String, DiagramPlace>,
    /// Where each open shell was, keyed by its tab key (`terminal:<id>`).
    pub terminals: BTreeMap<String, ShellPlace>,
    /// The keys of the pinned tabs. Which pane each is in, and where, is the layout's: a pinned
    /// tab is always ahead of the others in its pane's `tabs`.
    pub pinned: Vec<String>,
    /// The diagrams whose pictures on the web may be downloaded, by vault-relative path: the
    /// reader said Load on the diagram's banner. Kept here, never in the diagram.
    pub web_images: Vec<String>,
    /// The comparisons with git the tabs showed, by tab key: a note compared with the index in
    /// its own tab under the note's, and a staged change or a commit's file, a tab of its own,
    /// under that tab's. A comparison with anything else is not kept.
    pub compared: BTreeMap<String, Comparison>,
    /// By file key (vault-relative, absolute outside the vault), the breakpoints a program stops
    /// at; kept here and never in the vault. Under DAP's name for them: the key `breakpoints`
    /// held bare lines.
    #[serde(rename = "source_breakpoints")]
    pub breakpoints: BTreeMap<String, Vec<Breakpoint>>,
    /// The expressions the debugger's Watch section evaluates at each stop, in its order.
    pub watches: Vec<String>,
    /// The exceptions the debug adapter last offered to stop at, as the reader set them.
    pub exceptions: Vec<ExceptionFilter>,
    /// The kernelspec each notebook in another language than Python runs on, by vault-relative
    /// path, where the reader picked one: kept here and never in the notebook (user decision
    /// 2026-10-27). Entries are not pruned.
    pub kernels: BTreeMap<String, String>,
}

/// A line a debugged program stops at, and what the reader set beside it: VS Code's conditional
/// breakpoints, hit counts and logpoints, each as typed, for the adapter to read.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Breakpoint {
    /// 0-based.
    pub line: u32,
    /// The program stops only where this expression is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    /// The program stops only on the hits this names (debugpy reads `5`, `>= 5`, `% 5`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hit_condition: Option<String>,
    /// Printed to the Debug Console instead of stopping, the `{expression}`s in it evaluated.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_message: Option<String>,
}

impl Breakpoint {
    /// A plain breakpoint on `line`.
    pub fn at(line: u32) -> Breakpoint {
        Breakpoint {
            line,
            ..Breakpoint::default()
        }
    }
}

/// A kind of exception a debugged program can stop at, as its adapter names it, and whether it
/// does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExceptionFilter {
    /// The adapter's id for it.
    pub filter: String,
    pub label: String,
    pub description: Option<String>,
    pub on: bool,
}

impl Default for Session {
    fn default() -> Self {
        Session {
            open: Vec::new(),
            active: None,
            layout: None,
            sidebar: true,
            sidebar_width: 280,
            info: InfoPane::default(),
            code: CodePane::default(),
            zoom: 1.0,
            recent_files: Vec::new(),
            recent_commands: Vec::new(),
            pdf: BTreeMap::new(),
            diagram: BTreeMap::new(),
            terminals: BTreeMap::new(),
            pinned: Vec::new(),
            web_images: Vec::new(),
            compared: BTreeMap::new(),
            breakpoints: BTreeMap::new(),
            watches: Vec::new(),
            exceptions: Vec::new(),
            kernels: BTreeMap::new(),
        }
    }
}

/// The panes of a window: one pane's tabs, or a split between two layouts. The same tree the
/// window's `GtkPaned`s make, written down.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layout {
    /// Tabs in the order the bar shows them, and the one in front.
    Pane {
        tabs: Vec<String>,
        selected: Option<String>,
    },
    /// `start` is left of `end`, or above it when `vertical`. `ratio` is the share of the split
    /// `start` has.
    Split {
        vertical: bool,
        ratio: f64,
        start: Box<Layout>,
        end: Box<Layout>,
    },
}

impl Layout {
    /// This layout holding exactly the tabs in `open`. A tab it does not have goes to the end of
    /// the pane holding `active`, or of the first pane when none does; a tab no longer open is
    /// dropped; a pane left with nothing collapses, its sibling taking the split's place; and a
    /// ratio is kept to 0.1–0.9, so neither side of a split comes back too thin to find. `None`
    /// when no tab is left.
    pub fn place(self, open: &[String], active: Option<&str>) -> Option<Layout> {
        let missing: Vec<String> = open
            .iter()
            .filter(|key| !self.holds(key))
            .cloned()
            .collect();
        let Some(mut layout) = self.keep(open) else {
            return (!missing.is_empty()).then_some(Layout::Pane {
                tabs: missing,
                selected: None,
            });
        };
        let active = active.filter(|key| layout.holds(key));
        let into = |tabs: &[String]| active.is_none_or(|key| tabs.iter().any(|t| t == key));
        if let Some(tabs) = layout.pane_where(&into) {
            tabs.extend(missing);
        }
        Some(layout)
    }

    fn holds(&self, key: &str) -> bool {
        match self {
            Layout::Pane { tabs, .. } => tabs.iter().any(|t| t == key),
            Layout::Split { start, end, .. } => start.holds(key) || end.holds(key),
        }
    }

    /// The tabs of the first pane `wanted` accepts.
    fn pane_where(&mut self, wanted: &impl Fn(&[String]) -> bool) -> Option<&mut Vec<String>> {
        match self {
            Layout::Pane { tabs, .. } => wanted(tabs).then_some(tabs),
            Layout::Split { start, end, .. } => {
                start.pane_where(wanted).or_else(|| end.pane_where(wanted))
            }
        }
    }

    /// The part of [`place`](Self::place) that takes away: tabs not in `open`, then the panes and
    /// splits that leaves with nothing.
    fn keep(self, open: &[String]) -> Option<Layout> {
        match self {
            Layout::Pane { mut tabs, selected } => {
                tabs.retain(|key| open.contains(key));
                let selected = selected.filter(|key| tabs.contains(key));
                (!tabs.is_empty()).then_some(Layout::Pane { tabs, selected })
            }
            Layout::Split {
                vertical,
                ratio,
                start,
                end,
            } => match (start.keep(open), end.keep(open)) {
                (Some(start), Some(end)) => Some(Layout::Split {
                    vertical,
                    ratio: ratio.clamp(0.1, 0.9),
                    start: Box::new(start),
                    end: Box::new(end),
                }),
                (one, other) => one.or(other),
            },
        }
    }
}

impl Session {
    /// Missing or broken state is not an error: the window simply opens empty. A broken file is
    /// only worth a warning, because the next close rewrites it and nothing in it is the user's
    /// own work.
    pub fn load(root: &Path) -> Session {
        let path = state_path(root);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Session::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("{}: {e}, opening empty", path.display());
            Session::default()
        })
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        let path = state_path(root);
        let bytes = serde_json::to_vec_pretty(self).map_err(|e| unreadable(&path, e))?;
        replace(&path, &bytes)
    }

    /// Every session written down on this machine, whatever vault or terminal session it is for.
    /// `None` when one of them cannot be read, so that nothing is decided on the rest alone.
    pub fn stored() -> Option<Vec<Session>> {
        let dir = match std::fs::read_dir(state_dir()) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
            dir => dir.ok()?,
        };
        let mut sessions = Vec::new();
        for entry in dir {
            let path = entry.ok()?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                sessions.push(serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?);
            }
        }
        Some(sessions)
    }

    /// The panes to put back: the stored layout with the open tabs placed in it, or one pane
    /// holding them all when no layout was stored. `None` when there is no tab to put back.
    pub fn panes(&self) -> Option<Layout> {
        let layout = self.layout.clone().unwrap_or(Layout::Pane {
            tabs: Vec::new(),
            selected: None,
        });
        layout.place(&self.open, self.active.as_deref())
    }
}

/// Move `value` to the front of a recently-used list, deduplicated and capped at `cap`.
pub fn touch(list: &mut Vec<String>, value: &str, cap: usize) {
    list.retain(|v| v != value);
    list.insert(0, value.to_string());
    list.truncate(cap);
}

/// Follow a rename through a list of vault-relative paths, so a renamed note keeps its place
/// instead of leaving a dead entry behind. `from` may be a folder, in which case everything under
/// it moves with it.
pub fn rename_in(list: &mut [String], from: &str, to: &str) {
    let prefix = format!("{from}/");
    for rel in list {
        if rel == from {
            *rel = to.to_string();
        } else if let Some(rest) = rel.strip_prefix(&prefix) {
            *rel = format!("{to}/{rest}");
        }
    }
}

/// [`rename_in`] for the keys of a map: an entry under `from`, a file or a folder, moves with it.
pub fn rename_keys<T>(map: &mut BTreeMap<String, T>, from: &str, to: &str) {
    *map = std::mem::take(map)
        .into_iter()
        .map(|(mut key, value)| {
            rename_in(std::slice::from_mut(&mut key), from, to);
            (key, value)
        })
        .collect();
}
