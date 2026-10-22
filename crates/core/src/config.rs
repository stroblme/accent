//! Global settings and per-vault session state.
//!
//! Config stays outside the vault so preferences do not sync through Syncthing or git.
//! `~/.config/accent/config.toml` is documented by [`Config`] with a worked example.

use crate::git::Comparison;
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A macro supplies the literal required by the doc attribute; tests parse the same example.
macro_rules! example {
    () => {
        r#"recent_vaults = ["/home/me/Notes"]
spellcheck = true
word_suggestions = true
ghost_text = true
minimap = false
line_numbers = false
git_tree = true
show_hidden = false
git_tint = false
column_width = 50
full_width_preview = true
indent_width = 4
forward_keys_to_terminal = false
theme = "solarized"
focus_mode = "high"

[recolour]
images = true
pdfs = false
diagrams = true

[shortcuts]
"win.find-next" = ["F3"]
"win.about" = []

[search]
exclude = ["Archive", "Code/vendor"]

[drawing]
mouse = false
pen_width = 2.0
highlighter_width = 14.0
highlighter_color = [0, 0, 0]
eraser_radius = 4.0
eraser_partial = true

[diagram]
rounded = true
route = "curved"
arrow = false
grid = true

[lsp.servers]
python3 = ["pyright-langserver", "--stdio"]
rust = ["rust-analyzer"]

[debug.adapters]
python3 = ["python3", "-m", "debugpy.adapter"]

[vaults."/home/me/Notes"]
templates_dir = "Templates"
new_file_dir = "Inbox"
attachment_folder = "./attachments"

[vaults."/home/me/Notes".lsp.servers]
python3 = ["pylsp"]

[vaults."/home/me/Notes".debug]
python = [".venv/bin/python"]
"#
    };
}

/// Which colours the window paints itself in. System, light and dark are libadwaita's own;
/// Solarized is a palette of ours that still follows the system's light and dark state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
    Solarized,
}

/// How much of the window recedes while the user types (DESIGN.md, Chrome auto-hide).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FocusMode {
    /// Nothing fades.
    None,
    /// The bars, the sidebar and the minimap fade away.
    #[default]
    Medium,
    /// As Medium, and the other panes and the text away from the caret fade too.
    High,
}

/// The global config file.
#[doc = concat!("\n```toml\n", example!(), "```")]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Most recent first, and not capped: a vault leaves the list when it is removed from it, or,
    /// for a folder on this machine, when the app finds the folder gone.
    pub recent_vaults: Vec<PathBuf>,
    /// `None` follows the GNOME document font.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub editor_font: Option<String>,
    pub spellcheck: bool,
    /// Offer words as one is typed in prose: the document's own, then the system dictionary's.
    pub word_suggestions: bool,
    /// Suggest the rest of the line as the caret sits, from what the vault already says. Needs
    /// `merl-rt` on the path; the switch only says whether to ask for it.
    pub ghost_text: bool,
    /// A code map beside the document instead of the scrollbar.
    pub minimap: bool,
    /// Numbers in the editor's left gutter, in every text tab, code as well as prose. Off by
    /// default: a note is prose, and the gutter is what an ATX heading's markers hang in.
    pub line_numbers: bool,
    /// Group Git changes by folder to avoid repeating directory names in a flat list. Enabled by
    /// default.
    pub git_tree: bool,
    /// List dot-named files and folders in the Files pane, dimmed. On by default: `.gitignore`
    /// and `.python-version` are files people edit. `.git` and `.trash` are never listed.
    pub show_hidden: bool,
    /// Tint changed Files entries and their ancestors with the accent, and new files green.
    /// Enabled by default; ignored entries are always dimmed.
    pub git_tint: bool,
    /// Document-column width as a percentage of editor width. The editor enforces a readable
    /// minimum on narrow windows; 50 approximates the former 800 px cap on a maximised window.
    pub column_width: u32,
    /// Let the preview's page fill its pane instead of taking [`Config::column_width`]'s share
    /// of it, for a note whose tables are wider than a column of prose. Off by default.
    pub full_width_preview: bool,
    /// How many columns one indent is worth in a code tab: the width of a tab character and of
    /// the run of spaces Tab writes in its place. Prose is not measured in columns and keeps
    /// GtkSourceView's own.
    pub indent_width: u32,
    /// Hand a focused shell every key, accent's own shortcuts included, but Copy and Paste in
    /// Terminal. Off by default: the shell then gets every key but a small reserved set the
    /// window keeps (DESIGN.md, Keyboard → Terminal).
    pub forward_keys_to_terminal: bool,
    pub theme: Theme,
    pub focus_mode: FocusMode,
    pub recolour: RecolourConfig,
    /// Accelerator overrides, keyed by full action name ("win.save"). Only what the user changed
    /// is stored, so the built-in table stays the source of truth for everything else; an empty
    /// list means the action is deliberately unbound.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub shortcuts: BTreeMap<String, Vec<String>>,
    pub search: SearchConfig,
    pub drawing: DrawingConfig,
    pub diagram: DiagramConfig,
    /// This machine's servers, under every local vault's own (`Config::local_vault`).
    #[serde(skip_serializing_if = "LspConfig::is_empty")]
    pub lsp: LspConfig,
    /// This machine's debug adapters and interpreter, under every local vault's own.
    #[serde(skip_serializing_if = "DebugConfig::is_empty")]
    pub debug: DebugConfig,
    /// Keyed by canonical vault path.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub vaults: BTreeMap<String, VaultConfig>,
}

/// User-defined search exclusions, combined with git ignores. `All` includes them, the file tree
/// shows them dimmed, and markdown notes remain searchable.
///
/// Unlike [`crate::walk`] skips, these directories are still indexed and watched; exclusions
/// control results rather than traversal.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SearchConfig {
    /// Vault-relative directory paths, applied to every vault; one that names nothing in this
    /// vault costs nothing. A path, not a glob: `Archive` is the top-level folder of that name,
    /// and `Code/vendor` is the one inside `Code`.
    pub exclude: Vec<String>,
}

/// Which kinds of file a dark or Solarized theme puts on its paper and ink unasked. A kind
/// switched off shows its files as they are, and its Invert command recolours one of them
/// ([`crate::recolour::onto`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RecolourConfig {
    pub images: bool,
    pub pdfs: bool,
    pub diagrams: bool,
}

