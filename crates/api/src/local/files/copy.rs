//! Copies of files and folders, within the vault, into it from this machine and out to another
//! vault, never over a file made meanwhile and a large file in chunks a stop can cut short.

use std::io;

use super::{Local, Msg};
use crate::{Result, fs};

impl Local {
    /// Copy a file or a folder between vault-relative paths, beside the files on a remote vault.
    /// Nothing is replaced: a `to` taken by the time the copy gets there fails with
    /// `AlreadyExists(to)`, and the caller takes its next free name. A folder is copied as Paste
    /// carries one in ([`copy_tree`]), and never into itself.
    pub fn copy(&self, from: &str, to: &str) -> Result<()> {
        let (src, dest) = (self.resolve(from)?, self.resolve(to)?);
        let named = |e| copy_error(from, to, e);
        let landed = match src.is_dir() {
            // Before anything is made: a folder copied into itself would copy its copy again.
            true if lands_inside(&src, &dest) => {
                return Err(accent_core::Error::Invalid(format!("{to} is inside {from}")).into());
            }
            // `create_dir`, not `create_dir_all`: a folder already there is taken, not merged into.
            true => {
                let root = src.canonicalize().map_err(named)?;
                std::fs::create_dir(&dest).map_err(named)?;
                copy_tree(&src, &dest, &root)
            }
            false => {
                copy_file(&src, &dest, &|| false).map_err(named)?;
                Ok(())
            }
        };
        // Made either way: what landed before a later file failed stays, as in a file manager.
        self.post(Msg::Update {
            rel: to.to_string(),
            own: true,
        });
        Ok(landed.map_err(named)?)
    }

    /// [`Vault::upload`](crate::Vault::upload) here: over the file at `rel` when `replace`, else
    /// only to a name nothing holds, as [`copy`](Self::copy) does, until `stopped`.
    pub fn upload(
        &self,
        local: &std::path::Path,
        rel: &str,
        replace: bool,
        stopped: &dyn Fn() -> bool,
    ) -> Result<()> {
        let dest = self.resolve(rel)?;
        match replace {
            true => replace_file(local, &dest, stopped),
            false => copy_file(local, &dest, stopped),
        }
        .map_err(|e| copy_error(local.display(), rel, e))?;
        Ok(())
    }

    /// [`fs::walk_copy`] of `rel`: what copying it into another vault carries, walked where the
    /// files are.
    pub fn list_tree(&self, rel: &str) -> Result<fs::Walked> {
        Ok(fs::walk_copy(&self.resolve(rel)?))
    }

    /// A bounded page of one directory's copy candidates, with the same link rules as `walk_copy`.
    pub(crate) fn copy_page(
        &self,
        root: &str,
        dir: &str,
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<crate::vault::CopyPage> {
        use std::collections::BinaryHeap;
        use std::os::unix::ffi::OsStrExt;

        use crate::vault::{COPY_PAGE, CopyPage};

        let root = self.resolve(root)?;
        let path = Local::join(&root, dir)?;
        if !path.is_dir() {
            return Ok(match (dir, after) {
                ("", None) => CopyPage::File,
                _ => CopyPage::Unread,
            });
        }
        let Ok(children) = std::fs::read_dir(&path) else {
            return Ok(CopyPage::Unread);
        };
        let limit = limit.clamp(1, COPY_PAGE);
        // Stateless pages rescan names. Keep only the smallest names after the cursor so memory,
        // metadata checks and wire size are bounded even for a very large flat directory.
        let mut smallest = BinaryHeap::new();
        for entry in children.flatten() {
            if after.is_some_and(|cursor| entry.file_name().as_bytes() <= cursor) {
                continue;
            }
            smallest.push(entry.path());
            if smallest.len() > limit + 1 {
                smallest.pop();
            }
        }
        let mut children = smallest.into_sorted_vec();
        let next = (children.len() > limit).then(|| {
            children[limit - 1]
                .file_name()
                .expect("a directory entry has a name")
                .as_bytes()
                .to_vec()
        });
        children.truncate(limit);
        let real_root = root.canonicalize().unwrap_or(root);
        let entries = children
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_string_lossy().into_owned();
                let meta = path.symlink_metadata().ok()?;
                if meta.is_dir() {
                    Some((name, true))
                } else if meta.is_file()
                    || (meta.is_symlink() && fs::links_inside(&path, &real_root))
                {
                    Some((name, false))
                } else {
                    None
                }
            })
            .collect();
        Ok(CopyPage::Directory { entries, next })
    }
}

