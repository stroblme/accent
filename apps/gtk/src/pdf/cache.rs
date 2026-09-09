//! The rendered tiles a page is painted from, under a budget, least recently used dropped first.

use adw::prelude::*;
use gtk::{gdk, glib};
use std::collections::HashMap;

/// Tile edge in device pixels. 512 is 1 MiB of RGBA, small enough that a scroll never waits on
/// one page-sized render and large enough that a screen is a handful of them.
pub const TILE: i32 = 512;

/// Width of the low-resolution stand-in, in device pixels. Also what the thumbnail strip paints.
pub const LOWRES_W: i32 = 256;

/// Tile bytes held before the least recently used are dropped.
const BUDGET: usize = 256 << 20;

/// The same for the low-resolution stand-ins, which used to be kept for the life of the tab: at
/// 370 KB each (256 x 362 x 4 for A4) a 500-page document strip-scrolled from end to end held
/// 177 MB of them, and twice that once the reader had seen it in both light and dark.
///
/// A quarter of [`BUDGET`], which is about 180 A4 pages. The most that can be on screen at once
/// is far less: the reading view paints one viewport of prefetch either side of the one being
/// read, which at the 10 % minimum zoom and a 2 000 px-tall viewport is 48 pages, and the strip
/// beside it another 20. Eviction therefore never reaches a page either view is painting.
const LOWRES_BUDGET: usize = 64 << 20;

/// One tile of one page at one scale, in one colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: u32,
    /// Device pixels per point times 1000, so a float scale can be a hash key.
    pub scale_milli: u32,
    pub tx: u16,
    pub ty: u16,
    pub dark: bool,
}

/// Textures already rendered, dropped least-recently-used first once they outgrow their budget.
///
/// Two maps and two budgets, because the two kinds of texture are wanted for different lengths of
/// time: a tile is one square of one page at one zoom and is stale the moment the zoom changes,
/// while a stand-in is a whole page at a fixed size and stays useful at every zoom.
#[derive(Default)]
pub struct Cache {
    tiles: HashMap<TileKey, (gdk::MemoryTexture, u64)>,
    /// One whole-page thumbnail per page and theme, which is what a page shows before its tiles
    /// arrive and what the thumbnail strip paints, under [`LOWRES_BUDGET`].
    lowres: HashMap<(u32, bool), (gdk::MemoryTexture, u64)>,
    bytes: usize,
    lowres_bytes: usize,
    tick: u64,
}

impl Cache {
    pub fn get(&mut self, key: &TileKey) -> Option<gdk::MemoryTexture> {
        self.tick += 1;
        let tick = self.tick;
        let (texture, used) = self.tiles.get_mut(key)?;
        *used = tick;
        Some(texture.clone())
    }

    pub fn insert(&mut self, key: TileKey, texture: gdk::MemoryTexture, bytes: usize) {
        self.tick += 1;
        self.bytes += bytes;
        // A re-render of a tile already held replaces it, so its bytes go with it: counting only
        // what arrives made the total drift up after every stroke and evict long before the
        // budget was really reached.
        if let Some((old, _)) = self.tiles.insert(key, (texture, self.tick)) {
            self.bytes -= bytes_of(&old);
        }
        self.bytes -= drop_oldest(&mut self.tiles, self.bytes, BUDGET);
    }

    /// Takes `&mut self` so that painting a page counts as using its stand-in: eviction is by
    /// least recently *painted*, which is what keeps what is on screen off the list.
    pub fn lowres(&mut self, page: u32, dark: bool) -> Option<gdk::MemoryTexture> {
        self.tick += 1;
        let tick = self.tick;
        let (texture, used) = self.lowres.get_mut(&(page, dark))?;
        *used = tick;
        Some(texture.clone())
    }

    pub fn insert_lowres(&mut self, page: u32, dark: bool, texture: gdk::MemoryTexture) {
        self.tick += 1;
        self.lowres_bytes += (texture.width() * texture.height() * 4) as usize;
        if let Some((old, _)) = self.lowres.insert((page, dark), (texture, self.tick)) {
            self.lowres_bytes -= (old.width() * old.height() * 4) as usize;
        }
        self.lowres_bytes -= drop_oldest(&mut self.lowres, self.lowres_bytes, LOWRES_BUDGET);
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.lowres.clear();
        self.bytes = 0;
        self.lowres_bytes = 0;
    }

