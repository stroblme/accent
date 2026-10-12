//! The rendered tiles a page is painted from, under a budget, least recently used dropped first.

use accent_core::pdf::Theme;
use adw::prelude::*;
use gtk::{gdk, glib};
use std::collections::HashMap;
use std::ops::Range;

/// Tile edge in device pixels. 512 is 1 MiB of RGBA, small enough that a scroll never waits on
/// one page-sized render and large enough that a screen is a handful of them.
pub const TILE: i32 = 512;

/// Width of the low-resolution stand-in, in device pixels. Also what the thumbnail strip paints.
pub const LOWRES_W: i32 = 256;

/// Tile bytes held before the least recently used are dropped.
const BUDGET: usize = 256 << 20;

/// Low-resolution texture budget, one quarter of [`BUDGET`]. Bounds memory when scrolling long
/// documents in multiple themes while retaining more pages than either view can display or
/// prefetch.
const LOWRES_BUDGET: usize = 64 << 20;

/// One tile of one page at one scale, in one colour scheme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub page: u32,
    /// Device pixels per point times 1000, so a float scale can be a hash key.
    pub scale_milli: u32,
    pub tx: u16,
    pub ty: u16,
    pub theme: Theme,
}

/// LRU texture cache with separate budgets for zoom-specific tiles and fixed-size stand-ins that
/// remain useful across zoom changes.
#[derive(Default)]
pub struct Cache {
    tiles: HashMap<TileKey, (gdk::MemoryTexture, u64)>,
    /// One whole-page thumbnail per page and theme, which is what a page shows before its tiles
    /// arrive and what the thumbnail strip paints, under [`LOWRES_BUDGET`].
    lowres: HashMap<(u32, Theme), (gdk::MemoryTexture, u64)>,
    bytes: usize,
    lowres_bytes: usize,
    tick: u64,
    /// How many times a page's renders have been declared out of date. See [`Cache::generation`].
    generation: u64,
    /// Every tile that has landed, in order, until a drill takes the list.
    #[cfg(feature = "bench")]
    pub rendered: Vec<TileKey>,
}

