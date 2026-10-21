//! Git calls for a vault or the directory of a focused shell.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use accent_api::git::{self, Repo};
use accent_api::git_remote::RemoteGit;
use accent_api::{Result, Vault};

#[derive(Clone)]
pub(crate) enum Source {
    Vault(Arc<Vault>),
    Local(PathBuf),
    Remote { git: Box<RemoteGit>, cwd: PathBuf },
    Empty,
}

impl Source {
    pub fn same_location(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Vault(a), Self::Vault(b)) => Arc::ptr_eq(a, b),
            (Self::Local(a), Self::Local(b)) => a == b,
            (Self::Remote { git: a, cwd: x }, Self::Remote { git: b, cwd: y }) => {
                a.url() == b.url() && x == y
            }
            (Self::Empty, Self::Empty) => true,
            _ => false,
        }
    }

    pub fn key_root(&self) -> PathBuf {
        match self {
            Self::Vault(vault) => vault.root(),
            // Loose file keys are absolute even when the shell sits inside its repository.
            Self::Local(_) | Self::Remote { .. } => PathBuf::new(),
            Self::Empty => PathBuf::new(),
        }
    }

    pub fn is_remote(&self) -> bool {
        match self {
            Self::Vault(vault) => vault.is_remote(),
            Self::Remote { .. } => true,
            _ => false,
        }
    }

    pub fn repos(&self) -> Result<Vec<Repo>> {
        match self {
            Self::Vault(vault) => vault.repos(),
            Self::Local(cwd) => Ok(git::toplevel(cwd)?.into_iter().collect()),
            Self::Remote { git, cwd } => Ok(git.repo_at(cwd)?.into_iter().collect()),
            Self::Empty => Ok(Vec::new()),
        }
    }

    /// A comparison's worktree side is repository-relative for shell sources.
    pub fn read_text(&self, repo: &Repo, rel: &str) -> Result<accent_api::fs::Read> {
        match self {
            Self::Vault(vault) => {
                let key = super::vault_key(&vault.root(), repo, rel);
                if Path::new(&key).is_absolute() {
                    if vault.is_remote() {
                        return Err(accent_core::Error::Invalid(
                            "the file is outside the remote vault".into(),
                        )
                        .into());
                    }
                    Ok(accent_core::fs::read_text(Path::new(&key))?)
                } else {
                    vault.read_text(&key)
                }
            }
            Self::Local(_) => Ok(accent_core::fs::read_text(&repo.root.join(rel))?),
            Self::Remote { git, .. } => git.read_text(repo, rel),
            Self::Empty => Err(unavailable()),
        }
    }

    pub fn read(&self, repo: &Repo, rel: &str) -> Result<(String, accent_api::Etag)> {
        match self {
            Self::Vault(vault) => {
                let key = super::vault_key(&vault.root(), repo, rel);
                if Path::new(&key).is_absolute() {
                    if vault.is_remote() {
                        return Err(accent_core::Error::Invalid(
                            "the file is outside the remote vault".into(),
                        )
                        .into());
                    }
                    Ok(accent_core::fs::read_note(Path::new(&key))?)
                } else {
                    vault.read(&key)
                }
            }
            Self::Local(_) => Ok(accent_core::fs::read_note(&repo.root.join(rel))?),
            Self::Remote { git, .. } => git.read(repo, rel),
            Self::Empty => Err(unavailable()),
        }
    }

    pub fn exists(&self, key: &str) -> bool {
        match self {
            Self::Vault(vault) => vault.exists(key),
            Self::Local(_) => Path::new(key).exists(),
            Self::Remote { .. } | Self::Empty => false,
        }
    }
}

fn unavailable() -> accent_api::Error {
    accent_core::Error::Invalid("no focused Git repository".into()).into()
}

macro_rules! git_methods {
    ($(fn $name:ident($repo:ident: &Repo $(, $arg:ident: $ty:ty)* ) -> $out:ty = $core:ident;)*) => {
        impl Source {
            $(
                pub fn $name(&self, $repo: &Repo $(, $arg: $ty)*) -> Result<$out> {
                    match self {
                        Self::Vault(vault) => vault.$name($repo $(, $arg)*),
                        Self::Local(_) => Ok(git::$core($repo $(, $arg)*)?),
                        Self::Remote { git, .. } => git.$name($repo $(, $arg)*),
                        Self::Empty => Err(unavailable()),
                    }
                }
            )*
        }
    };
}

git_methods! {
    fn git_status(repo: &Repo) -> git::Status = status;
    fn git_untracked(repo: &Repo, dir: &str) -> Vec<String> = untracked;
    fn git_log(repo: &Repo, skip: usize, limit: usize) -> Vec<git::Commit> = log;
    fn git_show(repo: &Repo, rev: &str, path: &str) -> Option<git::Blob> = show;
    fn git_changed_files(repo: &Repo, oid: &str) -> Vec<git::ChangedFile> = changed_files;
    fn git_submodules(repo: &Repo) -> Vec<git::Submodule> = submodules;
    fn git_branches(repo: &Repo) -> git::Branches = branches;
    fn git_checkout(repo: &Repo, branch: &str) -> () = checkout;
    fn git_track(repo: &Repo, remote: &str) -> () = track;
    fn git_checkout_commit(repo: &Repo, oid: &str) -> () = checkout_commit;
    fn git_create_branch(repo: &Repo, name: &str, checkout: bool) -> () = create_branch;
    fn git_delete_branch(repo: &Repo, name: &str, force: bool) -> () = delete_branch;
    fn git_merge(repo: &Repo, branch: &str) -> git::Merge = merge;
    fn git_merge_abort(repo: &Repo) -> () = merge_abort;
    fn git_rebase_continue(repo: &Repo) -> git::Rebase = rebase_continue;
    fn git_rebase_abort(repo: &Repo) -> () = rebase_abort;
    fn git_stage(repo: &Repo, paths: &[String]) -> () = stage;
    fn git_unstage(repo: &Repo, paths: &[String]) -> () = unstage;
    fn git_discard(repo: &Repo, paths: &[String]) -> () = discard;
    fn git_stage_text(repo: &Repo, path: &str, text: &str) -> () = stage_text;
    fn git_commit(repo: &Repo, message: &str, all: bool) -> String = commit;
    fn git_pull(repo: &Repo) -> String = pull;
    fn git_push(repo: &Repo) -> String = push;
    fn git_fetch(repo: &Repo) -> String = fetch;
    fn git_incoming(repo: &Repo) -> Vec<String> = incoming;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_shell_source_finds_only_its_cwd_repository() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("project");
        std::fs::create_dir_all(repo.join("nested")).unwrap();
        assert!(
            std::process::Command::new("git")
                .args(["init", "-q"])
                .current_dir(&repo)
                .status()
                .unwrap()
                .success()
        );
        let source = Source::Local(repo.join("nested"));
        let found = source.repos().unwrap().remove(0);
        assert_eq!(found.root, repo);
        std::fs::write(repo.join("a.md"), "a").unwrap();
        assert!(
            source
                .git_status(&found)
                .unwrap()
                .entries
                .iter()
                .any(|entry| entry.path == "a.md")
        );
        assert_eq!(
            super::super::vault_key(&source.key_root(), &found, "a.md"),
            repo.join("a.md").to_string_lossy()
        );
        assert!(source.same_location(&Source::Local(repo.join("nested"))));
        assert!(!source.same_location(&Source::Local(repo.clone())));
        assert!(
            Source::Local(tmp.path().to_path_buf())
                .repos()
                .unwrap()
                .is_empty()
        );
    }
}
