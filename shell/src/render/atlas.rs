//! Texture atlases.
//!
//! `ShelfAtlas`: rows ("shelves") of similar height, filled left to right; entries live until the atlas is
//! full, then everything is dropped at once (`clear`) and re-rasterized on demand. Holds glyphs and shadow
//! slices, which are small and change rarely.
//!
//! `CellAtlas`: fixed square cells with least-recently-used eviction, for icons (any size up to the cell).

use super::batch::Rect;

const PAD: u32 = 1;

pub struct ShelfAtlas {
    pub w: u32,
    pub h: u32,
    shelves: Vec<Shelf>,
    next_y: u32,
}

struct Shelf {
    y: u32,
    h: u32,
    x: u32,
}

impl ShelfAtlas {
    pub fn new(w: u32, h: u32) -> ShelfAtlas {
        ShelfAtlas { w, h, shelves: Vec::new(), next_y: 0 }
    }

    pub fn clear(&mut self) {
        self.shelves.clear();
        self.next_y = 0;
    }

    /// Space for a w x h image (plus a 1 px gap so linear filtering does not bleed). None when full.
    pub fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let (pw, ph) = (w + PAD, h + PAD);
        if pw > self.w {
            return None;
        }
        // Best-fitting existing shelf: tall enough, wasting at most half its height.
        let mut best: Option<usize> = None;
        for (i, s) in self.shelves.iter().enumerate() {
            if s.h >= ph && s.h <= ph * 2 && s.x + pw <= self.w && best.is_none_or(|b| self.shelves[b].h > s.h) {
                best = Some(i);
            }
        }
        if best.is_none() {
            let sh = ph.next_multiple_of(4);
            if self.next_y + sh > self.h {
                return None;
            }
            self.shelves.push(Shelf { y: self.next_y, h: sh, x: 0 });
            self.next_y += sh;
            best = Some(self.shelves.len() - 1);
        }
        let s = &mut self.shelves[best?];
        let pos = (s.x, s.y);
        s.x += pw;
        Some(pos)
    }
}

pub struct CellAtlas {
    pub cell: u32,
    cols: u32,
    /// Per cell: the frame it was last used in, or None when free.
    used: Vec<Option<u64>>,
}

impl CellAtlas {
    pub fn new(size: u32, cell: u32) -> CellAtlas {
        let cols = size / cell;
        CellAtlas { cell, cols, used: vec![None; (cols * cols) as usize] }
    }

    /// A cell for an entry used in `frame`: a free one, else the least recently used one that was not used
    /// in this frame. Returns (index, evicted: whether it held something).
    pub fn alloc(&mut self, frame: u64) -> Option<(usize, bool)> {
        if let Some(i) = self.used.iter().position(|u| u.is_none()) {
            self.used[i] = Some(frame);
            return Some((i, false));
        }
        let (i, last) = self.used.iter().enumerate().filter_map(|(i, u)| u.map(|f| (i, f))).min_by_key(|&(_, f)| f)?;
        if last >= frame {
            return None; // every cell is on screen right now
        }
        self.used[i] = Some(frame);
        Some((i, true))
    }

    pub fn touch(&mut self, i: usize, frame: u64) {
        self.used[i] = Some(frame);
    }

    pub fn origin(&self, i: usize) -> (u32, u32) {
        ((i as u32 % self.cols) * self.cell, (i as u32 / self.cols) * self.cell)
    }

    /// Texel rect of an entry of size w x h stored at cell i.
    pub fn rect(&self, i: usize, w: u32, h: u32) -> Rect {
        let (x, y) = self.origin(i);
        Rect::new(x as f32, y as f32, w as f32, h as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelves_fill_and_report_full() {
        let mut a = ShelfAtlas::new(64, 32);
        let mut n = 0;
        while a.alloc(15, 15).is_some() {
            n += 1;
        }
        assert_eq!(n, 4 * 2);
        a.clear();
        assert_eq!(a.alloc(15, 15), Some((0, 0)));
    }

    #[test]
    fn cells_evict_least_recent_but_not_current() {
        let mut c = CellAtlas::new(256, 128); // 4 cells
        for f in 0..4 {
            assert_eq!(c.alloc(f), Some((f as usize, false)));
        }
        c.touch(0, 10);
        assert_eq!(c.alloc(10), Some((1, true)));
        c.touch(2, 10);
        c.touch(3, 10);
        assert_eq!(c.alloc(10), None);
    }
}