impl Cache {
    /// Which generation of the document's renders this is: a page that changed starts a new one.
    /// A view asks for a list of tiles again when this has moved even if the list has not, since
    /// the same tiles are now wanted for what the page says after the change.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn get(&mut self, key: &TileKey) -> Option<gdk::MemoryTexture> {
        self.tick += 1;
        let tick = self.tick;
        let (texture, used) = self.tiles.get_mut(key)?;
        *used = tick;
        Some(texture.clone())
    }

    pub fn insert(&mut self, key: TileKey, texture: gdk::MemoryTexture, bytes: usize) {
        #[cfg(feature = "bench")]
        self.rendered.push(key);
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
    pub fn lowres(&mut self, page: u32, theme: Theme) -> Option<gdk::MemoryTexture> {
        self.tick += 1;
        let tick = self.tick;
        let (texture, used) = self.lowres.get_mut(&(page, theme))?;
        *used = tick;
        Some(texture.clone())
    }

    pub fn insert_lowres(&mut self, page: u32, theme: Theme, texture: gdk::MemoryTexture) {
        self.tick += 1;
        self.lowres_bytes += (texture.width() * texture.height() * 4) as usize;
        if let Some((old, _)) = self.lowres.insert((page, theme), (texture, self.tick)) {
            self.lowres_bytes -= (old.width() * old.height() * 4) as usize;
        }
        self.lowres_bytes -= drop_oldest(&mut self.lowres, self.lowres_bytes, LOWRES_BUDGET);
    }

    /// Remap cached renders after page edits, dropping deleted pages and advancing the
    /// generation. Retaining surviving textures avoids blank pages and thumbnails during
    /// rerendering.
    pub fn repage(&mut self, map: impl Fn(usize) -> Option<usize>) {
        let page = |p: u32| map(p as usize).map(|p| p as u32);
        self.generation += 1;
        self.tiles = std::mem::take(&mut self.tiles)
            .into_iter()
            .filter_map(|(key, tile)| match page(key.page) {
                Some(page) => Some((TileKey { page, ..key }, tile)),
                None => {
                    self.bytes -= bytes_of(&tile.0);
                    None
                }
            })
            .collect();
        self.lowres = std::mem::take(&mut self.lowres)
            .into_iter()
            .filter_map(|((at, theme), low)| match page(at) {
                Some(at) => Some(((at, theme), low)),
                None => {
                    self.lowres_bytes -= bytes_of(&low.0);
                    None
                }
            })
            .collect();
    }

    /// Let go of every tile and keep the stand-ins: what a tab nobody is looking at holds on to.
    pub fn drop_tiles(&mut self) {
        self.tiles.clear();
        self.bytes = 0;
    }

    pub fn clear(&mut self) {
        self.tiles.clear();
        self.lowres.clear();
        self.bytes = 0;
        self.lowres_bytes = 0;
    }

    /// Invalidate one page while retaining tiles at the visible scale and scheme until
    /// replacements arrive. This avoids flashes after ink or highlight changes without
    /// rerendering unrelated pages. Retained stale tiles are requested again by
    /// [`super::PdfView::refresh_page`]; other scales are dropped to prevent later stale display.
    pub fn forget_page_except(&mut self, page: u32, scale_milli: u32, theme: Theme) {
        self.generation += 1;
        let (tiles, lowres) = (&mut self.bytes, &mut self.lowres_bytes);
        self.tiles.retain(|key, (texture, _)| {
            let keep = key.page != page || (key.scale_milli == scale_milli && key.theme == theme);
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

/// A part of the content in logical pixels: its span across and its span down.
pub(super) type Area = ((f64, f64), (f64, f64));

/// Tile request area in logical pixels at device scale `sf`: one viewport above and below, and
/// one tile on each side. Limiting requests to this area prevents high-zoom page renders from
/// exceeding [`BUDGET`] and causing eviction/rerender loops.
pub(super) fn reach((across, down): Area, sf: i32) -> Area {
    let tile = f64::from(TILE) / f64::from(sf);
    let tall = down.1 - down.0;
    (
        (across.0 - tile, across.1 + tile),
        (down.0 - tall, down.1 + tall),
    )
}

/// The tiles along one axis of a page `count` tiles long that cover `span` of the view, with the
/// page starting at `origin`: logical pixels throughout, `sf` device pixels to one.
pub(super) fn tiles_within(origin: f32, span: (f64, f64), sf: i32, count: i32) -> Range<i32> {
    let tile = f64::from(TILE) / f64::from(sf);
    let at = |x: f64| ((x - f64::from(origin)) / tile).clamp(0.0, f64::from(count));
    at(span.0).floor() as i32..at(span.1).ceil() as i32
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

    /// A page four tiles long starting 100 px down the view, on a 2x screen, where a tile is 256
    /// logical pixels: a span takes every tile it touches and none off the page.
    #[test]
    fn a_span_takes_the_tiles_it_touches() {
        assert_eq!(tiles_within(100.0, (0.0, 50.0), 2, 4), 0..0);
        assert_eq!(tiles_within(100.0, (300.0, 400.0), 2, 4), 0..2);
        assert_eq!(tiles_within(100.0, (0.0, 5000.0), 2, 4), 0..4);
        assert_eq!(tiles_within(100.0, (2000.0, 3000.0), 2, 4), 4..4);
    }

    /// Requested tiles must fit within eviction's three-quarter budget to avoid rerender loops.
    /// Exercise a 3840 × 2160 device-pixel viewport at 2× with a page break splitting a tile
    /// row.
    #[test]
    fn a_4k_view_and_its_reach_fit_the_budget() {
        let (across, down) = reach(((0.0, 1920.0), (0.0, 1080.0)), 2);
        // Device pixels over a tile, and one more for a page's grid that is not the screen's.
        let tiles = |(from, to): (f64, f64)| ((to - from) * 2.0 / f64::from(TILE)).ceil() + 1.0;
        let wanted = tiles(across) * (tiles(down) + 1.0);
        let bytes = wanted * f64::from(TILE * TILE * 4);
        assert!(bytes <= (BUDGET / 4 * 3) as f64, "{wanted} tiles");
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

    /// A page that changed makes the cache a generation newer, which is what tells a view that
    /// the same tiles it asked for before are wanted again: a stroke and then an erase on one
    /// part of a page make the one list twice.
    #[test]
    fn a_changed_page_is_a_new_generation() {
        let mut cache = Cache::default();
        let before = cache.generation();
        cache.forget_page_except(0, 1000, Theme::Plain);
        assert_ne!(cache.generation(), before);
    }
}
