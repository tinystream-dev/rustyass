// SPDX-License-Identifier: LGPL-3.0-or-later
//! Coverage of polygons, antialiased by exact area. Each edge adds the area it covers to the
//! cells of the columns it crosses; summing down the columns, a whole row at a time, gives every
//! pixel's winding, its absolute value clamped to one being the coverage (so overlapping parts
//! don't cancel out).

use std::simd::prelude::*;

use crate::bitmap::{Bitmap, stride_for};
use crate::outline::Pt;
use crate::simd::dispatch;

/// Columns a bit of [`Cells::touched`] stands for.
const BLOCK: usize = 32;
/// Masks with this many pixels or more keep track of which blocks edges touched.
const TRACKED: usize = 1 << 16;

/// Where edges add their areas: a cell per pixel, and two rows more (edges below the mask land in
/// the first).
struct Cells {
    acc: Vec<f32>,
    stride: usize,
    /// For large masks, a bit per row and [`BLOCK`] columns that edges touched; the rest of a row
    /// comes out as the row above did, so it's neither read nor summed. Empty for small masks.
    touched: Vec<u64>,
    words: usize,
}

impl Cells {
    #[inline(always)]
    fn add(&mut self, row: usize, x: usize, v: f32) {
        self.acc[row * self.stride + x] += v;

        if !self.touched.is_empty() {
            let b = x / BLOCK;
            self.touched[row * self.words + b / 64] |= 1 << (b % 64);
        }
    }
}

/// Rasterizes the polylines, given relative to the mask's corner, into a `w`×`h` mask at
/// `(left, top)`.
pub(crate) fn fill(polys: &[Vec<Pt>], left: i32, top: i32, w: i32, h: i32) -> Bitmap {
    fill_with(polys, left, top, w, h, stride_for(w) * h.max(0) as usize >= TRACKED)
}

fn fill_with(polys: &[Vec<Pt>], left: i32, top: i32, w: i32, h: i32, tracked: bool) -> Bitmap {
    let mut bm = Bitmap::new(left, top, w, h);

    if bm.is_empty() {
        return bm;
    }

    let (w, h, stride) = (w as usize, h as usize, bm.stride);
    let words = (stride / BLOCK).div_ceil(64);
    let mut cells = Cells {
        acc: vec![0f32; stride * (h + 2)],
        stride,
        touched: if tracked { vec![0; words * (h + 2)] } else { Vec::new() },
        words,
    };

    for poly in polys {
        let n = poly.len();

        for i in 0..n {
            line(&mut cells, w, h, poly[i], poly[(i + 1) % n]);
        }
    }

    if tracked {
        integrate_touched(&cells.acc, &mut bm.data, stride, h, &cells.touched, words);
    } else {
        integrate(&cells.acc, &mut bm.data, stride, h);
    }

    bm
}

/// Adds an edge's area into the cells of the columns it crosses; parts above or below the mask
/// count as running along its top or bottom.
fn line(cells: &mut Cells, w: usize, h: usize, p0: Pt, p1: Pt) {
    if p0.x == p1.x || !(p0.x.is_finite() && p1.x.is_finite() && p0.y.is_finite() && p1.y.is_finite()) {
        return;
    }

    let hf = h as f64;

    if (0.0..=hf).contains(&p0.y) && (0.0..=hf).contains(&p1.y) {
        span(cells, w, h, p0, p1);
        return;
    }

    let mut cuts = [0.0, 1.0, 1.0, 1.0];
    let mut n = 1;

    for edge in [0.0, hf] {
        let t = (edge - p0.y) / (p1.y - p0.y);

        if t > 0.0 && t < 1.0 {
            cuts[n] = t;
            n += 1;
        }
    }

    cuts[..=n].sort_by(f64::total_cmp);
    let at = |t: f64| crate::outline::pt(p0.x + (p1.x - p0.x) * t, p0.y + (p1.y - p0.y) * t);

    for i in 0..n {
        let (a, b) = (at(cuts[i]), at(cuts[i + 1]));
        let mid = (a.y + b.y) * 0.5;

        if mid <= 0.0 || mid >= hf {
            let y = if mid <= 0.0 { 0.0 } else { hf };
            span(cells, w, h, crate::outline::pt(a.x, y), crate::outline::pt(b.x, y));
        } else {
            span(cells, w, h, a, b);
        }
    }
}

