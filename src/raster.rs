// SPDX-License-Identifier: LGPL-3.0-or-later
//! Coverage of polygons, antialiased by exact area. Each edge adds the area it covers to the
//! cells of the columns it crosses; summing down the columns, a whole row at a time, gives every
//! pixel's winding, its absolute value clamped to one being the coverage (so overlapping parts
//! don't cancel out).

use std::simd::prelude::*;

use crate::bitmap::{Bitmap, stride_for};
use crate::outline::Pt;
use crate::simd::dispatch;

/// Rasterizes the polylines, given relative to the mask's corner, into a `w`×`h` mask at
/// `(left, top)`.
pub(crate) fn fill(polys: &[Vec<Pt>], left: i32, top: i32, w: i32, h: i32) -> Bitmap {
    let mut bm = Bitmap::new(left, top, w, h);

    if bm.is_empty() {
        return bm;
    }

    let (w, h, stride) = (w as usize, h as usize, bm.stride);
    // Two rows more: edges below the mask land in the first.
    let mut acc = vec![0f32; stride * (h + 2)];

    for poly in polys {
        let n = poly.len();

        for i in 0..n {
            line(&mut acc, stride, w, h, poly[i], poly[(i + 1) % n]);
        }
    }

    integrate(&acc, &mut bm.data, stride, h);
    bm
}

/// Adds an edge's area into the cells of the columns it crosses; parts above or below the mask
/// count as running along its top or bottom.
fn line(acc: &mut [f32], stride: usize, w: usize, h: usize, p0: Pt, p1: Pt) {
    if p0.x == p1.x || !(p0.x.is_finite() && p1.x.is_finite() && p0.y.is_finite() && p1.y.is_finite()) {
        return;
    }

    let hf = h as f64;
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
            span(acc, stride, w, h, crate::outline::pt(a.x, y), crate::outline::pt(b.x, y));
        } else {
            span(acc, stride, w, h, a, b);
        }
    }
}

fn span(acc: &mut [f32], stride: usize, w: usize, h: usize, p0: Pt, p1: Pt) {
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
        let cell = |acc: &mut [f32], row: usize, v: f32| acc[row * stride + cx] += v;
        let y0f = ya.floor();
        let y0i = y0f as usize;
        let y1c = yb.ceil();
        let y1i = y1c as usize;

        if y1i <= y0i + 1 {
            let ymf = (0.5 * (ya + yb) - y0f) as f32;
            cell(acc, y0i, d - d * ymf);
            cell(acc, y0i + 1, d * ymf);
        } else {
            let s = (1.0 / (yb - ya)) as f32;
            let yf0 = (ya - y0f) as f32;
            let a0 = 0.5 * s * (1.0 - yf0) * (1.0 - yf0);
            let yf1 = (yb - y1c + 1.0) as f32;
            let am = 0.5 * s * yf1 * yf1;
            cell(acc, y0i, d * a0);

            if y1i == y0i + 2 {
                cell(acc, y0i + 1, d * (1.0 - a0 - am));
            } else {
                let a1 = s * (1.5 - yf0);
                cell(acc, y0i + 1, d * (a1 - a0));

                for row in y0i + 2..y1i - 1 {
                    cell(acc, row, d * s);
                }

                let a2 = a1 + (y1i - y0i - 3) as f32 * s;
                cell(acc, y1i - 1, d * (1.0 - a2 - am));
            }

            cell(acc, y1i, d * am);
        }

        y = ynext;
        cx += 1;
    }
}

#[inline(always)]
fn integrate_kernel<const N: usize>(acc: &[f32], out: &mut [u8], stride: usize, h: usize) {
    let mut run = vec![0f32; stride];
    let (scale, max) = (Simd::<f32, N>::splat(256.0), Simd::<f32, N>::splat(255.0));

    for y in 0..h {
        let src = &acc[y * stride..][..stride];
        let dst = &mut out[y * stride..][..stride];

        for x in (0..stride).step_by(N) {
            let r = Simd::<f32, N>::from_slice(&run[x..]) + Simd::from_slice(&src[x..]);
            r.copy_to_slice(&mut run[x..x + N]);
            let v = (r.abs() * scale).simd_min(max);
            v.cast::<u8>().copy_to_slice(&mut dst[x..x + N]);
        }
    }
}

dispatch!(fn integrate(acc: &[f32], out: &mut [u8], stride: usize, h: usize) = integrate_kernel::<16, 32>);

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
/// filled apart, as each is meant on its own.
pub(crate) fn fill_groups(groups: &[&[Vec<Pt>]]) -> Option<Bitmap> {
    let all: Vec<Vec<Pt>> = groups.iter().flat_map(|g| g.iter().cloned()).collect();
    let (l, t, w, h) = bounds(&all)?;
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
    fn clips_to_the_mask() {
        let bm = fill(&[rect(-5.0, -5.0, 2.0, 2.0)], 0, 0, 4, 4);
        assert_eq!(bm.data[0], 255);
        assert_eq!(bm.data[bm.stride + 1], 255);
        assert_eq!(bm.data[2 * bm.stride + 2], 0);
    }
}
