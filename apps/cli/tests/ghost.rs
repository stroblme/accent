//! Ghost text end to end, from the merl compiled into `accent-cli`: a vault on this machine runs
//! it as `accent-cli ghost`, named as the app names it at startup.

use accent_api::{Pos, Vault, VaultConfig};
use std::path::PathBuf;
use std::time::Duration;

/// A scratch directory, removed afterwards.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// The suggestion at the end of `typed`, once merl has one: it builds its index on a thread of
/// its own, and answers nothing until that lands.
async fn suggestion(vault: &Vault, rel: &str, typed: &str) -> Option<String> {
    let at = Pos {
        line: 0,
        character: typed.chars().count() as u32,
    };
    for _ in 0..100 {
        if let Some(got) = vault.inline_completion(rel, at).await.unwrap() {
            return Some(got);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    None
}

#[test]
fn merl_suggests_the_rest_of_a_line_the_vault_has_written_before() {
    accent_api::language::set_cli(PathBuf::from(env!("CARGO_BIN_EXE_accent-cli")));
    let scratch =
        Scratch(std::env::temp_dir().join(format!("accent-ghost-{}", std::process::id())));
    let root = scratch.0.join("vault");
    std::fs::create_dir_all(&root).unwrap();
    // Twice, because a match has to have been seen more than once to be offered.
    let seen = "The kettle was already boiling.\n";
    std::fs::write(root.join("a.md"), format!("{seen}{seen}")).unwrap();
    let (vault, _events) =
        Vault::open_at(&root, &scratch.0.join("index.db"), VaultConfig::default()).unwrap();

    accent_lsp::runtime().block_on(async {
        let typed = "The kettle was";
        let support = vault
            .open_document("b.md", "markdown", typed.to_string())
            .await
            .unwrap();
        assert!(support.inline, "merl answers for a note");
        assert_eq!(
            suggestion(&vault, "b.md", typed).await.as_deref(),
            Some(" already boiling.")
        );

        // A note written after the session started is not in the index until the document
        // it was typed in settles.
        let later = "Rhubarb crumble needs custard.\n";
        std::fs::write(root.join("c.md"), format!("{later}{later}")).unwrap();
        let typed = "Rhubarb crumble";
        vault
            .change_document("b.md", typed.to_string())
            .await
            .unwrap();
        let at = Pos {
            line: 0,
            character: typed.chars().count() as u32,
        };
        assert_eq!(vault.inline_completion("b.md", at).await.unwrap(), None);
        vault.save_document("b.md").await.unwrap();
        vault.settle("b.md").await.unwrap();
        assert_eq!(
            suggestion(&vault, "b.md", typed).await.as_deref(),
            Some(" needs custard."),
            "settle re-indexed"
        );
    });
}
