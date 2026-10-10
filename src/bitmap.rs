// SPDX-License-Identifier: LGPL-3.0-or-later
//! 8-bit masks and the arithmetic that combines them.

use std::simd::prelude::*;

use crate::simd::dispatch;

/// Rows are padded to this many bytes.
pub(crate) const ALIGN: usize = 32;

/// A mask, `left` and `top` being where its first pixel goes.
#[derive(Clone, Default, Debug)]
pub(crate) struct Bitmap {
    pub left: i32,
    pub top: i32,
    pub w: i32,
    pub h: i32,
    pub stride: usize,
    pub data: Vec<u8>,
}

pub(crate) fn stride_for(w: i32) -> usize {
    (w.max(0) as usize).div_ceil(ALIGN) * ALIGN
}

impl Bitmap {
    pub fn new(left: i32, top: i32, w: i32, h: i32) -> Bitmap {
        let stride = stride_for(w);
        Bitmap { left, top, w, h, stride, data: vec![0; stride * h.max(0) as usize] }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn right(&self) -> i32 {
        self.left + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.top + self.h
    }

    pub fn bytes(&self) -> usize {
        self.data.len()
    }

    /// Adds `src`, moved by `at`, saturating, wherever it overlaps.
    pub fn add_at(&mut self, src: &Bitmap, at: (i32, i32)) {
        if let Some((d, s, w, h)) = overlap_at(self, src, at) {
            add_rows(&mut self.data[d..], self.stride, &src.data[s..], src.stride, w, h);
        }
    }

    /// The maximum of the two, wherever they overlap.
    pub fn max(&mut self, src: &Bitmap) {
        if let Some((d, s, w, h)) = overlap(self, src) {
            max_rows(&mut self.data[d..], self.stride, &src.data[s..], src.stride, w, h);
        }
    }

    /// Takes the glyph out of its outline, so a translucent fill doesn't show the border under it.
    pub fn fix_outline(g: &Bitmap, o: &mut Bitmap) {
        if let Some((d, s, w, h)) = overlap(o, g) {
            let stride = o.stride;
            fix_outline_rows(&mut o.data[d..], stride, &g.data[s..], g.stride, w, h);
        }
    }

    /// Moves the mask right and down by a fraction of a pixel, `x` and `y` in 1/64ths.
    pub fn shift(&mut self, x: u32, y: u32) {
        let (w, h, stride) = (self.w as usize, self.h as usize, self.stride);

        if self.is_empty() {
            return;
        }

        if x != 0 {
            for row in self.data.chunks_exact_mut(stride).take(h) {
                let mut carry = 0u8;

                for p in &mut row[..w - 1] {
                    let b = ((*p as u32 * x) >> 6) as u8;
                    *p = *p - b + carry;
                    carry = b;
                }

                row[w - 1] = row[w - 1].wrapping_add(carry);
            }
        }

        if y != 0 {
            shift_down(&mut self.data, stride, w, h, y);
        }
    }
}

/// Where `src` overlaps `dst`: offsets into each, and the size.
fn overlap(dst: &Bitmap, src: &Bitmap) -> Option<(usize, usize, usize, usize)> {
    overlap_at(dst, src, (0, 0))
}

fn overlap_at(dst: &Bitmap, src: &Bitmap, (ax, ay): (i32, i32)) -> Option<(usize, usize, usize, usize)> {
    let (sl, st) = (src.left + ax, src.top + ay);
    let x0 = dst.left.max(sl);
    let y0 = dst.top.max(st);
    let x1 = dst.right().min(sl + src.w);
    let y1 = dst.bottom().min(st + src.h);

    if x0 >= x1 || y0 >= y1 {
        return None;
    }

    let d = (y0 - dst.top) as usize * dst.stride + (x0 - dst.left) as usize;
    let s = (y0 - st) as usize * src.stride + (x0 - sl) as usize;
    Some((d, s, (x1 - x0) as usize, (y1 - y0) as usize))
}

/// Runs `f` on `N` pixels at a time across `w` columns of `h` rows, then pixel by pixel.
#[inline(always)]
fn rows2<const N: usize>(
    dst: &mut [u8],
    ds: usize,
    src: &[u8],
    ss: usize,
    w: usize,
    h: usize,
    f: impl Fn(Simd<u8, N>, Simd<u8, N>) -> Simd<u8, N>,
    g: impl Fn(u8, u8) -> u8,
) {
    for y in 0..h {
        let d = &mut dst[y * ds..y * ds + w];
        let s = &src[y * ss..y * ss + w];
        let mut x = 0;

        while x + N <= w {
            let r = f(Simd::from_slice(&d[x..]), Simd::from_slice(&s[x..]));
            r.copy_to_slice(&mut d[x..x + N]);
            x += N;
        }

        for x in x..w {
            d[x] = g(d[x], s[x]);
        }
    }
}

#[inline(always)]
fn add_kernel<const N: usize>(dst: &mut [u8], ds: usize, src: &[u8], ss: usize, w: usize, h: usize) {
    rows2::<N>(dst, ds, src, ss, w, h, |a, b| a.saturating_add(b), |a, b| a.saturating_add(b));
}

#[inline(always)]
fn max_kernel<const N: usize>(dst: &mut [u8], ds: usize, src: &[u8], ss: usize, w: usize, h: usize) {
    rows2::<N>(dst, ds, src, ss, w, h, |a, b| a.simd_max(b), |a, b| a.max(b));
}

#[inline(always)]
fn fix_outline_kernel<const N: usize>(o: &mut [u8], os: usize, g: &[u8], gs: usize, w: usize, h: usize) {
    rows2::<N>(
        o,
        os,
        g,
        gs,
        w,
        h,
        |o, g| o.simd_gt(g).select(o - (g >> 1), Simd::splat(0)),
        |o, g| if o > g { o - g / 2 } else { 0 },
    );
}

/// `dst = dst * src / 255`, rounded as libass rounds.
#[inline(always)]
fn mul_kernel<const N: usize>(dst: &mut [u8], ds: usize, src: &[u8], ss: usize, w: usize, h: usize) {
    rows2::<N>(
        dst,
        ds,
        src,
        ss,
        w,
        h,
        |a, b| ((a.cast::<u16>() * b.cast::<u16>() + Simd::splat(255)) >> 8).cast(),
        |a, b| ((a as u16 * b as u16 + 255) >> 8) as u8,
    );
}

/// `dst = dst * (255 - src) / 255`: what's outside an inverse clip.
#[inline(always)]
fn imul_kernel<const N: usize>(dst: &mut [u8], ds: usize, src: &[u8], ss: usize, w: usize, h: usize) {
    rows2::<N>(
        dst,
        ds,
        src,
        ss,
        w,
        h,
        |a, b| ((a.cast::<u16>() * (Simd::splat(255) - b.cast::<u16>()) + Simd::splat(255)) >> 8).cast(),
        |a, b| ((a as u16 * (255 - b as u16) + 255) >> 8) as u8,
    );
}

#[inline(always)]
fn shift_down_kernel<const N: usize>(data: &mut [u8], stride: usize, w: usize, h: usize, s: u32) {
    let mul = Simd::<u16, N>::splat(s as u16);
    let mut carry = vec![0u8; w];

    for y in 0..h {
        let row = &mut data[y * stride..y * stride + w];
        let last = y + 1 == h;
        let mut x = 0;

        while x + N <= w {
            let p = Simd::<u8, N>::from_slice(&row[x..]);
            let c = Simd::<u8, N>::from_slice(&carry[x..]);

            if last {
                p.saturating_add(c).copy_to_slice(&mut row[x..x + N]);
            } else {
                let b = ((p.cast::<u16>() * mul) >> 6).cast::<u8>();
                (p - b + c).copy_to_slice(&mut row[x..x + N]);
                b.copy_to_slice(&mut carry[x..x + N]);
            }

            x += N;
        }

        for x in x..w {
            if last {
                row[x] = row[x].wrapping_add(carry[x]);
            } else {
                let b = ((row[x] as u32 * s) >> 6) as u8;
                row[x] = row[x] - b + carry[x];
                carry[x] = b;
            }
        }
    }
}

dispatch!(fn add_rows(dst: &mut [u8], ds: usize, src: &[u8], ss: usize, w: usize, h: usize) = add_kernel::<16, 32>);
dispatch!(fn max_rows(dst: &mut [u8], ds: usize, src: &[u8], ss: usize, w: usize, h: usize) = max_kernel::<16, 32>);
dispatch!(fn fix_outline_rows(o: &mut [u8], os: usize, g: &[u8], gs: usize, w: usize, h: usize) = fix_outline_kernel::<16, 32>);
dispatch!(pub(crate) fn mul_rows(dst: &mut [u8], ds: usize, src: &[u8], ss: usize, w: usize, h: usize) = mul_kernel::<16, 32>);
dispatch!(pub(crate) fn imul_rows(dst: &mut [u8], ds: usize, src: &[u8], ss: usize, w: usize, h: usize) = imul_kernel::<16, 32>);
dispatch!(fn shift_down(data: &mut [u8], stride: usize, w: usize, h: usize, s: u32) = shift_down_kernel::<16, 32>);

#[cfg(test)]
mod tests {
    use super::*;

