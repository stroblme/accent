//! Jupyter kernelspecs: `kernels/<name>/kernel.json` in the folders jupyter_core reads them from,
//! for a notebook in another language than Python (user decision 2026-10-27).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// A kernel a notebook can run on, as its `kernel.json` describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    /// Its folder's name, lowercased as jupyter_client keys it: what `kernelspec.name` names.
    pub name: String,
    pub display_name: String,
    pub language: String,
    /// Its folder as the machine that runs it names it, for `{resource_dir}`.
    pub dir: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// `interrupt_mode: "message"`: interrupted with an `interrupt_request` on control, not SIGINT.
    pub message_interrupt: bool,
}

/// The kernelspecs found, the first of a name winning, and the folders they were looked for in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Specs {
    pub specs: Vec<Spec>,
    pub searched: Vec<String>,
}

/// The kernelspec a notebook in `language` runs on: the one the reader `chosen`, else the one its
/// metadata `named`, else the first of its language; each only if it is of that language.
pub fn pick<'a>(
    specs: &'a [Spec],
    language: &str,
    chosen: Option<&str>,
    named: Option<&str>,
) -> Option<&'a Spec> {
    let by_name = |name: Option<&str>| {
        let name = name?;
        of_language(specs, language).find(|s| s.name.eq_ignore_ascii_case(name))
    };
    let first = || of_language(specs, language).next();
    by_name(chosen).or_else(|| by_name(named)).or_else(first)
}

/// Those of `specs` in `language`, which kernelspecs spell as they like (`R`, `julia`).
pub fn of_language<'a>(specs: &'a [Spec], language: &str) -> impl Iterator<Item = &'a Spec> {
    specs
        .iter()
        .filter(move |s| s.language.eq_ignore_ascii_case(language))
}

/// The kernelspecs on this machine, for a vault at `root`.
pub(crate) fn find(root: &Path) -> Specs {
    let sandbox = std::env::var_os("FLATPAK_ID").is_some() && Path::new("/.flatpak-info").exists();
    let venvs = crate::python::venvs(root);
    let dirs = dirs(|key| std::env::var(key).ok(), sandbox, &venvs);
    Specs {
        specs: read(&dirs),
        searched: dirs
            .iter()
            .map(|d| d.to_string_lossy().into_owned())
            .collect(),
    }
}

/// The folders holding `kernels/`, in jupyter_core's order (`jupyter_path`): each of
/// `JUPYTER_PATH`, the user's data folder, the vault's virtual environments' `share/jupyter`
/// (where `--sys-prefix` installs one), then the system's. In the Flatpak (`sandbox`) the data
/// folder is the app's own, so the user's is the default one, and the host's `/usr` is under
/// `/run/host`; the host's `JUPYTER_PATH` is out of sight.
fn dirs(var: impl Fn(&str) -> Option<String>, sandbox: bool, venvs: &[PathBuf]) -> Vec<PathBuf> {
    let var = |key: &str| var(key).filter(|value| !value.is_empty());
    let mut dirs: Vec<PathBuf> = match var("JUPYTER_PATH") {
        Some(paths) => std::env::split_paths(&paths)
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        None => Vec::new(),
    };
    let home = var("HOME").map(PathBuf::from);
    let user = match (var("JUPYTER_DATA_DIR"), var("XDG_DATA_HOME")) {
        (Some(dir), _) => Some(PathBuf::from(dir)),
        _ if cfg!(target_os = "macos") => home.map(|h| h.join("Library/Jupyter")),
        (None, Some(data)) if !sandbox => Some(Path::new(&data).join("jupyter")),
        (None, _) => home.map(|h| h.join(".local/share/jupyter")),
    };
    dirs.extend(user);
    dirs.extend(venvs.iter().map(|venv| venv.join("share/jupyter")));
    // Homebrew's and MacPorts' prefixes besides jupyter_core's two.
    let system: &[&str] = match cfg!(target_os = "macos") {
        true => &[
            "/usr/local/share/jupyter",
            "/opt/homebrew/share/jupyter",
            "/opt/local/share/jupyter",
            "/usr/share/jupyter",
        ],
        false => &["/usr/local/share/jupyter", "/usr/share/jupyter"],
    };
    let host = if sandbox { "/run/host" } else { "" };
    dirs.extend(
        system
            .iter()
            .map(|dir| PathBuf::from(format!("{host}{dir}"))),
    );
    dirs
}

/// The kernelspecs in `dirs`, in their order and by name within each, the first of a name
/// winning; a `kernel.json` that does not read is skipped.
fn read(dirs: &[PathBuf]) -> Vec<Spec> {
    let mut specs: Vec<Spec> = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir.join("kernels")) else {
            continue;
        };
        let mut found: Vec<Spec> = entries.flatten().filter_map(|e| one(&e.path())).collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        for spec in found {
            if !specs.iter().any(|s| s.name == spec.name) {
                specs.push(spec);
            }
        }
    }
    specs
}

/// The kernelspec in the folder `dir`, if its `kernel.json` reads as one.
fn one(dir: &Path) -> Option<Spec> {
    #[derive(Deserialize)]
    struct KernelJson {
        argv: Vec<String>,
        #[serde(default)]
        display_name: String,
        #[serde(default)]
        language: String,
        #[serde(default)]
        interrupt_mode: String,
        #[serde(default)]
        env: BTreeMap<String, String>,
    }
    let file = dir.join("kernel.json");
    let text = std::fs::read_to_string(&file).ok()?;
    let json: KernelJson = serde_json::from_str(&text)
        .inspect_err(|e| tracing::debug!("skipping the kernelspec {}: {e}", file.display()))
        .ok()
        .filter(|json: &KernelJson| !json.argv.is_empty())?;
    let name = dir.file_name()?.to_string_lossy().to_lowercase();
    // The Flatpak's view of the host's `/usr`, which runs the kernel, is `/run/host/usr`.
    let dir = dir.to_string_lossy();
    let dir = match dir.strip_prefix("/run/host/") {
        Some(rest) => format!("/{rest}"),
        None => dir.into_owned(),
    };
    Some(Spec {
        display_name: match json.display_name.is_empty() {
            true => name.clone(),
            false => json.display_name,
        },
        name,
        language: json.language,
        dir,
        argv: json.argv,
        env: json.env,
        message_interrupt: json.interrupt_mode == "message",
    })
}