    /// Forget what was rendered of one page, **except** its tiles at the scale and scheme now on
    /// screen, which stay to be painted while their replacements render.
    ///
    /// What a stroke or an exported highlight invalidates: the page it landed on is drawn
    /// differently now and every other page is exactly as it was, so dropping the whole cache
    /// would re-render the viewport and its prefetch after every stroke. What is kept is stale
    /// and is asked for again — see [`PdfView::refresh_page`] — but painting yesterday's render
    /// of a page for the 30 ms its replacement takes is invisible, where painting blank paper is
    /// the flash this exists to avoid. Everything else is going spare: nobody is looking at a
    /// render at another zoom, and keeping it would paint the old page after the next one.
    pub fn forget_page_except(&mut self, page: u32, scale_milli: u32, dark: bool) {
        let (tiles, lowres) = (&mut self.bytes, &mut self.lowres_bytes);
        self.tiles.retain(|key, (texture, _)| {
            let keep = key.page != page || (key.scale_milli == scale_milli && key.dark == dark);
            if !keep {
                *tiles -= bytes_of(texture);
            }
            keep
        });
        self.lowres.retain(|(at, _), (texture, _)| {
            let keep = *at != page;
            if !keep {
                *lowres -= bytes_of(texture);
            }
            keep
        });
    }
}

/// Drop the least recently used entries of `map` until it is comfortably under `budget`, and
/// report how many bytes that freed.
fn drop_oldest<K: Copy + Eq + std::hash::Hash>(
    map: &mut HashMap<K, (gdk::MemoryTexture, u64)>,
    bytes: usize,
    budget: usize,
) -> usize {
    let used = map
        .iter()
        .map(|(key, (texture, tick))| (*tick, bytes_of(texture), *key))
        .collect();
    let mut freed = 0;
    for key in overflowing(used, bytes, budget) {
        if let Some((texture, _)) = map.remove(&key) {
            freed += bytes_of(&texture);
        }
    }
    freed
}

/// Which entries a cache of `bytes` has to give up to come back comfortably under `budget`:
/// the least recently used first, down to three quarters of it rather than to the line, so
/// eviction happens in batches rather than on every insert once the cache is full.
///
/// `used` is every entry as its tick, its size and its key. Kept apart from the textures so the
/// policy can be checked without a display.
fn overflowing<K: Copy>(mut used: Vec<(u64, usize, K)>, bytes: usize, budget: usize) -> Vec<K> {
    if bytes <= budget {
        return Vec::new();
    }
    used.sort_unstable_by_key(|(tick, _, _)| *tick);
    let mut left = bytes;
    let mut out = Vec::new();
    for (_, size, key) in used {
        if left * 4 <= budget * 3 {
            break;
        }
        left -= size;
        out.push(key);
    }
    out
}

pub(super) fn bytes_of(texture: &gdk::MemoryTexture) -> usize {
    (texture.width() * texture.height() * 4) as usize
}

/// A tile the widget wants and does not have. `u16::MAX` in both axes means the whole page at
/// low resolution, which is what it paints while the real tiles are still being rendered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Want {
    pub page: u32,
    pub tx: u16,
    pub ty: u16,
}

impl Want {
    pub fn is_lowres(&self) -> bool {
        self.tx == u16::MAX
    }
}

/// A rendered image as something GTK can paint.
pub fn texture(image: accent_core::pdf::RgbaImage) -> gdk::MemoryTexture {
    let stride = image.width as usize * 4;
    gdk::MemoryTexture::new(
        image.width as i32,
        image.height as i32,
        gdk::MemoryFormat::R8g8b8a8,
        &glib::Bytes::from_owned(image.data),
        stride,
    )
}

/// How many tiles cover `pixels`.
pub(super) fn tiles_across(pixels: i32) -> i32 {
    (pixels + TILE - 1) / TILE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiles_cover_the_page_including_a_partial_last_one() {
        assert_eq!(tiles_across(TILE), 1);
        assert_eq!(tiles_across(TILE + 1), 2);
        assert_eq!(tiles_across(0), 0);
    }

    /// Ten entries of 10 bytes against a budget of 100: nothing goes until the eleventh, and then
    /// enough of the oldest go at once to leave room for three more.
    #[test]
    fn eviction_drops_the_least_recently_used_in_batches() {
        let entries =
            |n: u64| -> Vec<(u64, usize, u64)> { (0..n).map(|tick| (tick, 10, tick)).collect() };
        assert!(overflowing(entries(10), 100, 100).is_empty());
        // 110 down to 75 or less: four of the ten, oldest first.
        assert_eq!(overflowing(entries(11), 110, 100), vec![0, 1, 2, 3]);
        // Freshly painted pages sort last, so they are the ones eviction never reaches.
        let mut used = entries(11);
        used[0].0 = 99;
        assert!(!overflowing(used, 110, 100).contains(&0));
    }
}