fn span(cells: &mut Cells, w: usize, h: usize, p0: Pt, p1: Pt) {
    if p0.x == p1.x {
        return;
    }

    let (dir, p0, p1) = if p0.x < p1.x { (1.0, p0, p1) } else { (-1.0, p1, p0) };
    let x0 = p0.x.max(0.0);
    let x1 = p1.x.min(w as f64);

    if x0 >= x1 {
        return;
    }

    let dydx = (p1.y - p0.y) / (p1.x - p0.x);
    let hf = h as f64;
    let mut y = p0.y + (x0 - p0.x) * dydx;
    let mut cx = x0.floor() as usize;

    while (cx as f64) < x1 && cx < w {
        let xa = x0.max(cx as f64);
        let xb = x1.min(cx as f64 + 1.0);
        let dx = xb - xa;
        let ynext = y + dydx * dx;
        let d = (dx * dir) as f32;

        let (ya, yb) = if y < ynext { (y, ynext) } else { (ynext, y) };
        let (ya, yb) = (ya.clamp(0.0, hf), yb.clamp(0.0, hf));
        let mut cell = |row: usize, v: f32| cells.add(row, cx, v);
        let y0f = ya.floor();
        let y0i = y0f as usize;
        let y1c = yb.ceil();
        let y1i = y1c as usize;

        if y1i <= y0i + 1 {
            let ymf = (0.5 * (ya + yb) - y0f) as f32;
            cell(y0i, d - d * ymf);
            cell(y0i + 1, d * ymf);
        } else {
            let s = (1.0 / (yb - ya)) as f32;
            let yf0 = (ya - y0f) as f32;
            let a0 = 0.5 * s * (1.0 - yf0) * (1.0 - yf0);
            let yf1 = (yb - y1c + 1.0) as f32;
            let am = 0.5 * s * yf1 * yf1;
            cell(y0i, d * a0);

            if y1i == y0i + 2 {
                cell(y0i + 1, d * (1.0 - a0 - am));
            } else {
                let a1 = s * (1.5 - yf0);
                cell(y0i + 1, d * (a1 - a0));

                for row in y0i + 2..y1i - 1 {
                    cell(row, d * s);
                }

                let a2 = a1 + (y1i - y0i - 3) as f32 * s;
                cell(y1i - 1, d * (1.0 - a2 - am));
            }

            cell(y1i, d * am);
        }

        y = ynext;
        cx += 1;
    }
}

/// Adds `N` cells from `x` on to the windings so far, and writes their coverage.
#[inline(always)]
fn sum<const N: usize>(run: &mut [f32], src: &[f32], dst: &mut [u8], x: usize) {
    let r = Simd::<f32, N>::from_slice(&run[x..]) + Simd::from_slice(&src[x..]);
    r.copy_to_slice(&mut run[x..x + N]);
    // Clamped with a select, as a float min's NaN rules and saturating casts aren't single
    // instructions on wasm and x86_64.
    let a = r.abs();
    let v = a.simd_lt(Simd::splat(1.0)).select(a * Simd::splat(256.0), Simd::splat(255.0));
    // SAFETY: `v` is in 0..256.
    unsafe { v.to_int_unchecked::<i32>() }.cast::<u8>().copy_to_slice(&mut dst[x..x + N]);
}

#[inline(always)]
fn integrate_kernel<const N: usize>(acc: &[f32], out: &mut [u8], stride: usize, h: usize) {
    let mut run = vec![0f32; stride];

    for y in 0..h {
        let src = &acc[y * stride..][..stride];
        let dst = &mut out[y * stride..][..stride];

        for x in (0..stride).step_by(N) {
            sum::<N>(&mut run, src, dst, x);
        }
    }
}

/// [`integrate_kernel`], summing only the blocks edges touched.
#[inline(always)]
fn integrate_touched_kernel<const N: usize>(
    acc: &[f32],
    out: &mut [u8],
    stride: usize,
    h: usize,
    touched: &[u64],
    words: usize,
) {
    let mut run = vec![0f32; stride];

    for y in 0..h {
        if y > 0 {
            out.copy_within((y - 1) * stride..y * stride, y * stride);
        }

        let src = &acc[y * stride..][..stride];
        let dst = &mut out[y * stride..][..stride];

        for (i, &word) in touched[y * words..][..words].iter().enumerate() {
            let mut bits = word;

            while bits != 0 {
                let block = (i * 64 + bits.trailing_zeros() as usize) * BLOCK;
                bits &= bits - 1;

                for x in (block..block + BLOCK).step_by(N) {
                    sum::<N>(&mut run, src, dst, x);
                }
            }
        }
    }
}

dispatch!(fn integrate(acc: &[f32], out: &mut [u8], stride: usize, h: usize) = integrate_kernel::<16, 32>);
dispatch!(
    fn integrate_touched(acc: &[f32], out: &mut [u8], stride: usize, h: usize, touched: &[u64], words: usize)
        = integrate_touched_kernel::<16, 32>
);

/// The mask's box: the polylines' bounds, a pixel more on each side.
pub(crate) fn bounds(polys: &[Vec<Pt>]) -> Option<(i32, i32, i32, i32)> {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);

    for p in polys.iter().flatten() {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }

    if x0 > x1 || y0 > y1 || !(x1 - x0).is_finite() || !(y1 - y0).is_finite() {
        return None;
    }

    const LIMIT: f64 = 1e6;

    if x0 < -LIMIT || y0 < -LIMIT || x1 > LIMIT || y1 > LIMIT {
        return None;
    }

    let (l, t) = ((x0 - 1.0 / 64.0).floor() as i32, (y0 - 1.0 / 64.0).floor() as i32);
    let (r, b) = ((x1 + 2.0 - 1.0 / 64.0).floor() as i32, (y1 + 2.0 - 1.0 / 64.0).floor() as i32);
    Some((l, t, r - l, b - t))
}

