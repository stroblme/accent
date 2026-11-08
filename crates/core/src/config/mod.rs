//! Global settings and per-vault session state.
//!
//! Config stays outside the vault so preferences do not sync through Syncthing or git.
//! `~/.config/accent/config.toml` is documented by [`Config`] with a worked example.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// A macro supplies the literal required by the doc attribute; tests parse the same example.
macro_rules! example {
    () => {
        r#"recent_vaults = ["/home/me/Notes"]
preview_font = "Serif 12"
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
debug_terminal = "down"
editor = "helix"
theme = "solarized"
focus_mode = "high"

[recolour]
images = true
pdfs = false
diagrams = true

[shortcuts]
"win.find-next" = ["F3"]
"win.about" = []

[[commands]]
name = "Claude"
run = "claude"
shortcut = "<Control><Shift>j"

[[commands]]
name = "Build"
run = "make -j8"

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

[lsp]
ghost = ["~/merl/build/merl-rt"]

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

// After `example!`, which the tests read.
mod file;
mod session;
#[cfg(test)]
mod tests;

pub use file::{Reread, config_path, daily_keys, daily_target, state_path, vault_hash, xdg};
pub use session::{
    Breakpoint, CodePane, DiagramPlace, Edits, ExceptionFilter, InfoPane, Layout, PdfPlace,
    PdfZoom, Session, ShellPlace, rename_in, rename_keys, touch,
};

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

/// Where a debugged program's terminal and the Debug Console open: behind the tab in front of
/// the active pane, or in a pane split off it on a side. The keyboard stays where it was either
/// way, so the Code pane stays up over the file being debugged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DebugTerminal {
    #[default]
    Same,
    Right,
    Left,
    Up,
    Down,
}

/// What opens a text file: accent's own editor, or a terminal editor in a terminal tab
/// (DESIGN.md, Terminal).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Editor {
    #[default]
    Accent,
    Helix,
    Neovim,
    Vim,
    Kakoune,
    Emacs,
    Nano,
    Micro,
    /// The user's own command line, [`Config::editor_command`].
    Custom,
}

impl Editor {
    /// Accent's own, which the file leaves out.
    pub fn is_accent(&self) -> bool {
        *self == Editor::Accent
    }
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
    /// The rendered preview's prose; `None` follows the GNOME document font. Code keeps the
    /// monospace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_font: Option<String>,
    /// Pattern used for newly generated bibliography keys; existing keys are never changed on save.
    pub bibliography_key_pattern: String,
    /// Add Reference tries to download the new reference's PDF; the dialog's switch remembers it.
    pub bibliography_download_pdf: bool,
    /// Sent to Unpaywall for open-access PDF lookups, as the Download PDF dialog last had it.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub bibliography_unpaywall_email: String,
    pub spellcheck: bool,
    /// Offer words as one is typed in prose: the document's own, then the system dictionary's.
    pub word_suggestions: bool,
    /// Suggest the rest of the line as the caret sits, from what the vault already says, by the
    /// merl compiled into `accent-cli` ([`LspConfig::ghost`] names another).
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
    pub debug_terminal: DebugTerminal,
    /// The editor a text file opens in.
    #[serde(skip_serializing_if = "Editor::is_accent")]
    pub editor: Editor,
    /// The command line `editor = "custom"` runs, `{file}` and `{line}` filled in.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub editor_command: String,
    pub theme: Theme,
    pub focus_mode: FocusMode,
    pub recolour: RecolourConfig,
    /// Accelerator overrides, keyed by full action name ("win.save"). Only what the user changed
    /// is stored, so the built-in table stays the source of truth for everything else; an empty
    /// list means the action is deliberately unbound.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub shortcuts: BTreeMap<String, Vec<String>>,
    /// The user's own commands, `[[commands]]` in the file, listed by Run Command in this order.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<CustomCommand>,
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

/// A command line the user named, which Run Command and its shortcut run in a terminal tab of its
/// own at the vault root (DESIGN.md, Terminal).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomCommand {
    /// What Run Command lists it as and its tab is titled with.
    pub name: String,
    /// A shell command line, run by the user's login shell.
    pub run: String,
    /// One accelerator in GTK's syntax (`<Control><Shift>j`), as `[shortcuts]` writes them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut: Option<String>,
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
    /// The size typed text is set in, in points.
    pub text_size: f32,
    /// `None` is the accent, as for the pen.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_color: Option<[u8; 3]>,
}

impl Default for DrawingConfig {
    fn default() -> Self {
        DrawingConfig {
            mouse: false,
            pen_width: 1.5,
            // A line of text, near enough.
            highlighter_width: 10.5,
            pen_color: None,
            highlighter_color: None,
            eraser_radius: 3.0,
            eraser_partial: false,
            text_size: 14.0,
            text_color: None,
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
            preview_font: None,
            bibliography_key_pattern: crate::bib::PATTERN.into(),
            bibliography_download_pdf: true,
            bibliography_unpaywall_email: String::new(),
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
            debug_terminal: DebugTerminal::Same,
            editor: Editor::Accent,
            editor_command: String::new(),
            theme: Theme::System,
            focus_mode: FocusMode::default(),
            recolour: RecolourConfig::default(),
            shortcuts: BTreeMap::new(),
            commands: Vec::new(),
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
    /// The ghost-text server's command line, `--vault <root>` added, in place of the bundled
    /// `accent-cli ghost`: a merl build of one's own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ghost: Vec<String>,
    pub servers: BTreeMap<String, Vec<String>>,
}

impl LspConfig {
    /// Nothing chosen: written as nothing, rather than as a bare `lsp.servers` header.
    fn is_empty(&self) -> bool {
        self.ghost.is_empty() && self.servers.is_empty()
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
