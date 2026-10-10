// SPDX-License-Identifier: LGPL-3.0-or-later
//! Borders: an outline's offsets by an ellipse of radii `bx` and `by` to either side. Filling the
//! two offsets apart and keeping the larger coverage gives the outline grown by the ellipse, even
//! where the outline crosses itself.
//!
//! Corners get round joins on their outer side; on the inner side the offset goes through the
//! corner itself, so that what it covers twice doesn't cancel out.

use std::f64::consts::PI;

use crate::outline::{Pt, pt};

/// The outer and inner offsets of the closed polylines, `tolerance` being how far arcs may be
/// from round.
pub(crate) fn stroke(polys: &[Vec<Pt>], bx: f64, by: f64, tolerance: f64) -> [Vec<Vec<Pt>>; 2] {
    let mut out = [Vec::new(), Vec::new()];
    let (bx, by) = (bx.max(1e-3), by.max(1e-3));
    let r = bx.max(by);
    let tol = tolerance.min(r * 0.5);
    let merge_cos = 1.0 - tol / r;
    // The largest angle an arc's chord can span and stay within the tolerance.
    let step = (2.0 * (1.0 - tol / r).clamp(-1.0, 1.0).acos()).clamp(PI / 90.0, PI / 4.0);
    let offset = |n: Pt, sign: f64| pt(sign * bx * n.x, sign * by * n.y);

    for poly in polys {
        let mut pts: Vec<Pt> = Vec::with_capacity(poly.len());

        for &p in poly {
            let near = |q: Pt| ((p.x - q.x) / bx).hypot((p.y - q.y) / by) * r < tol * 0.25;

            if !pts.last().is_some_and(|&q| near(q)) {
                pts.push(p);
            }
        }

        while pts.len() > 1 && {
            let (a, b) = (pts[0], pts[pts.len() - 1]);
            ((a.x - b.x) / bx).hypot((a.y - b.y) / by) * r < tol * 0.25
        } {
            pts.pop();
        }

        let n = pts.len();

        if n == 0 {
            continue;
        }

        if n == 1 {
            let c = pts[0];
            let k = (2.0 * PI / step).ceil().max(8.0) as usize;
            out[0]
                .push((0..k).map(|i| c + offset(rotate(pt(1.0, 0.0), 2.0 * PI * i as f64 / k as f64), 1.0)).collect());
            continue;
        }

        let normals: Vec<Pt> = (0..n)
            .map(|i| {
                let d = pts[(i + 1) % n] - pts[i];
                let v = pt(d.y / by, -d.x / bx);
                v * (1.0 / v.x.hypot(v.y))
            })
            .collect();

        for (side, sign) in [(0, 1.0), (1, -1.0)] {
            let mut line = Vec::with_capacity(n * 3);

            for j in 0..n {
                let p = pts[j];
                let (n0, n1) = (normals[(j + n - 1) % n], normals[j]);
                let c = n0.x * n1.x + n0.y * n1.y;

                if c > merge_cos {
                    line.push(p + offset((n0 + n1) * (1.0 / (1.0 + c)), sign));
                    continue;
                }

                let s = n0.x * n1.y - n0.y * n1.x;
                let concave = if s < 0.0 { side == 0 } else { side == 1 };

                if concave {
                    line.push(p + offset(n0, sign));
                    line.push(p);
                    line.push(p + offset(n1, sign));
                    continue;
                }

                let angle = if s == 0.0 && c < 0.0 { if side == 0 { PI } else { -PI } } else { s.atan2(c) };
                let k = (angle.abs() / step).ceil().max(1.0) as usize;

                for i in 0..=k {
                    line.push(p + offset(rotate(n0, angle * i as f64 / k as f64), sign));
                }
            }

            out[side].push(line);
        }
    }

    out
}

fn rotate(v: Pt, a: f64) -> Pt {
    let (s, c) = a.sin_cos();
    pt(v.x * c - v.y * s, v.x * s + v.y * c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster;

    fn coverage(polys: &[Vec<Pt>], bx: f64, by: f64) -> crate::bitmap::Bitmap {
        let sides = stroke(polys, bx, by, 0.05);
        raster::fill_groups(&[&sides[0], &sides[1]]).unwrap()
    }

    #[test]
    fn grows_a_square() {
        let square = vec![pt(10.0, 10.0), pt(20.0, 10.0), pt(20.0, 20.0), pt(10.0, 20.0)];
        let bm = coverage(&[square], 2.0, 2.0);
        let px = |x: i32, y: i32| bm.data[(y - bm.top) as usize * bm.stride + (x - bm.left) as usize];
        assert_eq!(px(15, 15), 255);
        assert_eq!(px(9, 15), 255);
        assert_eq!(px(8, 15), 255);
        assert_eq!(px(7, 15), 0);
        assert_eq!(px(19, 19), 255);
        // The corner is rounded.
        assert!(px(8, 8) < 200);
    }

    #[test]
    fn thin_strips_stay_filled() {
        let strip = vec![pt(0.0, 10.0), pt(30.0, 10.0), pt(30.0, 11.0), pt(0.0, 11.0)];
        let bm = coverage(&[strip], 4.0, 4.0);
        let px = |x: i32, y: i32| bm.data[(y - bm.top) as usize * bm.stride + (x - bm.left) as usize];

        for y in 7..14 {
            assert_eq!(px(15, y), 255, "{y}");
        }
    }

    #[test]
    fn dots_become_ellipses() {
        let bm = coverage(&[vec![pt(10.0, 10.0)]], 4.0, 2.0);
        assert!(bm.w >= 8 && bm.w <= 11 && bm.h >= 4 && bm.h <= 7, "{}x{}", bm.w, bm.h);
    }
}
