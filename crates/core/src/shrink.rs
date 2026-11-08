//! Shrink straight RGBA8 pixels a band of rows at a time, so a small copy of a large image is
//! made without holding all of its pixels twice.

/// A `width` × `height` image being shrunk to `wide` × `high`, each pixel of the result the mean
/// of the image's pixels under it, each weighted by how much of it lies there and by its alpha,
/// as a box filter takes them.
///
/// Sizes are counted in units small enough to be exact: a result column is `width` of them and a
/// source column `wide`, a result row `height` and a source row `high`.
pub struct Shrink {
    width: u64,
    height: u64,
    wide: u64,
    high: u64,
    /// Each source column's first result column and the units of it there; the rest of its
    /// `wide` units lie in the next one.
    columns: Vec<(usize, u64)>,
    /// The source row given next.
    y: u64,
    /// Premultiplied sums, colour times alpha and then alpha, of the result row the rows given so
    /// far reach into, and of the one after it.
    sums: [Vec<u64>; 2],
    /// Those of the source row being read, across the result's columns.
    line: Vec<u64>,
    out: Vec<u8>,
}

impl Shrink {
    /// Shrink a `width` × `height` image to `wide` × `high`, no larger either way.
    pub fn new(width: u32, height: u32, wide: u32, high: u32) -> Shrink {
        debug_assert!(wide <= width && high <= height && wide > 0 && high > 0);
        let [width, height, wide, high] = [width, height, wide, high].map(u64::from);
        let columns = (0..width)
            .map(|x| {
                let (start, first) = (x * wide, x * wide / width);
                let units = ((x + 1) * wide).min((first + 1) * width) - start;
                (first as usize, units)
            })
            .collect();
        let row = vec![0; wide as usize * 4];
        Shrink {
            width,
            height,
            wide,
            high,
            columns,
            y: 0,
            sums: [row.clone(), row.clone()],
            line: row,
            out: Vec::with_capacity((wide * high * 4) as usize),
        }
    }

    /// The image's next whole rows.
    pub fn rows(&mut self, rgba: &[u8]) {
        for row in rgba.chunks_exact(self.width as usize * 4) {
            self.line.fill(0);
            let pixels = row.as_chunks::<4>().0;
            for (&[r, g, b, a], &(j, units)) in pixels.iter().zip(&self.columns) {
                let a = u64::from(a);
                let v = [r, g, b].map(|c| u64::from(c) * a);
                let v = [v[0], v[1], v[2], a];
                let rest = self.wide - units;
                for (k, v) in v.into_iter().enumerate() {
                    self.line[j * 4 + k] += units * v;
                    if rest > 0 {
                        self.line[(j + 1) * 4 + k] += rest * v;
                    }
                }
            }
            // The row's units in the result row it starts in, the rest in the next.
            let (start, end) = (self.y * self.high, (self.y + 1) * self.high);
            let ends = (start / self.height + 1) * self.height;
            let units = end.min(ends) - start;
            for (s, l) in self.sums[0].iter_mut().zip(&self.line) {
                *s += units * l;
            }
            for (s, l) in self.sums[1].iter_mut().zip(&self.line) {
                *s += (self.high - units) * l;
            }
            self.y += 1;
            if end >= ends {
                self.emit();
            }
        }
    }

    /// The result row now complete, from its sums.
    fn emit(&mut self) {
        let total = self.width * self.height;
        for &[r, g, b, a] in self.sums[0].as_chunks::<4>().0 {
            let colour = |c: u64| (c + a / 2).checked_div(a).unwrap_or(0) as u8;
            let alpha = ((a + total / 2) / total) as u8;
            self.out.extend([colour(r), colour(g), colour(b), alpha]);
        }
        self.sums.swap(0, 1);
        self.sums[1].fill(0);
    }

    /// The shrunk image, straight RGBA8, rows never given transparent.
    pub fn finish(mut self) -> Vec<u8> {
        self.out.resize((self.wide * self.high * 4) as usize, 0);
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shrunk(rgba: &[u8], (width, height): (u32, u32), (wide, high): (u32, u32)) -> Vec<u8> {
        let mut shrink = Shrink::new(width, height, wide, high);
        shrink.rows(rgba);
        shrink.finish()
    }

    fn grey(v: u8) -> [u8; 4] {
        [v, v, v, 255]
    }

    /// Three columns into two: the middle one is shared, a third of each.
    #[test]
    fn each_pixel_is_the_mean_of_those_under_it() {
        let across: Vec<u8> = [0, 90, 180].into_iter().flat_map(grey).collect();
        let means = [grey(30), grey(150)].concat();
        assert_eq!(shrunk(&across, (3, 1), (2, 1)), means);
        // And rows as columns.
        assert_eq!(shrunk(&across, (1, 3), (1, 2)), means);
    }

    /// A transparent pixel lends the mean its transparency but none of its colour.
    #[test]
    fn transparent_pixels_lend_no_colour() {
        let red_on_nothing = [255, 0, 0, 255, 0, 255, 0, 0];
        assert_eq!(shrunk(&red_on_nothing, (2, 1), (1, 1)), [255, 0, 0, 128]);
    }

    /// Rows given one band at a time, of any height, make the image given at once.
    #[test]
    fn bands_of_any_height_make_one_image() {
        let (width, height) = (7, 5);
        let rgba: Vec<u8> = (0..width * height * 4)
            .map(|i| (i * 37 % 251) as u8)
            .collect();
        let whole = shrunk(&rgba, (width, height), (3, 2));
        for band in [1, 2, 3] {
            let mut shrink = Shrink::new(width, height, 3, 2);
            for rows in rgba.chunks(band * width as usize * 4) {
                shrink.rows(rows);
            }
            assert_eq!(shrink.finish(), whole, "bands of {band}");
        }
    }
}