/// `spec`'s command, its folder in place of `{resource_dir}`; `{connection_file}` is the kernel
/// client's.
pub(crate) fn argv(spec: &Spec) -> Vec<String> {
    let argv = spec.argv.iter();
    argv.map(|arg| arg.replace("{resource_dir}", &spec.dir))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, json: &str) {
        let at = dir.join("kernels").join(name);
        std::fs::create_dir_all(&at).unwrap();
        std::fs::write(at.join("kernel.json"), json).unwrap();
    }

    fn spec(language: &str, display_name: &str) -> String {
        let argv = r#"["fake", "-f", "{connection_file}"]"#;
        format!(r#"{{"argv": {argv}, "display_name": "{display_name}", "language": "{language}"}}"#)
    }

    #[test]
    fn the_folders_go_in_jupyters_order() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |key: &str| {
                let found = vars.iter().find(|(k, _)| *k == key);
                found.map(|(_, v)| v.to_string())
            }
        };
        let venvs = [PathBuf::from("/vault/.venv")];
        let path = |s: &str| PathBuf::from(s);
        let set = env(&[
            ("JUPYTER_PATH", "/a:/b"),
            ("XDG_DATA_HOME", "/data"),
            ("HOME", "/home/u"),
        ]);
        let found = dirs(set, false, &venvs);
        let user = match cfg!(target_os = "macos") {
            true => path("/home/u/Library/Jupyter"),
            false => path("/data/jupyter"),
        };
        assert_eq!(
            found[..5],
            [
                path("/a"),
                path("/b"),
                user,
                path("/vault/.venv/share/jupyter"),
                path("/usr/local/share/jupyter"),
            ]
        );
        assert!(found.contains(&path("/usr/share/jupyter")));
        // `JUPYTER_DATA_DIR` names the user's folder outright.
        let found = dirs(
            env(&[("JUPYTER_DATA_DIR", "/jd"), ("HOME", "/h")]),
            false,
            &[],
        );
        assert_eq!(found[0], path("/jd"));
        // The Flatpak's own data folder is not the user's, and the host's `/usr` is under
        // `/run/host`.
        let found = dirs(
            env(&[("XDG_DATA_HOME", "/sandbox"), ("HOME", "/h")]),
            true,
            &[],
        );
        assert_eq!(found[0], path("/h/.local/share/jupyter"));
        assert!(found.contains(&path("/run/host/usr/share/jupyter")));
        assert!(!found.contains(&path("/usr/share/jupyter")));
    }

    #[test]
    fn the_first_of_a_name_wins_and_a_broken_one_is_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let (first, second) = (tmp.path().join("first"), tmp.path().join("second"));
        write(&first, "fake", &spec("fakelang", "Fake"));
        write(&first, "Shared", &spec("r", "Shared in first"));
        write(&second, "shared", &spec("r", "Shared in second"));
        write(&second, "broken", "{ not json");
        write(&second, "ir", &spec("R", "R"));
        let found = read(&[first.clone(), second, tmp.path().join("missing")]);
        let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["fake", "shared", "ir"]);
        assert_eq!(found[1].display_name, "Shared in first");
        assert_eq!(found[0].dir, first.join("kernels/fake").to_string_lossy());
    }

    #[test]
    fn a_notebook_gets_the_kernel_it_names_else_the_first_of_its_language() {
        let at = |name: &str, language: &str| Spec {
            name: name.to_string(),
            display_name: name.to_string(),
            language: language.to_string(),
            dir: String::new(),
            argv: Vec::new(),
            env: BTreeMap::new(),
            message_interrupt: false,
        };
        let specs = [at("julia", "julia"), at("ir", "R"), at("ir43", "R")];
        fn name(s: Option<&Spec>) -> Option<&str> {
            s.map(|s| s.name.as_str())
        }
        assert_eq!(name(pick(&specs, "R", None, Some("ir43"))), Some("ir43"));
        assert_eq!(name(pick(&specs, "r", None, Some("gone"))), Some("ir"));
        assert_eq!(name(pick(&specs, "R", None, None)), Some("ir"));
        // The reader's choice first, and never one of another language.
        assert_eq!(
            name(pick(&specs, "R", Some("ir43"), Some("ir"))),
            Some("ir43")
        );
        assert_eq!(name(pick(&specs, "R", None, Some("julia"))), Some("ir"));
        assert_eq!(name(pick(&specs, "haskell", None, Some("ihaskell"))), None);
    }

    #[test]
    fn its_folder_takes_the_place_of_resource_dir() {
        let spec = Spec {
            name: "k".to_string(),
            display_name: "K".to_string(),
            language: "k".to_string(),
            dir: "/usr/share/jupyter/kernels/k".to_string(),
            argv: ["k", "{resource_dir}/kernel.k", "{connection_file}"]
                .map(String::from)
                .into(),
            env: BTreeMap::new(),
            message_interrupt: true,
        };
        assert_eq!(
            argv(&spec),
            [
                "k",
                "/usr/share/jupyter/kernels/k/kernel.k",
                "{connection_file}"
            ]
        );
    }
}