/// Coverage of each group, merged by taking the larger: a border's two offset outlines are
/// filled apart, as each is meant on its own. Only what's within `clip` (left, top, right and
/// bottom), if given, is drawn.
pub(crate) fn fill_groups(groups: &[&[Vec<Pt>]], clip: Option<[i32; 4]>) -> Option<Bitmap> {
    let all: Vec<Vec<Pt>> = groups.iter().flat_map(|g| g.iter().cloned()).collect();
    let (mut l, mut t, mut w, mut h) = bounds(&all)?;
    let full = [l, t, l + w, t + h];
    let mut cut = None;

    // Columns are summed from the top and edges above the mask count along its top, so a cut
    // box covers the same as the whole one does.
    if let Some([cl, ct, cr, cb]) = clip
        && (l < cl || t < ct || l + w > cr || t + h > cb)
    {
        let (r, b) = ((l + w).min(cr), (t + h).min(cb));
        (l, t) = (l.max(cl), t.max(ct));
        (w, h) = ((r - l).max(0), (b - t).max(0));
        cut = Some(full);

        if w == 0 || h == 0 {
            return Some(Bitmap { cut, ..Bitmap::new(l, t, 0, 0) });
        }
    }
    let mut out: Option<Bitmap> = None;

    for g in groups {
        let shifted: Vec<Vec<Pt>> =
            g.iter().map(|p| p.iter().map(|q| crate::outline::pt(q.x - l as f64, q.y - t as f64)).collect()).collect();
        let bm = fill(&shifted, l, t, w, h);

        match &mut out {
            Some(o) => o.max(&bm),
            None => out = Some(bm),
        }
    }

    out.map(|mut b| {
        b.stride = stride_for(b.w);
        b.cut = cut;
        b
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::outline::pt;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Pt> {
        vec![pt(x0, y0), pt(x1, y0), pt(x1, y1), pt(x0, y1)]
    }

    #[test]
    fn fills_a_square() {
        let bm = fill(&[rect(1.0, 1.0, 3.0, 3.0)], 0, 0, 4, 4);
        let px = |x: usize, y: usize| bm.data[y * bm.stride + x];
        assert_eq!(px(1, 1), 255);
        assert_eq!(px(2, 2), 255);
        assert_eq!(px(0, 0), 0);
        assert_eq!(px(3, 1), 0);
    }

    #[test]
    fn antialiases_by_area() {
        let bm = fill(&[rect(0.5, 0.0, 2.0, 2.0)], 0, 0, 3, 2);
        assert_eq!(bm.data[0], 128);

        let tri = vec![pt(0.0, 0.0), pt(2.0, 0.0), pt(0.0, 2.0)];
        let bm = fill(&[tri], 0, 0, 2, 2);
        assert_eq!(bm.data[1], 128);
        assert_eq!(bm.data[bm.stride + 1], 0);
    }

    #[test]
    fn overlaps_dont_cancel() {
        let back = vec![pt(0.0, 0.0), pt(0.0, 4.0), pt(4.0, 4.0), pt(4.0, 0.0)];
        let bm = fill(&[rect(0.0, 0.0, 4.0, 4.0), back], 0, 0, 4, 4);
        assert_eq!(bm.data[0], 0);

        let bm = fill(&[rect(0.0, 0.0, 4.0, 4.0), rect(1.0, 1.0, 3.0, 3.0)], 0, 0, 4, 4);
        assert_eq!(bm.data[bm.stride + 1], 255);
    }

    #[test]
    fn tracking_blocks_changes_nothing() {
        let star: Vec<Pt> = (0..14)
            .map(|i| {
                let (a, r) = (i as f64 * std::f64::consts::PI / 7.0, if i % 2 == 0 { 390.0 } else { 120.0 });
                pt(400.5 + r * a.cos(), 300.25 + r * a.sin())
            })
            .collect();
        let polys = [star, rect(10.3, 10.6, 700.2, 40.9), rect(-20.0, 500.5, 30.0, 700.0)];
        let dense = fill_with(&polys, 0, 0, 800, 600, false);
        let tracked = fill_with(&polys, 0, 0, 800, 600, true);
        assert!(dense.data.contains(&255));
        assert_eq!(dense.data, tracked.data);
    }

    #[test]
    fn clips_to_the_mask() {
        let bm = fill(&[rect(-5.0, -5.0, 2.0, 2.0)], 0, 0, 4, 4);
        assert_eq!(bm.data[0], 255);
        assert_eq!(bm.data[bm.stride + 1], 255);
        assert_eq!(bm.data[2 * bm.stride + 2], 0);
    }
}
