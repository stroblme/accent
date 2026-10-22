//! Accent.app (`build-aux/macos-bundle.sh`) carries what GLib, GIO and gdk-pixbuf load at run time
//! in `Contents/Resources`, while the paths built into them are the build machine's
//! (`/opt/local`): pointed nowhere, the app has no icons, schemas or image loaders, or loads the
//! package manager's modules beside its own GLib where one is installed.

use std::path::{Path, PathBuf};

use gtk::{gdk_pixbuf, gio, glib};

/// Each variable, and the file or folder under `Contents/Resources` it names.
const PATHS: [(&str, &str); 3] = [
    // The icon themes, and the GSettings schemas in `glib-2.0/schemas`.
    ("XDG_DATA_DIRS", "share"),
    ("GDK_PIXBUF_MODULE_FILE", "lib/gdk-pixbuf/loaders.cache"),
    // GIO's TLS backend.
    ("GIO_MODULE_DIR", "lib/gio/modules"),
];

/// When running from the app, point GLib, GIO and gdk-pixbuf into it, then put the environment
/// back, so that a shell, a language server or a GTK program run in a terminal tab does not
/// inherit the app's paths. A build run from `target/` uses the package manager's as it is.
/// Started from the Dock or Finder, which only the app is, it also takes the login shell's PATH.
pub fn setup() {
    let Some(resources) = std::env::current_exe().ok().and_then(|exe| resources(&exe)) else {
        return;
    };
    let old: Vec<_> = PATHS
        .iter()
        .map(|(name, _)| std::env::var_os(name))
        .collect();
    for (name, path) in PATHS {
        // SAFETY: the first thing `main` does, before any other thread is started.
        unsafe { std::env::set_var(name, resources.join(path)) };
    }
    // Each reads its variable once, here, and keeps what it found.
    glib::system_data_dirs();
    gio::TlsBackend::default();
    gdk_pixbuf::Pixbuf::formats();
    for ((name, _), value) in PATHS.iter().zip(old) {
        // SAFETY: as above; loading GIO's modules started no thread.
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }
    super::login::take_path();
}

/// `Contents/Resources` of the app `exe` runs from, if it runs from one.
fn resources(exe: &Path) -> Option<PathBuf> {
    let resources = exe.parent()?.parent()?.join("Resources");
    resources.join("share").is_dir().then_some(resources)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_app_has_resources() {
        let dir = tempfile::tempdir().unwrap();
        let contents = dir.path().join("Accent.app/Contents");
        std::fs::create_dir_all(contents.join("Resources/share")).unwrap();
        let exe = contents.join("MacOS/accent");
        assert_eq!(resources(&exe), Some(contents.join("Resources")));
        assert_eq!(resources(&dir.path().join("target/release/accent")), None);
    }
}