impl Default for RecolourConfig {
    fn default() -> Self {
        RecolourConfig {
            images: true,
            pdfs: true,
            diagrams: true,
        }
    }
}

/// How the PDF drawing tools behave. Global rather than per vault: a pen is a property of the
/// machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrawingConfig {
    /// Draw with the mouse or touchpad even while a pen is attached. Off, the hand selects text
    /// and only the pen draws — until no pen is attached at all, when the hand draws again.
    pub mouse: bool,
    /// Stroke widths in page points; the shapes draw in the pen's.
    pub pen_width: f32,
    pub highlighter_width: f32,
    /// `None` is the system accent, resolved when a stroke is written so the tool keeps following
    /// it; a colour picked on the ring is kept as it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pen_color: Option<[u8; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlighter_color: Option<[u8; 3]>,
    /// How close the eraser has to pass to a stroke to take it, in page points.
    pub eraser_radius: f32,
    /// Whether the eraser takes only the part of a stroke it passes over, leaving the rest as
    /// strokes of their own, rather than the whole stroke.
    pub eraser_partial: bool,
}

impl Default for DrawingConfig {
    fn default() -> Self {
        DrawingConfig {
            mouse: false,
            pen_width: 2.0,
            // A line of text, near enough.
            highlighter_width: 14.0,
            pen_color: None,
            highlighter_color: None,
            eraser_radius: 4.0,
            eraser_partial: false,
        }
    }
}

/// How a diagram's tools draw, as the ring's outer orbit last set them, and whether its grid
/// shows: global, as the PDF's picks are, so every diagram starts where the last one left off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DiagramConfig {
    /// A rectangle's corners are rounded.
    pub rounded: bool,
    pub route: Route,
    /// A connector ends in an arrow.
    pub arrow: bool,
    /// The canvas shows the page's grid at all times, not only while something is placed
    /// (Show Grid).
    pub grid: bool,
}

impl Default for DiagramConfig {
    fn default() -> Self {
        DiagramConfig {
            rounded: false,
            route: Route::Orthogonal,
            arrow: true,
            grid: false,
        }
    }
}

/// The way a new connector runs between its ends.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Route {
    Straight,
    #[default]
    Orthogonal,
    Curved,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            recent_vaults: Vec::new(),
            editor_font: None,
            spellcheck: true,
            word_suggestions: true,
            ghost_text: true,
            minimap: false,
            line_numbers: false,
            git_tree: true,
            show_hidden: true,
            git_tint: true,
            column_width: 50,
            full_width_preview: false,
            indent_width: 4,
            forward_keys_to_terminal: false,
            theme: Theme::System,
            focus_mode: FocusMode::default(),
            recolour: RecolourConfig::default(),
            shortcuts: BTreeMap::new(),
            search: SearchConfig::default(),
            drawing: DrawingConfig::default(),
            diagram: DiagramConfig::default(),
            lsp: LspConfig::default(),
            debug: DebugConfig::default(),
            vaults: BTreeMap::new(),
        }
    }
}

/// Per-vault preferences. All paths are vault-relative.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VaultConfig {
    pub templates_dir: String,
    /// Empty means the vault root.
    pub new_file_dir: String,
    /// Where an image pasted or dropped into a note is written, as Obsidian's setting of the same
    /// job reads: empty is the note's own folder, `./sub` a folder inside it, anything else a
    /// folder from the vault root ([`crate::attachment::folder`]).
    pub attachment_folder: String,
    #[serde(skip_serializing_if = "LspConfig::is_empty")]
    pub lsp: LspConfig,
    #[serde(skip_serializing_if = "DebugConfig::is_empty")]
    pub debug: DebugConfig,
}

/// Which language server answers for a language, where the built-in choice is not the one
/// wanted. Keyed by GtkSourceView language id (`python3`, `rust`), valued by a command line.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LspConfig {
    pub servers: BTreeMap<String, Vec<String>>,
}

impl LspConfig {
    /// Nothing chosen: written as nothing, rather than as a bare `lsp.servers` header.
    fn is_empty(&self) -> bool {
        self.servers.is_empty()
    }
}

/// Which debug adapter runs a language's programs, and the interpreter they run in, where the
/// built-in choice is not the one wanted. Adapters are keyed as [`LspConfig::servers`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebugConfig {
    pub adapters: BTreeMap<String, Vec<String>>,
    /// The Python a program runs in, a relative path with a `/` read from the vault root; empty
    /// is the nearest virtual environment above the program, else `python3`.
    pub python: Vec<String>,
}

impl DebugConfig {
    /// Nothing chosen: written as nothing, as [`LspConfig`] is.
    fn is_empty(&self) -> bool {
        self.adapters.is_empty() && self.python.is_empty()
    }
}

impl Default for VaultConfig {
    fn default() -> Self {
        VaultConfig {
            templates_dir: "Templates".to_string(),
            new_file_dir: String::new(),
            attachment_folder: String::new(),
            lsp: LspConfig::default(),
            debug: DebugConfig::default(),
        }
    }
}

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
    pub view: String,
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
            view: "editor".to_string(),
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

impl Config {
    /// Load defaults on a missing or broken config so hand-edit errors cannot prevent startup.
    ///
    /// An invalid file is first moved to `config.toml.broken`, preserving preferences for repair
    /// before a later save writes defaults.
    pub fn load() -> Config {
        let path = config_path();
        let text = std::fs::read_to_string(&path);
        let parsed = match &text {
            Ok(text) => Config::parse(text, &path),
            Err(e) => Err(Error::Io(format!("{}: {e}", path.display()))),
        };
        match parsed {
            Ok(c) => {
                set_known(text.ok());
                c
            }
            Err(e) => {
                if path.exists() {
                    let aside = path.with_extension("toml.broken");
                    match std::fs::rename(&path, &aside) {
                        Ok(()) => tracing::warn!(
                            "{}: {e}; using defaults, the file is kept as {}",
                            path.display(),
                            aside.display()
                        ),
                        Err(re) => tracing::warn!(
                            "{}: {e}; using defaults, and could not move it aside: {re}",
                            path.display()
                        ),
                    }
                }
                set_known(None);
                Config::default()
            }
        }
    }

    pub fn read(path: &Path) -> Result<Config> {
        let text = std::fs::read_to_string(path).map_err(|e| Error::io(path.display(), e))?;
        Config::parse(&text, path)
    }

