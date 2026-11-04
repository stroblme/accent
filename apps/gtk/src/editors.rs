//! Terminal editors (Helix, Vim, …) a text file opens in, in a terminal tab of its own
//! (DESIGN.md, Terminal): the ones accent knows, and which of them the user's shell finds.

use accent_core::config::Editor;
use gtk::glib;
use std::cell::RefCell;
use std::rc::Rc;

/// A terminal editor accent knows: what Preferences calls it, and the programs it goes by, the
/// first one the shell finds being the one run.
struct Known {
    editor: Editor,
    name: &'static str,
    binaries: &'static [&'static str],
}

const KNOWN: [Known; 7] = [
    Known {
        editor: Editor::Helix,
        name: "Helix",
        binaries: &["hx", "helix"],
    },
    Known {
        editor: Editor::Neovim,
        name: "Neovim",
        binaries: &["nvim"],
    },
    Known {
        editor: Editor::Vim,
        name: "Vim",
        binaries: &["vim"],
    },
    Known {
        editor: Editor::Kakoune,
        name: "Kakoune",
        binaries: &["kak"],
    },
    Known {
        editor: Editor::Emacs,
        name: "Emacs",
        binaries: &["emacs"],
    },
    Known {
        editor: Editor::Nano,
        name: "Nano",
        binaries: &["nano"],
    },
    Known {
        editor: Editor::Micro,
        name: "Micro",
        binaries: &["micro"],
    },
];

/// An editor the shell finds, by the program it found it as.
pub type Found = (Editor, &'static str);

/// What Preferences calls `editor`.
pub fn name(editor: Editor) -> &'static str {
    KNOWN
        .iter()
        .find(|known| known.editor == editor)
        .map_or("Accent", |known| known.name)
}

/// What the probe prints before each program it finds, so the lines a chatty rc file prints are
/// not taken for an answer.
const MARK: &str = "accent-editor";

/// Ask the login shell a terminal tab runs (`$SHELL -l -i -c`, on the host from the Flatpak) for
/// every program [`KNOWN`] names, so its `PATH` and aliases are the ones a tab will have. Blocks.
fn probe() -> Vec<Found> {
    let script: Vec<String> = KNOWN
        .iter()
        .flat_map(|known| known.binaries)
        .map(|bin| format!("command -v {bin} >/dev/null 2>&1 && echo {MARK} {bin}"))
        .collect();
    let host = std::fs::read_to_string("/.flatpak-info")
        .ok()
        .and_then(|info| crate::terminal::host_prefix(&info));
    let mut argv = host.unwrap_or_default();
    argv.extend(
        [
            "sh",
            "-c",
            r#"exec "${SHELL:-/bin/sh}" -l -i -c "$1""#,
            "sh",
            &script.join("; "),
        ]
        .map(str::to_string),
    );
    let out = std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    match out {
        Ok(out) => parse(&String::from_utf8_lossy(&out.stdout)),
        Err(e) => {
            tracing::warn!("cannot ask the shell for terminal editors: {e}");
            Vec::new()
        }
    }
}

/// The editors the probe's output names, each by the first of its programs found.
fn parse(out: &str) -> Vec<Found> {
    let found: Vec<&str> = out
        .lines()
        .filter_map(|line| line.trim().strip_prefix(MARK)?.strip_prefix(' '))
        .collect();
    KNOWN
        .iter()
        .filter_map(|known| {
            let bin = known.binaries.iter().find(|bin| found.contains(bin))?;
            Some((known.editor, *bin))
        })
        .collect()
}

/// What waits for the probe's answer.
type Waiting = Box<dyn FnOnce(&[Found])>;

/// The probe, asked once a process.
enum Probe {
    Idle,
    Running(Vec<Waiting>),
    Done(Rc<[Found]>),
}

thread_local! {
    static PROBE: RefCell<Probe> = const { RefCell::new(Probe::Idle) };
}

/// Call `then` with the editors the shell finds: at once once the probe has answered, else when
/// it does, the probe starting off the main thread the first time.
pub fn installed(then: impl FnOnce(&[Found]) + 'static) {
    let mut then: Option<Waiting> = Some(Box::new(then));
    let (done, start) = PROBE.with_borrow_mut(|probe| match probe {
        Probe::Done(found) => (Some(found.clone()), false),
        Probe::Running(waiting) => {
            waiting.extend(then.take());
            (None, false)
        }
        Probe::Idle => {
            *probe = Probe::Running(then.take().into_iter().collect());
            (None, true)
        }
    });
    if let (Some(found), Some(then)) = (done, then) {
        then(&found);
    }
    if start {
        glib::spawn_future_local(async {
            let found: Rc<[Found]> = crate::work::off_thread("editor probe", probe)
                .await
                .unwrap_or_default()
                .into();
            let waiting = PROBE.with_borrow_mut(|probe| {
                match std::mem::replace(probe, Probe::Done(found.clone())) {
                    Probe::Running(waiting) => waiting,
                    _ => Vec::new(),
                }
            });
            for then in waiting {
                then(&found);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An rc file may print anything; only the marked lines answer, and Helix is run by the first
    /// of its names the shell finds.
    #[test]
    fn the_probe_reads_its_own_lines_and_the_first_name_wins() {
        let out = "Welcome back!\naccent-editor helix\naccent-editor hx\n\
                   accent-editor vim\nhx is an alias\naccent-editor nanos\n";
        assert_eq!(
            parse(out),
            [(Editor::Helix, "hx"), (Editor::Vim, "vim")],
            "nanos is no nano"
        );
        assert_eq!(parse("accent-editor helix\n"), [(Editor::Helix, "helix")]);
        assert!(parse("").is_empty());
    }
}
