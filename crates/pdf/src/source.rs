//! What a PDF is read from: a file, read where it is asked for (`pread`), or bytes in memory —
//! Android's `content://` documents and a base normalised in memory.

use std::fs::File;
use std::io;
use std::os::unix::fs::FileExt;

/// Bytes read at an offset, never through a shared file position, which pdfium's reader of the
/// same file moves.
pub trait Source: Send + Sync {
    /// The size in bytes. Not `len`, which on `Vec<u8>` would shadow the vector's own.
    fn size(&self) -> u64;

    /// Read into `buf` from `at`; fewer bytes only at the end.
    fn read_at(&self, at: u64, buf: &mut [u8]) -> io::Result<usize>;

    /// Up to `n` bytes from `at`, fewer at the end.
    fn read_vec(&self, at: u64, n: usize) -> io::Result<Vec<u8>> {
        let n = n.min(self.size().saturating_sub(at) as usize);
        let mut buf = vec![0; n];
        let mut done = 0;
        while done < n {
            match self.read_at(at + done as u64, &mut buf[done..])? {
                0 => break,
                read => done += read,
            }
        }
        buf.truncate(done);
        Ok(buf)
    }
}

impl Source for File {
    fn size(&self) -> u64 {
        self.metadata().map_or(0, |m| m.len())
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> io::Result<usize> {
        FileExt::read_at(self, buf, at)
    }
}

impl Source for Vec<u8> {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn read_at(&self, at: u64, buf: &mut [u8]) -> io::Result<usize> {
        let rest = self.get(at as usize..).unwrap_or_default();
        let n = rest.len().min(buf.len());
        buf[..n].copy_from_slice(&rest[..n]);
        Ok(n)
    }
}