    /// `text` as a config; `path` is only for the messages.
    fn parse(text: &str, path: &Path) -> Result<Config> {
        let table: toml::Table = text.parse().map_err(|e| unreadable(path, e))?;
        for key in unknown_keys(&table) {
            tracing::warn!("{}: unknown key `{key}` is ignored", path.display());
        }
        table.try_into().map_err(|e| unreadable(path, e))
    }

    /// Write the config, unless the file holds a change accent has not taken in yet
    /// ([`reread`](Self::reread)): then nothing is written and the answer is `false`. That is
    /// what keeps a hand edit — or a file that no longer parses — from being written over.
    pub fn save(&self) -> Result<bool> {
        let path = config_path();
        // An error rather than `None`: a file that cannot be read is not a file that is not
        // there, and reading the second out of the first wrote the defaults over a config whose
        // bytes nobody had seen.
        let on_disk = on_disk(&path)?;
        if on_disk.is_some() && on_disk != known() {
            return Ok(false);
        }
        let text = toml::to_string_pretty(self).map_err(|e| unreadable(&path, e))?;
        replace(&path, text.as_bytes())?;
        set_known(Some(text));
        Ok(true)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        let text = toml::to_string_pretty(self).map_err(|e| unreadable(path, e))?;
        replace(path, text.as_bytes())
    }

    /// Take in a change someone else made to `config.toml`: the file's config, with whatever
    /// accent changed since it last read or wrote the file kept on top, key by key. Where both
    /// changed one key the file wins, since that is what someone just typed, and the log names
    /// the key. `None` when the file holds nothing new; an error when it does not parse, and then
    /// nothing is taken in and [`save`](Self::save) keeps refusing until it does.
    pub fn reread(&self) -> Result<Option<Reread>> {
        let path = config_path();
        let text = on_disk(&path)?;
        let known = known();
        let Some(file) = theirs(text.as_deref(), known.as_deref(), &path)? else {
            return Ok(None);
        };
        // What accent last had on disk, which is what both sides moved on from.
        let base = known
            .as_deref()
            .and_then(|k| Config::parse(k, &path).ok())
            .unwrap_or_default();
        let value = |c: &Config| toml::Value::try_from(c).map_err(|e| unreadable(&path, e));
        let (base, ours, file) = (value(&base)?, value(self)?, value(&file)?);
        let mut lost = Vec::new();
        let merged = merge(Some(&base), Some(&ours), Some(&file), "", &mut lost)
            .ok_or_else(|| Error::Config("merging config.toml".to_string()))?;
        for key in lost {
            tracing::warn!(
                "config.toml: `{key}` was changed by hand and here at once; the file's value is kept"
            );
        }
        set_known(text);
        Ok(Some(Reread {
            unwritten: merged != file,
            config: merged.try_into().map_err(|e| unreadable(&path, e))?,
        }))
    }

    /// The stored entry for `root`, or the defaults.
    pub fn vault(&self, root: &Path) -> VaultConfig {
        self.vaults
            .get(&vault_key(root))
            .cloned()
            .unwrap_or_default()
    }

    /// What a vault on this machine opens with: its entry, its own servers and debug adapters over
    /// the global `[lsp.servers]` and `[debug.adapters]` key by key, and its interpreter if it
    /// names one. A remote vault takes [`Self::vault`] alone, its host's servers being the host's
    /// own.
    pub fn local_vault(&self, root: &Path) -> VaultConfig {
        let mut cfg = self.vault(root);
        for (language, argv) in &self.lsp.servers {
            cfg.lsp
                .servers
                .entry(language.clone())
                .or_insert_with(|| argv.clone());
        }
        for (language, argv) in &self.debug.adapters {
            cfg.debug
                .adapters
                .entry(language.clone())
                .or_insert_with(|| argv.clone());
        }
        if cfg.debug.python.is_empty() {
            cfg.debug.python = self.debug.python.clone();
        }
        cfg
    }

    pub fn set_vault(&mut self, root: &Path, cfg: VaultConfig) {
        self.vaults.insert(vault_key(root), cfg);
    }

