use super::file::{edited, fields, merge, retired_daily, theirs, unknown_keys};
use super::*;
use crate::git::Comparison;
use std::path::Path;
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
    assert!(c.folders_among_files);
    assert!(!c.git_tint);
    assert_eq!(c.column_width, 50);
    assert!(c.full_width_preview);
    assert_eq!(c.indent_width, 4);
    assert!(!c.forward_keys_to_terminal);
    assert_eq!(c.debug_terminal, DebugTerminal::Down);
    assert_eq!(c.editor, Editor::Helix);
    assert_eq!(c.theme, Theme::Solarized);
    assert_eq!(c.focus_mode, FocusMode::High);
    assert!(c.recolour.images && !c.recolour.pdfs && c.recolour.diagrams);
    assert_eq!(c.editor_font, None);
    assert_eq!(c.preview_font.as_deref(), Some("Serif 12"));
    let v = &c.vaults["/home/me/Notes"];
    assert_eq!(v.templates_dir, "Templates");
    assert_eq!(v.new_file_dir, "Inbox");
    assert_eq!(v.attachment_folder, "./attachments");
    // An override is a list, so an action can keep several chords, and an empty list is how
    // the user says "no shortcut at all" rather than "fall back to the default".
    assert_eq!(c.shortcuts["win.find-next"], ["F3"]);
    assert!(c.shortcuts["win.about"].is_empty());
    let claude = CustomCommand {
        name: "Claude".into(),
        run: "claude".into(),
        shortcut: Some("<Control><Shift>j".into()),
    };
    assert_eq!(c.commands[0], claude);
    assert_eq!(c.commands[1].shortcut, None);
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
    assert_eq!(again.debug_terminal, DebugTerminal::Down);
    assert_eq!(again.editor, Editor::Helix);
    assert_eq!(again.vaults["/home/me/Notes"].new_file_dir, "Inbox");
    assert_eq!(again.shortcuts, c.shortcuts);
    assert_eq!(again.commands, c.commands);
    assert_eq!(again.search.exclude, c.search.exclude);
    assert_eq!(again.drawing, c.drawing);
    assert_eq!(again.diagram, c.diagram);
    assert_eq!(again.preview_font, c.preview_font);
}

/// accent's own write changes the keys it changed and nothing else: the user's comments,
/// order and spacing stay byte for byte, a key left out at its default stays left out.
#[test]
fn a_save_keeps_the_files_comments_order_and_spacing() {
    let file = r#"# Mine, by hand.
theme = "dark"    # easier on the eyes
minimap   = false # for now

# Chords I like
[shortcuts]
"win.save" = ["F2"]  # muscle memory

[[commands]]
# The agent
name = "Claude"
run = "claude"

[drawing]
pen_width = 2.0
"#;
    let read = |text: &str| -> Config { toml::from_str(text).unwrap() };
    let mut c = read(file);
    assert_eq!(edited(Some(file), &c).unwrap(), file, "nothing changed");

    c.minimap = true;
    let text = edited(Some(file), &c).unwrap();
    assert_eq!(text, file.replace("minimap   = false", "minimap   = true"));

    // A key added, a list emptied and a table that was not there: written, and only they.
    c.line_numbers = true;
    c.commands.clear();
    c.recolour.pdfs = false;
    let text = edited(Some(file), &c).unwrap();
    assert_eq!(read(&text), c);
    for kept in [
        "# Mine, by hand.",
        "# easier on the eyes",
        "# muscle memory",
    ] {
        assert!(text.contains(kept), "{text}");
    }
    assert!(
        !text.contains("[[commands]]") && !text.contains("spellcheck"),
        "{text}"
    );
    assert!(text.contains("[recolour]\npdfs = false\n"), "{text}");
    // No file yet: the whole config, as before.
    assert_eq!(
        edited(None, &c).unwrap(),
        toml::to_string_pretty(&c).unwrap()
    );
}

/// A custom editor is the user's command line, read and written as the user wrote it.
#[test]
fn a_custom_editor_keeps_its_command() {
    let c: Config =
        toml::from_str("editor = \"custom\"\neditor_command = \"kak +{line} {file}\"\n").unwrap();
    assert_eq!(c.editor, Editor::Custom);
    let again: Config = toml::from_str(&toml::to_string(&c).unwrap()).unwrap();
    assert_eq!(again.editor_command, "kak +{line} {file}");
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
    assert!(!text.contains("[[commands]]"), "{text}");
    assert!(!text.contains("[vaults"), "{text}");
    // Accent's own editor is the default, so the file names none.
    assert!(!text.contains("editor ="), "{text}");
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
    assert_eq!(c.debug_terminal, DebugTerminal::Same);
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

/// Every key the config reads is known, at both levels and whatever its value: a field written
/// only while it is not empty is read all the same.
#[test]
fn every_field_read_is_known() {
    let table: toml::Table = "bibliography_unpaywall_email = \"me@example.org\""
        .parse()
        .unwrap();
    assert_eq!(unknown_keys(&table), Vec::<String>::new());

    let each = |names: &[&str]| -> toml::Table {
        names.iter().map(|n| (n.to_string(), true.into())).collect()
    };
    let mut table = each(fields::<Config>());
    let vault = toml::Table::from_iter([("/v".to_string(), each(fields::<VaultConfig>()).into())]);
    table.insert("vaults".into(), vault.into());
    assert!(table.contains_key("bibliography_unpaywall_email"));
    assert_eq!(unknown_keys(&table), Vec::<String>::new());
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
    assert_eq!(
        c.local_vault(notes).lsp.ghost,
        argv(&["~/merl/build/merl-rt"])
    );

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
        file_order: crate::order::Sort::CreatedOld,
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
        zoom: 1.2,
        recent_files: vec!["Daily/2026-09-03.md".to_string()],
        recent_commands: vec!["win.save".to_string()],
        pdf: BTreeMap::from([(
            "Attachments/paper.pdf".to_string(),
            PdfPlace {
                page: 4,
                zoom: PdfZoom::Scale(1.5),
                drawing: true,
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
        terminals: BTreeMap::from([
            (
                "terminal:0123456789abcdef".to_string(),
                ShellPlace {
                    at: "/home/me/src".to_string(),
                    command: Some("Claude".to_string()),
                    edits: None,
                },
            ),
            (
                "terminal:fedcba9876543210".to_string(),
                ShellPlace {
                    at: "/home/me/Notes".to_string(),
                    command: None,
                    edits: Some(Edits {
                        editor: Editor::Helix,
                        file: "Inbox/a b.md".to_string(),
                    }),
                },
            ),
        ]),
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
        kernels: BTreeMap::from([("r.ipynb".to_string(), "ir".to_string())]),
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
        assert_eq!(back.file_order, s.file_order);
        assert_eq!(back.info, s.info);
        assert_eq!(back.code, s.code);
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
        assert_eq!(back.kernels, s.kernels);

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
/// the default, the `pane` and `view` every file written until now carries are simply dropped
/// — which sidebar pane was showing, and the split preview, are no longer restored — and
/// `recent_notes` is `recent_files`.
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
