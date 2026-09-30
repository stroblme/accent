//! The read-only subcommands open the vault as the app does, so what they print is the index
//! brought up to date with the disk, under the app's own scan policy and search default.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Run `accent-cli <args> --vault <dir>/vault --db <dir>/index.db` and hand back what it printed.
fn run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_accent-cli"))
        .args(args)
        .arg("--vault")
        .arg(dir.join("vault"))
        .arg("--db")
        .arg(dir.join("index.db"))
        .env("XDG_CACHE_HOME", dir.join("cache"))
        .output()
        .expect("accent-cli must be built");
    assert!(out.status.success(), "{args:?}: {out:?}");
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn a_query_answers_from_an_index_brought_up_to_date_first() {
    let dir: PathBuf = std::env::temp_dir().join(format!("accent-query-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("vault")).unwrap();
    std::fs::write(dir.join("vault/a.md"), "hello [[b]] [[nowhere]]\n").unwrap();
    std::fs::write(dir.join("vault/b.md"), "#tag\n").unwrap();

    // Nothing has indexed this vault before: the answer is there because the walk ran first.
    let tags = run(&dir, &["tags"]);
    let stats = run(&dir, &["stats"]);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(tags.contains("#tag"), "{tags}");
    assert!(stats.contains(r#""unresolved": 1"#), "{stats}");
}