    /// Move `root` to the front of the recent list, deduplicated.
    pub fn touch_recent(&mut self, root: &Path) {
        let path = PathBuf::from(vault_key(root));
        self.recent_vaults.retain(|v| v != &path);
        self.recent_vaults.insert(0, path);
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

/// Unknown or retired `config.toml` keys. Compare the raw top-level and vault tables with known
/// fields before Serde discards extras. Leave `daily_*` keys to [`daily_keys`] for migration advice.
fn unknown_keys(table: &toml::Table) -> Vec<String> {
    // The struct's own field names, read off a serialised value rather than kept as a second
    // list. `editor_font` is skipped when `None` and the maps when empty, so each is filled in
    // to be seen.
    fn names<T: Serialize>(v: T) -> Vec<String> {
        toml::Value::try_from(v)
            .ok()
            .and_then(|v| v.as_table().map(|t| t.keys().cloned().collect()))
            .unwrap_or_default()
    }
    let one = || BTreeMap::from([(String::new(), Vec::new())]);
    let debug = || DebugConfig {
        adapters: one(),
        ..DebugConfig::default()
    };
    let vault = VaultConfig {
        lsp: LspConfig { servers: one() },
        debug: debug(),
        ..VaultConfig::default()
    };
    let top = names(Config {
        editor_font: Some(String::new()),
        shortcuts: one(),
        lsp: LspConfig { servers: one() },
        debug: debug(),
        vaults: BTreeMap::from([(String::new(), vault.clone())]),
        ..Config::default()
    });
    let vault = names(vault);
    let mut out: Vec<String> = table.keys().filter(|k| !top.contains(k)).cloned().collect();
    if let Some(vaults) = table.get("vaults").and_then(toml::Value::as_table) {
        for (name, v) in vaults {
            let Some(v) = v.as_table() else { continue };
            out.extend(
                v.keys()
                    .filter(|k| !vault.contains(k) && !k.starts_with("daily_"))
                    .map(|k| format!("vaults.{name}.{k}")),
            );
        }
    }
    out
}

/// A change to `config.toml` taken in by [`Config::reread`].
#[derive(Debug)]
pub struct Reread {
    pub config: Config,
    /// Whether it holds a change of accent's the file does not, and so still wants a save.
    pub unwritten: bool,
}

/// The text accent last read from `config.toml` or wrote to it, so a change of someone else's can
/// be told from accent's own write coming back. Process-wide, like the file.
static KNOWN: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn known() -> Option<String> {
    KNOWN.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn set_known(text: Option<String>) {
    *KNOWN.lock().unwrap_or_else(|e| e.into_inner()) = text;
}

/// Read the file text, or `None` if absent. Unreadable or non-UTF-8 files return an error so
/// their unknown contents cannot be overwritten.
fn on_disk(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(Error::io(path.display(), e)),
    }
}

fn theirs(text: Option<&str>, known: Option<&str>, path: &Path) -> Result<Option<Config>> {
    match text {
        Some(text) if Some(text) != known => Config::parse(text, path).map(Some),
        _ => Ok(None),
    }
}

/// Three-way merge of two configs that both moved on from `base`, as TOML values, key by key down
/// through the tables: a key the file (`theirs`) changed takes the file's value, every other key
/// keeps accent's. A key both changed differently is pushed to `lost` by its dotted path, `at`
/// being where this value sits. `None` stands for a key that is absent.
fn merge(
    base: Option<&toml::Value>,
    ours: Option<&toml::Value>,
    theirs: Option<&toml::Value>,
    at: &str,
    lost: &mut Vec<String>,
) -> Option<toml::Value> {
    use toml::Value::Table;
    // An empty map is not written at all (`Config::shortcuts`), so a table on one side and
    // nothing on the other is that table emptied there, and its keys still merge one by one.
    // A table the merge leaves empty is left out again, as it would be written.
    let empty = Table(toml::Table::new());
    let (ours, theirs) = match (ours, theirs) {
        (Some(Table(_)), None) => (ours, Some(&empty)),
        (None, Some(Table(_))) => (Some(&empty), theirs),
        sides => sides,
    };
    if let (Some(Table(o)), Some(Table(t))) = (ours, theirs) {
        let b = match base {
            Some(Table(b)) => b.clone(),
            _ => toml::Table::new(),
        };
        let keys: std::collections::BTreeSet<&String> =
            b.keys().chain(o.keys()).chain(t.keys()).collect();
        let merged: toml::Table = keys
            .into_iter()
            .filter_map(|k| {
                let at = if at.is_empty() {
                    k.clone()
                } else {
                    format!("{at}.{k}")
                };
                Some((k.clone(), merge(b.get(k), o.get(k), t.get(k), &at, lost)?))
            })
            .collect();
        return (!merged.is_empty()).then_some(Table(merged));
    }
    if theirs == base {
        return ours.cloned();
    }
    if ours != base && ours != theirs {
        lost.push(at.to_string());
    }
    theirs.cloned()
}

/// Atomically write config or state bytes using a temporary file and rename. This prevents torn
/// files from loading as defaults and losing all preferences.
fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| Error::io(dir.display(), e))?;
    // A unique temp name: the GUI and `accent-cli`, or two windows, may write at once, and a
    // fixed `<name>.tmp` would have them tear each other's file.
    let write = || -> std::io::Result<()> {
        let mut tmp = tempfile::Builder::new()
            .prefix(".accent-")
            .tempfile_in(dir)?;
        tmp.write_all(bytes)?;
        tmp.as_file().sync_all()?;
        tmp.persist(path).map_err(|e| e.error)?;
        Ok(())
    };
    write().map_err(|e| Error::io(path.display(), e))
}

/// A config or state file that does not parse, or a config that does not serialise, by its path.
fn unreadable(path: &Path, e: impl std::fmt::Display) -> Error {
    Error::Config(format!("{}: {e}", path.display()))
}

/// `$XDG_<var>` when set and absolute, else `$HOME/<fallback>`, else the temp dir.
pub fn xdg(var: &str, home_fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| Path::new(&h).join(home_fallback)))
        .unwrap_or_else(std::env::temp_dir)
}

/// The canonical path a vault is keyed by, so the same tree reached through a symlink or a
/// relative path is one vault. Falls back to the path as given when it does not exist yet.
fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

fn vault_key(root: &Path) -> String {
    canonical(root).to_string_lossy().into_owned()
}

/// First 16 hex of the blake3 of the canonical vault path. Deliberately byte-for-byte what
/// `index::default_db_path` names its file, so a vault has one name across cache and state.
pub fn vault_hash(root: &Path) -> String {
    let digest = blake3::hash(canonical(root).as_os_str().as_encoded_bytes()).to_hex();
    digest[..16].to_string()
}

/// Migration advice for a vault's retired `daily_*` settings: their equivalent `accent-target`
/// and template.
///
/// Returns `None` when no retired key remains for `root`. A second TOML-table parse preserves
/// keys Serde discards. Nothing is written here; the user adds the directive, and the next
/// [`Config::save`] drops the old keys.
pub fn daily_keys(root: &Path) -> Option<(String, Option<String>)> {
    let text = std::fs::read_to_string(config_path()).ok()?;
    retired_daily(&text.parse().ok()?, &vault_key(root))
}

/// The pure half of [`daily_keys`], so the table shape is testable without a config file.
fn retired_daily(config: &toml::Table, key: &str) -> Option<(String, Option<String>)> {
    let vault = config.get("vaults")?.get(key)?;
    let at = |k| vault.get(k).and_then(toml::Value::as_str);
    let (dir, pattern, template) = (at("daily_dir"), at("daily_pattern"), at("daily_template"));
    (dir.is_some() || pattern.is_some() || template.is_some())
        .then(|| (daily_target(dir, pattern), template.map(str::to_string)))
}

/// The `accent-target` pattern a pair of old daily keys spelled, e.g. `Daily/{{date:%Y-%m-%d}}.md`.
/// An absent key means what its default meant.
pub fn daily_target(dir: Option<&str>, pattern: Option<&str>) -> String {
    let name = format!("{{{{date:{}}}}}.md", pattern.unwrap_or("%Y-%m-%d"));
    match dir.unwrap_or("Daily").trim_matches('/') {
        "" => name,
        dir => format!("{dir}/{name}"),
    }
}

pub fn config_path() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config")
        .join("accent")
        .join("config.toml")
}

pub fn state_path(root: &Path) -> PathBuf {
    state_dir().join(format!("{}.json", vault_hash(root)))
}