    fn filled(left: i32, top: i32, w: i32, h: i32, v: u8) -> Bitmap {
        let mut b = Bitmap::new(left, top, w, h);
        b.data.fill(v);
        b
    }

    #[test]
    fn adds_where_they_overlap() {
        let mut a = filled(0, 0, 40, 3, 200);
        a.add_at(&filled(10, 1, 40, 5, 100), (0, 0));
        assert_eq!(a.data[0], 200);
        assert_eq!(a.data[a.stride + 10], 255);
        assert_eq!(a.data[a.stride + 9], 200);
        assert_eq!(a.data[2 * a.stride + 39], 255);
    }

    #[test]
    fn fixes_outlines() {
        let mut o = filled(0, 0, 33, 1, 255);
        Bitmap::fix_outline(&filled(0, 0, 33, 1, 100), &mut o);
        assert!(o.data[..33].iter().all(|&v| v == 205));
    }

    #[test]
    fn shifts_by_a_fraction() {
        let mut b = Bitmap::new(0, 0, 3, 3);
        b.data[b.stride + 1] = 128;
        b.shift(32, 32);
        let px = |x: usize, y: usize| b.data[y * b.stride + x];
        assert_eq!([px(1, 1), px(2, 1), px(1, 2), px(2, 2)], [32, 32, 32, 32]);
    }

    #[test]
    fn multiplies() {
        let mut a = filled(0, 0, 40, 1, 255);
        mul_rows(&mut a.data, 64, &[128u8; 40], 40, 40, 1);
        assert_eq!(a.data[0], 128);
    }
}