/// Whether `dest` is the folder `src` or inside it, as spelled or as the filesystem resolves it:
/// through a link to `src` or to a folder above it. A `dest` not there yet goes by its nearest
/// folder that is.
fn lands_inside(src: &std::path::Path, dest: &std::path::Path) -> bool {
    let real = |path: &std::path::Path| {
        path.ancestors()
            .find_map(|at| Some(at.canonicalize().ok()?.join(path.strip_prefix(at).ok()?)))
    };
    dest.starts_with(src)
        || real(src)
            .zip(real(dest))
            .is_some_and(|(src, dest)| dest.starts_with(src))
}

/// A failed copy to `to` by what went wrong: `to` when it was taken, else `from`, what was read.
fn copy_error(from: impl std::fmt::Display, to: &str, e: io::Error) -> accent_core::Error {
    match e.kind() {
        io::ErrorKind::AlreadyExists => accent_core::Error::AlreadyExists(to.to_string()),
        _ => accent_core::Error::io(from, e),
    }
}

/// Fill `to`, a folder just made, with what the folder `from` holds, as Paste carries a folder in
/// from this machine: its folders and files, and a link as the file it names when that is inside
/// `root`, the real path of the folder being copied ([`fs::links_inside`]). Other links, FIFOs and
/// sockets stay behind, never opened.
fn copy_tree(
    from: &std::path::Path,
    to: &std::path::Path,
    root: &std::path::Path,
) -> io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let (src, dest) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type()?;
        if kind.is_dir() {
            std::fs::create_dir(&dest)?;
            copy_tree(&src, &dest, root)?;
        } else if kind.is_file() || (kind.is_symlink() && fs::links_inside(&src, root)) {
            copy_file(&src, &dest, &|| false)?;
        }
    }
    Ok(())
}

/// How much [`copy_chunks`] copies between asking whether to stop, so a large file's copy ends at
/// most a chunk after the stop. Tests copy a few bytes at a time, to stop one part way.
pub(super) const CHUNK: u64 = if cfg!(test) { 4 } else { 16 * 1024 * 1024 };

/// Copy the file `from` to `to`, a name nothing holds: `create_new` claims it and opens it in one
/// step, so a file that appeared there meanwhile stays as it was. The mode goes along, under the
/// umask. A copy cut short, by `stopped` too, is removed rather than left torn.
pub(super) fn copy_file(
    from: &std::path::Path,
    to: &std::path::Path,
    stopped: &dyn Fn() -> bool,
) -> io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let (src, meta) = open_file(from)?;
    let dest = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(meta.permissions().mode() & 0o777)
        .open(to)?;
    copy_chunks(&src, &dest, stopped).inspect_err(|_| {
        let _ = std::fs::remove_file(to);
    })
}

/// Copy the file `from` over the file `to` as `std::fs::copy` does, through a link to what it
/// names and with `from`'s mode, but into a `.part` beside it renamed over it once whole, as a
/// download lands: a copy cut short, by `stopped` too, leaves `to` as it was.
pub(super) fn replace_file(
    from: &std::path::Path,
    to: &std::path::Path,
    stopped: &dyn Fn() -> bool,
) -> io::Result<()> {
    let (src, meta) = open_file(from)?;
    let to = to.canonicalize().unwrap_or_else(|_| to.to_path_buf());
    let (part, out) = crate::remote::part_beside(&to)?;
    copy_chunks(&src, &out, stopped)
        .and_then(|()| out.set_permissions(meta.permissions()))
        .and_then(|()| std::fs::rename(&part, &to))
        .inspect_err(|_| {
            let _ = std::fs::remove_file(&part);
        })
}

/// `from` opened for a copy, once it is known to be a file: opening a FIFO would wait for a
/// writer that never comes.
pub(super) fn open_file(from: &std::path::Path) -> io::Result<(std::fs::File, std::fs::Metadata)> {
    let meta = std::fs::metadata(from)?;
    if !meta.is_file() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a file"));
    }
    Ok((std::fs::File::open(from)?, meta))
}

/// Copy `src` into `dest` [`CHUNK`] bytes at a time, asking `stopped` before each. A chunk is
/// std's `io::copy` between two files, one `copy_file_range` on Linux, which btrfs and XFS answer
/// with a reflink for a range as for a whole file, so the copy still shares its extents.
pub(super) fn copy_chunks(
    src: &std::fs::File,
    mut dest: &std::fs::File,
    stopped: &dyn Fn() -> bool,
) -> io::Result<()> {
    use std::io::Read;
    loop {
        if stopped() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "stopped"));
        }
        if io::copy(&mut src.take(CHUNK), &mut dest)? == 0 {
            return Ok(());
        }
    }
}