/// Where the sessions are written, one file per vault or terminal session, and nothing else.
fn state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("accent")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    const EXAMPLE: &str = example!();

    /// The XDG lookups read process-wide env vars, so the tests that set them take turns.
    static ENV: Mutex<()> = Mutex::new(());

    /// Point config and state at `dir` for the duration of `f`. The real `~/.config` is never
    /// read or written by these tests.
    fn with_xdg<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", dir);
            std::env::set_var("XDG_STATE_HOME", dir);
        }
        f()
    }

    #[test]
    fn daily_target_spells_what_the_old_keys_meant() {
        assert_eq!(
            daily_target(Some("Daily"), Some("%Y-%m-%d")),
            "Daily/{{date:%Y-%m-%d}}.md"
        );
        // Absent keys are the defaults they used to have.
        assert_eq!(daily_target(None, None), "Daily/{{date:%Y-%m-%d}}.md");
        // An empty folder was the vault root, and a root-relative target carries no leading slash.
        assert_eq!(daily_target(Some(""), Some("%d")), "{{date:%d}}.md");
    }

    #[test]
    fn retired_daily_keys_are_only_reported_where_they_are() {
        let with: toml::Table = r#"[vaults."/v"]
daily_pattern = "%d"
daily_template = "DailyNote.md"
"#
        .parse()
        .unwrap();
        assert_eq!(
            retired_daily(&with, "/v"),
            Some((
                "Daily/{{date:%d}}.md".to_string(),
                Some("DailyNote.md".to_string())
            ))
        );
        assert_eq!(retired_daily(&with, "/other"), None);

        let without: toml::Table = "[vaults.\"/v\"]\ntemplates_dir = \"T\"\n".parse().unwrap();
        assert_eq!(retired_daily(&without, "/v"), None);
        assert_eq!(retired_daily(&toml::Table::new(), "/v"), None);
    }

    #[test]
    fn config_roundtrip_parses_the_documented_example() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        std::fs::write(&p, EXAMPLE).unwrap();

        let c = Config::read(&p).unwrap();
        assert_eq!(c.recent_vaults, [PathBuf::from("/home/me/Notes")]);
        assert!(c.spellcheck);
        assert!(c.word_suggestions);
        assert!(c.ghost_text);
        assert!(!c.minimap);
        assert!(!c.line_numbers);
        assert!(c.git_tree);
        assert!(!c.show_hidden);
        assert!(!c.git_tint);
        assert_eq!(c.column_width, 50);
        assert!(c.full_width_preview);
        assert_eq!(c.indent_width, 4);
        assert!(!c.forward_keys_to_terminal);
        assert_eq!(c.theme, Theme::Solarized);
        assert_eq!(c.focus_mode, FocusMode::High);
        assert!(c.recolour.images && !c.recolour.pdfs && c.recolour.diagrams);
        assert_eq!(c.editor_font, None);
        let v = &c.vaults["/home/me/Notes"];
        assert_eq!(v.templates_dir, "Templates");
        assert_eq!(v.new_file_dir, "Inbox");
        assert_eq!(v.attachment_folder, "./attachments");
        // An override is a list, so an action can keep several chords, and an empty list is how
        // the user says "no shortcut at all" rather than "fall back to the default".
        assert_eq!(c.shortcuts["win.find-next"], ["F3"]);
        assert!(c.shortcuts["win.about"].is_empty());
        assert_eq!(c.search.exclude, ["Archive", "Code/vendor"]);
        assert!(!c.drawing.mouse);
        assert_eq!(c.drawing.pen_width, 2.0);
        assert_eq!(c.drawing.pen_color, None);
        assert_eq!(c.drawing.highlighter_color, Some([0, 0, 0]));
        assert_eq!(c.drawing.eraser_radius, 4.0);
        assert!(c.drawing.eraser_partial);
        let diagram = DiagramConfig {
            rounded: true,
            route: Route::Curved,
            arrow: false,
            grid: true,
        };
        assert_eq!(c.diagram, diagram);

        let back = tmp.path().join("written.toml");
        c.write(&back).unwrap();
        let again = Config::read(&back).unwrap();
        assert_eq!(again.recent_vaults, c.recent_vaults);
        assert_eq!(again.show_hidden, c.show_hidden);
        assert_eq!(again.theme, Theme::Solarized);
        assert_eq!(again.focus_mode, FocusMode::High);
        assert_eq!(again.vaults["/home/me/Notes"].new_file_dir, "Inbox");
        assert_eq!(again.shortcuts, c.shortcuts);
        assert_eq!(again.search.exclude, c.search.exclude);
        assert_eq!(again.drawing, c.drawing);
        assert_eq!(again.diagram, c.diagram);
    }

    /// An empty map is left out rather than written as a bare `[shortcuts]` header, and reads
    /// back as the empty map it was.
    #[test]
    fn a_default_config_writes_no_empty_table() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("config.toml");
        Config::default().write(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("[shortcuts]"), "{text}");
        assert!(!text.contains("[vaults"), "{text}");
        assert_eq!(Config::read(&p).unwrap(), Config::default());
        assert!(unknown_keys(&text.parse().unwrap()).is_empty());

        // Nor a vault's `lsp.servers` while it names no server.
        let mut c = Config::default();
        c.vaults.insert("/v".into(), VaultConfig::default());
        c.write(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert!(!text.contains("lsp"), "{text}");
        assert_eq!(Config::read(&p).unwrap(), c);
    }

    #[test]
    fn touch_moves_to_the_front_and_caps() {
        let mut list = Vec::new();
        for i in 0..12 {
            touch(&mut list, &format!("n{i}.md"), 10);
        }
        touch(&mut list, "n5.md", 10);
        assert_eq!(list.len(), 10);
        assert_eq!(list[0], "n5.md");
        assert_eq!(list[1], "n11.md");
        assert_eq!(list.iter().filter(|n| *n == "n5.md").count(), 1);
    }

    #[test]
    fn rename_in_follows_a_note_and_a_folder() {
        let mut list = vec![
            "Inbox/idea.md".to_string(),
            "Areas/Work/plan.md".to_string(),
            "Areas/Workshop.md".to_string(),
        ];
        rename_in(&mut list, "Areas/Work", "Areas/Job");
        rename_in(&mut list, "Inbox/idea.md", "Inbox/thought.md");
        assert_eq!(
            list,
            [
                "Inbox/thought.md",
                "Areas/Job/plan.md",
                // A sibling that merely starts with the old name is not part of the folder.
                "Areas/Workshop.md"
            ]
        );
    }

    #[test]
    fn rename_keys_moves_a_file_and_a_folder() {
        let mut map = BTreeMap::from([
            ("run.py".to_string(), vec![1]),
            ("pkg/a.py".to_string(), vec![2]),
            ("pkg/sub/b.py".to_string(), vec![3]),
            ("pkgs.py".to_string(), vec![4]),
        ]);
        rename_keys(&mut map, "run.py", "main.py");
        rename_keys(&mut map, "pkg", "lib");
        assert_eq!(
            map,
            BTreeMap::from([
                ("main.py".to_string(), vec![1]),
                ("lib/a.py".to_string(), vec![2]),
                ("lib/sub/b.py".to_string(), vec![3]),
                // A sibling that merely starts with the old name is not part of the folder.
                ("pkgs.py".to_string(), vec![4]),
            ])
        );
    }

    #[test]
    fn missing_config_file_is_default() {
        let tmp = tempfile::tempdir().unwrap();
        let c = with_xdg(tmp.path(), Config::load);
        assert!(c.spellcheck);
        assert!(c.show_hidden);
        assert!(c.git_tint);
        assert!(!c.full_width_preview);
        assert_eq!(c.focus_mode, FocusMode::Medium);
        assert!(c.recent_vaults.is_empty());
        assert!(!config_path_exists(tmp.path()));
    }

    /// An invalid config must be preserved before defaults can be saved.
    #[test]
    fn broken_config_file_is_default_and_kept_aside_for_the_next_save() {
        let tmp = tempfile::tempdir().unwrap();
        let c = with_xdg(tmp.path(), || {
            let p = config_path();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "recent_vaults = [ oops").unwrap();
            let c = Config::load();
            c.save().unwrap();
            c
        });
        assert!(c.spellcheck);
        assert!(c.recent_vaults.is_empty());
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("accent/config.toml.broken")).unwrap(),
            "recent_vaults = [ oops",
            "the user's file survives the save that follows"
        );
    }

    #[test]
    fn only_a_parsing_change_of_someone_elses_is_taken_in() {
        let p = Path::new("config.toml");
        let known = "minimap = false\n";
        // Nothing there, or accent's own write coming back: nothing to take in.
        assert!(theirs(None, Some(known), p).unwrap().is_none());
        assert!(theirs(Some(known), Some(known), p).unwrap().is_none());
        let changed = theirs(Some("minimap = true\n"), Some(known), p).unwrap();
        assert!(changed.is_some_and(|c| c.minimap));
        // A file accent never read is someone else's too.
        assert!(theirs(Some("minimap = true\n"), None, p).unwrap().is_some());
        assert!(theirs(Some("minimap = [ oops"), Some(known), p).is_err());
    }

    #[test]
    fn a_merge_keeps_both_sides_and_the_file_wins_a_key_both_changed() {
        let value = |c: Config| toml::Value::try_from(c).unwrap();
        let base = Config::default();
        let mut ours = base.clone();
        ours.minimap = true;
        ours.drawing.pen_width = 3.0;
        ours.theme = Theme::Light;
        let mut file = base.clone();
        file.drawing.mouse = true;
        file.theme = Theme::Dark;

        let mut lost = Vec::new();
        let merged = merge(
            Some(&value(base)),
            Some(&value(ours)),
            Some(&value(file)),
            "",
            &mut lost,
        );
        let merged: Config = merged.unwrap().try_into().unwrap();
        assert!(merged.minimap);
        // Down through a table: one key from each side.
        assert_eq!(merged.drawing.pen_width, 3.0);
        assert!(merged.drawing.mouse);
        assert_eq!(merged.theme, Theme::Dark);
        assert_eq!(lost, ["theme"]);
    }

    /// A map emptied on one side is not written at all, and still merges key by key with the
    /// other side's change: accent's last shortcut unbound while one was added by hand.
    #[test]
    fn a_map_emptied_on_one_side_merges_key_by_key() {
        let value = |c: &Config| toml::Value::try_from(c).unwrap();
        let mut base = Config::default();
        base.shortcuts.insert("win.a".into(), vec!["F1".into()]);
        let mut ours = base.clone();
        ours.shortcuts.clear();
        let mut file = base.clone();
        file.shortcuts.insert("win.b".into(), vec!["F2".into()]);

        let mut lost = Vec::new();
        let merged = merge(
            Some(&value(&base)),
            Some(&value(&ours)),
            Some(&value(&file)),
            "",
            &mut lost,
        );
        let merged: Config = merged.unwrap().try_into().unwrap();
        assert_eq!(
            merged.shortcuts,
            BTreeMap::from([("win.b".into(), vec!["F2".into()])])
        );
        assert!(lost.is_empty(), "{lost:?}");

        // Emptied by accent alone, it is left out of the merge, as it is out of the file.
        let merged = merge(
            Some(&value(&base)),
            Some(&value(&ours)),
            Some(&value(&base)),
            "",
            &mut lost,
        );
        assert!(merged.unwrap().get("shortcuts").is_none());
    }

    /// A hand edit made while accent runs must survive the next save.
    #[test]
    fn a_hand_edit_is_never_written_over_and_is_taken_in_under_accents_change() {
        let tmp = tempfile::tempdir().unwrap();
        with_xdg(tmp.path(), || {
            let p = config_path();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "minimap = false\n").unwrap();
            let mut c = Config::load();
            assert!(c.reread().unwrap().is_none(), "nothing new yet");

            c.line_numbers = true;
            std::fs::write(&p, "minimap = true\n").unwrap();
            assert!(!c.save().unwrap(), "the hand edit is not written over");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "minimap = true\n");

            let taken = c.reread().unwrap().unwrap();
            assert!(taken.config.minimap && taken.config.line_numbers);
            assert!(taken.unwritten);
            assert!(
                taken.config.save().unwrap(),
                "taken in, so saving goes ahead"
            );
            assert!(Config::read(&p).unwrap().line_numbers);
            assert!(taken.config.reread().unwrap().is_none(), "its own write");
        });
    }

    /// An unreadable config must block saving; treating it as absent would overwrite unknown
    /// contents.
    #[test]
    fn a_config_that_cannot_be_read_is_not_written_over() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        with_xdg(tmp.path(), || {
            let p = config_path();
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "minimap = false\n").unwrap();
            let mut c = Config::load();
            c.line_numbers = true;

            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
            // Root reads it anyway, and then there is nothing to test.
            if std::fs::read_to_string(&p).is_ok() {
                return;
            }
            assert!(c.save().is_err(), "an unreadable config is not written");
            assert!(c.reread().is_err(), "nor read as holding nothing new");

            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(std::fs::read_to_string(&p).unwrap(), "minimap = false\n");
        });
    }

    #[test]
    fn unknown_keys_are_named_at_both_levels() {
        let table: toml::Table = r#"
spellcheck = true
new_note_dir = "Inbox"
[vaults."/v"]
new_note_dir = "Inbox"
new_file_dir = "Inbox"
daily_dir = "Daily"
"#
        .parse()
        .unwrap();
        assert_eq!(
            unknown_keys(&table),
            ["new_note_dir", "vaults./v.new_note_dir"]
        );
        assert!(unknown_keys(&EXAMPLE.parse().unwrap()).is_empty());
    }

    fn config_path_exists(base: &Path) -> bool {
        base.join("accent/config.toml").exists()
    }

    #[test]
    fn vault_lookup_by_canonical_path() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("Notes");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let mut c = Config::default();
        c.set_vault(
            &link,
            VaultConfig {
                new_file_dir: "Inbox".to_string(),
                ..Default::default()
            },
        );
        assert_eq!(c.vaults.len(), 1);
        assert_eq!(c.vault(&real).new_file_dir, "Inbox");
        assert_eq!(c.vault(&tmp.path().join("Other")).new_file_dir, "");
    }

    /// The global table sits under a vault's own, key by key, and only where it is asked for.
    #[test]
    fn a_local_vault_takes_the_global_servers_under_its_own() {
        let c: Config = toml::from_str(EXAMPLE).unwrap();
        let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let notes = Path::new("/home/me/Notes");
        let local = c.local_vault(notes).lsp.servers;
        assert_eq!(local["python3"], argv(&["pylsp"]));
        assert_eq!(local["rust"], argv(&["rust-analyzer"]));
        assert_eq!(c.vault(notes).lsp.servers.len(), 1);
        let other = c.local_vault(Path::new("/elsewhere"));
        assert_eq!(other.lsp, c.lsp);

        // The debugger's table merges the same way; the interpreter is the vault's when it says one.
        let debug = c.local_vault(notes).debug;
        assert_eq!(
            debug.adapters["python3"],
            argv(&["python3", "-m", "debugpy.adapter"])
        );
        assert_eq!(debug.python, argv(&[".venv/bin/python"]));
        assert_eq!(other.debug, c.debug);
    }

    #[test]
    fn touch_recent_dedups_and_keeps_every_vault() {
        let mut c = Config::default();
        for i in 0..12 {
            c.touch_recent(Path::new(&format!("/vault/{i}")));
        }
        c.touch_recent(Path::new("/vault/5"));
        assert_eq!(c.recent_vaults.len(), 12);
        assert_eq!(c.recent_vaults[0], PathBuf::from("/vault/5"));
        assert_eq!(c.recent_vaults[1], PathBuf::from("/vault/11"));
        assert_eq!(
            c.recent_vaults.iter().filter(|p| p.ends_with("5")).count(),
            1
        );
    }

    #[test]
    fn vault_hash_matches_the_index_cache_name() {
        let tmp = tempfile::tempdir().unwrap();
        let db = crate::index::default_db_path(tmp.path());
        assert_eq!(db.file_stem().unwrap(), vault_hash(tmp.path()).as_str());
    }

    #[test]
    fn session_roundtrips_as_json() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let state = tmp.path().join("state");

        let s = Session {
            open: vec!["Daily/2026-09-03.md".to_string(), "b.md".to_string()],
            active: Some("Daily/2026-09-03.md".to_string()),
            layout: Some(split(
                true,
                0.3,
                pane(&["Daily/2026-09-03.md"], Some("Daily/2026-09-03.md")),
                pane(&["b.md"], None),
            )),
            sidebar: false,
            sidebar_width: 320,
            info: InfoPane {
                references: false,
                tags: true,
                details: true,
                dividers: [Some(0.25), None],
            },
            code: CodePane {
                goals: false,
                expected: true,
                stack: true,
                variables: false,
                watch: true,
                breakpoints: true,
                dividers: vec![Some(0.4)],
            },
            view: "preview".to_string(),
            zoom: 1.2,
            recent_files: vec!["Daily/2026-09-03.md".to_string()],
            recent_commands: vec!["win.save".to_string()],
            pdf: BTreeMap::from([(
                "Attachments/paper.pdf".to_string(),
                PdfPlace {
                    page: 4,
                    zoom: PdfZoom::Scale(1.5),
                },
            )]),
            diagram: BTreeMap::from([(
                "Figures/flow.drawio".to_string(),
                DiagramPlace {
                    page: 2,
                    zoom: Some(0.75),
                    x: 10.0,
                    y: 20.0,
                },
            )]),
            terminals: BTreeMap::from([(
                "terminal:0123456789abcdef".to_string(),
                ShellPlace {
                    at: "/home/me/src".to_string(),
                },
            )]),
            pinned: vec!["b.md".to_string()],
            web_images: vec!["Figures/flow.drawio".to_string()],
            compared: BTreeMap::from([(
                "diff:commit:abc1234:a.md".to_string(),
                Comparison {
                    repo: crate::git::Repo {
                        root: PathBuf::from("/vault"),
                        git_dir: PathBuf::from("/vault/.git"),
                        name: "vault".to_string(),
                    },
                    rel: "a.md".to_string(),
                    key: "a.md".to_string(),
                    sides: crate::git::Sides::Commit {
                        oid: "abc1234".to_string(),
                        parent: None,
                        orig: Some("old.md".to_string()),
                    },
                },
            )]),
            breakpoints: BTreeMap::from([(
                "run.py".to_string(),
                vec![
                    Breakpoint::at(0),
                    Breakpoint {
                        condition: Some("x > 1".to_string()),
                        log_message: Some("{x}".to_string()),
                        ..Breakpoint::at(7)
                    },
                ],
            )]),
            watches: vec!["len(items)".to_string()],
            exceptions: vec![ExceptionFilter {
                filter: "raised".to_string(),
                label: "Raised Exceptions".to_string(),
                description: None,
                on: true,
            }],
        };
        with_xdg(&state, || {
            assert_eq!(Session::load(&vault).open, Vec::<String>::new());
            s.save(&vault).unwrap();
            let path = state_path(&vault);
            assert!(path.starts_with(state.join("accent")));
            assert_eq!(path.file_stem().unwrap(), vault_hash(&vault).as_str());

            let back = Session::load(&vault);
            assert_eq!(back.open, s.open);
            assert_eq!(back.active, s.active);
            assert_eq!(back.layout, s.layout);
            assert_eq!(back.pdf, s.pdf);
            assert_eq!(back.diagram, s.diagram);
            assert!(!back.sidebar);
            assert_eq!(back.sidebar_width, 320);
            assert_eq!(back.info, s.info);
            assert_eq!(back.code, s.code);
            assert_eq!(back.view, "preview");
            assert_eq!(back.zoom, 1.2);
            assert_eq!(back.recent_files, s.recent_files);
            assert_eq!(back.recent_commands, s.recent_commands);
            assert_eq!(back.terminals, s.terminals);
            assert_eq!(back.pinned, s.pinned);
            assert_eq!(back.web_images, s.web_images);
            assert_eq!(back.compared, s.compared);
            assert_eq!(back.breakpoints, s.breakpoints);
            assert_eq!(back.watches, s.watches);
            assert_eq!(back.exceptions, s.exceptions);

            // A terminal session's key is no path: it hashes as given, as an `ssh://` one does.
            let named = Path::new("terminal://dev");
            s.save(named).unwrap();
            assert_eq!(
                state_path(named).file_stem().unwrap(),
                vault_hash(named).as_str()
            );
            assert_eq!(Session::load(named).terminals, s.terminals);
        });
    }

    /// A state file from another version must still load: one written before `zoom` existed gets
    /// the default, the `pane` every file written until now carries is simply dropped — which
    /// sidebar pane was showing is no longer restored — and `recent_notes` is `recent_files`.
    #[test]
    fn session_from_another_version_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let vault = tmp.path().join("vault");
        std::fs::create_dir(&vault).unwrap();
        let state = tmp.path().join("state");

        with_xdg(&state, || {
            let path = state_path(&vault);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(
                &path,
                r#"{"open":["a.md"],"active":"a.md","sidebar":true,"sidebar_width":280,"view":"editor","pane":"git","recent_notes":["a.md","b.pdf"],"breakpoints":{"run.py":[0,7]}}"#,
            )
            .unwrap();

            let back = Session::load(&vault);
            assert_eq!(back.open, ["a.md"]);
            assert_eq!(back.zoom, 1.0);
            assert_eq!(back.recent_files, ["a.md", "b.pdf"]);
            // Breakpoints kept as bare lines, before they had settings, are gone; the rest stays.
            assert!(back.breakpoints.is_empty());
            // No layout was written then, so every tab comes back into one pane.
            assert_eq!(back.layout, None);
            assert_eq!(back.panes(), Some(pane(&["a.md"], None)));
        });
    }

    fn pane(tabs: &[&str], selected: Option<&str>) -> Layout {
        Layout::Pane {
            tabs: tabs.iter().map(|t| t.to_string()).collect(),
            selected: selected.map(str::to_string),
        }
    }

    fn split(vertical: bool, ratio: f64, start: Layout, end: Layout) -> Layout {
        Layout::Split {
            vertical,
            ratio,
            start: Box::new(start),
            end: Box::new(end),
        }
    }

    fn open(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| k.to_string()).collect()
    }

    /// `[a b | [c / d]]`, the right-hand side stacked, a third of the width on the left.
    fn stored() -> Layout {
        split(
            false,
            0.33,
            pane(&["a.md", "b.md"], Some("b.md")),
            split(true, 0.5, pane(&["c.md"], None), pane(&["d.md"], None)),
        )
    }

    #[test]
    fn a_layout_takes_exactly_the_open_tabs() {
        // Everything still open: nothing moves.
        let all = open(&["a.md", "b.md", "c.md", "d.md"]);
        assert_eq!(stored().place(&all, Some("b.md")), Some(stored()));

        // b closed and e opened by a window that never restored: b goes, and so does the pane
        // selection it was; e joins the pane of the active tab.
        let placed = stored().place(&open(&["a.md", "c.md", "d.md", "e.md"]), Some("d.md"));
        assert_eq!(
            placed,
            Some(split(
                false,
                0.33,
                pane(&["a.md"], None),
                split(
                    true,
                    0.5,
                    pane(&["c.md"], None),
                    pane(&["d.md", "e.md"], None)
                ),
            ))
        );

        // An active tab the layout does not hold sends the newcomers to the first pane.
        let placed = stored().place(&open(&["a.md", "c.md", "d.md", "e.md"]), Some("e.md"));
        assert_eq!(
            placed,
            Some(split(
                false,
                0.33,
                pane(&["a.md", "e.md"], None),
                split(true, 0.5, pane(&["c.md"], None), pane(&["d.md"], None)),
            ))
        );
    }

    #[test]
    fn an_empty_pane_collapses_into_its_sibling() {
        // d gone: the stacked split is c alone, and the outer split keeps its ratio.
        assert_eq!(
            stored().place(&open(&["a.md", "b.md", "c.md"]), None),
            Some(split(
                false,
                0.33,
                pane(&["a.md", "b.md"], Some("b.md")),
                pane(&["c.md"], None)
            ))
        );
        // Only the stacked side left: no split at all.
        assert_eq!(
            stored().place(&open(&["d.md", "c.md"]), None),
            Some(split(
                true,
                0.5,
                pane(&["c.md"], None),
                pane(&["d.md"], None)
            ))
        );
        assert_eq!(stored().place(&[], None), None);
        // Nothing of the layout left, but a tab to show: one pane.
        assert_eq!(
            stored().place(&open(&["e.md"]), Some("e.md")),
            Some(pane(&["e.md"], None))
        );
    }

    #[test]
    fn a_ratio_is_kept_off_the_edges() {
        let thin = split(false, 0.01, pane(&["a.md"], None), pane(&["b.md"], None));
        let placed = thin.place(&open(&["a.md", "b.md"]), None);
        assert_eq!(
            placed,
            Some(split(
                false,
                0.1,
                pane(&["a.md"], None),
                pane(&["b.md"], None)
            ))
        );
    }
}
