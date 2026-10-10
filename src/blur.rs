// SPDX-License-Identifier: LGPL-3.0-or-later
//! `\blur`, a gaussian approximated by shrinking, filtering and growing back (libass's cascade),
//! and `\be`, VSFilter's 3×3 box blur. Passes run down columns, all of a row at once; horizontal
//! ones run on the image turned on its side.

use std::simd::prelude::*;

use crate::bitmap::{Bitmap, stride_for};
use crate::simd::dispatch;

/// How far `\be` spreads a mask, so it's padded by that much first.
pub(crate) fn be_padding(be: i32) -> i32 {
    match be {
        ..=3 => be,
        4..=7 => 4,
        _ => 5,
    }
}

#[derive(Clone)]
struct Method {
    level: u32,
    radius: usize,
    coeff: [i16; 8],
}

fn calc_gauss(res: &mut [f64], n: usize, r2: f64) {
    let alpha = 0.5 / r2;
    let mut mul = (-alpha).exp();
    let mul2 = mul * mul;
    let mut cur = (alpha / std::f64::consts::PI).sqrt();
    res[0] = cur;
    cur *= mul;
    res[1] = cur;

    for r in res.iter_mut().take(n).skip(2) {
        mul *= mul2;
        cur *= mul;
        *r = cur;
    }
}

fn coeff_filter(coeff: &mut [f64], n: usize, kernel: &[f64; 4]) {
    let (mut prev1, mut prev2, mut prev3) = (coeff[1], coeff[2], coeff[3]);

    for i in 0..n {
        let res = coeff[i] * kernel[0]
            + (prev1 + coeff[i + 1]) * kernel[1]
            + (prev2 + coeff[i + 2]) * kernel[2]
            + (prev3 + coeff[i + 3]) * kernel[3];
        prev3 = prev2;
        prev2 = prev1;
        prev1 = coeff[i];
        coeff[i] = res;
    }
}

fn calc_matrix(mat: &mut [[f64; 8]; 8], freq: &[f64], n: usize) {
    for i in 0..n {
        mat[i][i] = freq[2 * i + 2] + 3.0 * freq[0] - 4.0 * freq[i + 1];

        for j in i + 1..n {
            let v = freq[i + j + 2] + freq[j - i] + 2.0 * (freq[0] - freq[i + 1] - freq[j + 1]);
            mat[i][j] = v;
            mat[j][i] = v;
        }
    }

    // Inverts it in place.
    for k in 0..n {
        let z = 1.0 / mat[k][k];
        mat[k][k] = 1.0;

        for i in 0..n {
            if i == k {
                continue;
            }

            let mul = mat[i][k] * z;
            mat[i][k] = 0.0;

            for j in 0..n {
                mat[i][j] -= mat[k][j] * mul;
            }
        }

        for j in 0..n {
            mat[k][j] *= z;
        }
    }
}

/// The main filter's kernel: least squares against the gaussian, given the shrinking around it.
fn calc_coeff(mu: &mut [f64; 8], n: usize, r2: f64, mul: f64) {
    let w = 12096.0;

    let kernel = [
        (((3280.0 / w) * mul + 1092.0 / w) * mul + 2520.0 / w) * mul + 5204.0 / w,
        (((-2460.0 / w) * mul - 273.0 / w) * mul - 210.0 / w) * mul + 2943.0 / w,
        (((984.0 / w) * mul - 546.0 / w) * mul - 924.0 / w) * mul + 486.0 / w,
        (((-164.0 / w) * mul + 273.0 / w) * mul - 126.0 / w) * mul + 17.0 / w,
    ];

    let mut freq = [0.0; 17];
    freq[..4].copy_from_slice(&kernel);
    coeff_filter(&mut freq, 7, &kernel);

    let mut vec_freq = [0.0; 12];
    calc_gauss(&mut vec_freq, n + 4, r2 * mul);
    coeff_filter(&mut vec_freq, n + 1, &kernel);

    let mut mat = [[0.0; 8]; 8];
    calc_matrix(&mut mat, &freq, n);

    let mut vec = [0.0; 8];

    for i in 0..n {
        vec[i] = freq[0] - freq[i + 1] - vec_freq[0] + vec_freq[i + 1];
    }

    for i in 0..n {
        let res: f64 = (0..n).map(|j| mat[i][j] * vec[j]).sum();
        mu[i] = res.max(0.0);
    }
}

fn method(r2: f64) -> Method {
    let mut mu = [0.0; 8];

    let (level, radius) = if r2 < 0.5 {
        mu[1] = 0.085 * r2 * r2 * r2;
        mu[0] = 0.5 * r2 - 4.0 * mu[1];
        (0, 4)
    } else {
        let v = (0.11569 * r2 + 0.20591047).sqrt();
        // frexp: v = frac * 2^level, frac in [0.5, 1).
        let mut level = v.log2().floor() as i32 + 1;
        let mut frac = v / 2f64.powi(level);

        if frac < 0.5 {
            frac *= 2.0;
            level -= 1;
        } else if frac >= 1.0 {
            frac /= 2.0;
            level += 1;
        }

        let mul = 0.25f64.powi(level);
        let radius = (8 - ((10.1525 + 0.8335 * mul) * (1.0 - frac)) as i32).max(4) as usize;
        calc_coeff(&mut mu, radius, r2, mul);
        (level as u32, radius)
    };

    let mut coeff = [0i16; 8];

    for i in 0..radius {
        coeff[i] = (65536.0 * mu[i] + 0.5) as i32 as i16;
    }

    Method { level, radius, coeff }
}

/// A 16-bit image, a pixel's full value being 0x4000.
struct Image {
    w: usize,
    h: usize,
    stride: usize,
    data: Vec<i16>,
}

impl Image {
    fn new(w: usize, h: usize) -> Image {
        let stride = w.div_ceil(32) * 32;
        Image { w, h, stride, data: vec![0; stride * h] }
    }

    fn row(&self, y: isize) -> Option<&[i16]> {
        (y >= 0 && (y as usize) < self.h).then(|| &self.data[y as usize * self.stride..][..self.stride])
    }

    fn transpose(&self) -> Image {
        let mut t = Image::new(self.h, self.w);

        for by in (0..self.h).step_by(16) {
            for bx in (0..self.w).step_by(16) {
                for y in by..(by + 16).min(self.h) {
                    for x in bx..(bx + 16).min(self.w) {
                        t.data[x * t.stride + y] = self.data[y * self.stride + x];
                    }
                }
            }
        }

        t
    }
}

#[inline(always)]
fn load<const N: usize>(row: Option<&[i16]>, x: usize) -> Simd<i16, N> {
    match row {
        Some(r) => Simd::from_slice(&r[x..]),
        None => Simd::splat(0),
    }
}

#[inline(always)]
fn shrink_kernel<const N: usize>(src: &Image) -> Image {
    let mut dst = Image::new(src.w, (src.h + 5) >> 1);

    for (y, out) in dst.data.chunks_exact_mut(dst.stride).enumerate() {
        let n = N;
        let r = |k: isize| src.row(2 * y as isize + k);
        let (p1p, p1n, z0p, z0n, n1p, n1n) = (r(-4), r(-3), r(-2), r(-1), r(0), r(1));

        for x in (0..src.stride).step_by(n) {
            let l = |row| load::<N>(row, x).cast::<i32>();
            let (p1p, p1n, z0p, z0n, n1p, n1n) = (l(p1p), l(p1n), l(z0p), l(z0n), l(n1p), l(n1n));
            let mut v = (p1p + p1n + n1p + n1n) >> 1;
            v = (v + z0p + z0n) >> 1;
            v = (v + p1n + n1p) >> 1;
            v = (v + z0p + z0n + Simd::splat(2)) >> 2;
            v.cast::<i16>().copy_to_slice(&mut out[x..x + n]);
        }
    }

    dst
}

#[inline(always)]
fn expand_kernel<const N: usize>(src: &Image) -> Image {
    let mut dst = Image::new(src.w, 2 * src.h + 4);
    let stride = dst.stride;

    for k in 0..src.h + 2 {
        let r = |d: isize| src.row(k as isize + d);
        let (p1, z0, n1) = (r(-2), r(-1), r(0));
        let (even, odd) = dst.data[2 * k * stride..][..2 * stride].split_at_mut(stride);

        for x in (0..src.stride).step_by(N) {
            let l = |row| load::<N>(row, x).cast::<u16>();
            let (p1, z0, n1) = (l(p1), l(z0), l(n1));
            let one = Simd::splat(1);
            let r = (((p1 + n1) >> 1) + z0) >> 1;
            let rp = (((r + p1) >> 1) + z0 + one) >> 1;
            let rn = (((r + n1) >> 1) + z0 + one) >> 1;
            rp.cast::<i16>().copy_to_slice(&mut even[x..x + N]);
            rn.cast::<i16>().copy_to_slice(&mut odd[x..x + N]);
        }
    }

    dst
}

#[inline(always)]
fn blur_kernel<const N: usize>(src: &Image, m: &Method) -> Image {
    let n = m.radius;
    let mut dst = Image::new(src.w, src.h + 2 * n);

    for (y, out) in dst.data.chunks_exact_mut(dst.stride).enumerate() {
        let step = N;
        let r = |k: isize| src.row(y as isize - n as isize + k);
        let center = r(0);

        for x in (0..src.stride).step_by(step) {
            let c = load::<N>(center, x);
            let mut acc = Simd::<i32, N>::splat(0x8000);

            for i in (1..=n).rev() {
                let p = Simd::<i32, N>::splat(m.coeff[i - 1] as i32);
                let a = (load::<N>(r(-(i as isize)), x) - c).cast::<i32>();
                let b = (load::<N>(r(i as isize), x) - c).cast::<i32>();
                acc += a * p + b * p;
            }

            (c + (acc >> 16).cast::<i16>()).copy_to_slice(&mut out[x..x + step]);
        }
    }

    dst
}

dispatch!(fn shrink(src: &Image) -> Image = shrink_kernel::<16, 32>);
dispatch!(fn expand(src: &Image) -> Image = expand_kernel::<16, 32>);
dispatch!(fn blur(src: &Image, m: &Method) -> Image = blur_kernel::<16, 32>);

/// How far apart, across and down, masks can start and still blur alike: the cascade's steps,
/// and the dither's.
pub(crate) fn grid(r2x: f64, r2y: f64) -> (i32, i32) {
    let step = |r2: f64| if r2 > 0.001 { 1 << method(r2).level.max(1) } else { 2 };
    (step(r2x), step(r2y))
}

/// Gaussian blur with variances `r2x` and `r2y`; the mask grows to hold it.
pub(crate) fn gaussian(bm: &mut Bitmap, r2x: f64, r2y: f64) {
    if bm.is_empty() {
        return;
    }

    let mx = method(r2x);
    let my = if r2y == r2x { mx.clone() } else { method(r2y) };

    let mut img = Image::new(bm.w as usize, bm.h as usize);

    for y in 0..img.h {
        let src = &bm.data[y * bm.stride..][..img.w];

        for (d, &s) in img.data[y * img.stride..].iter_mut().zip(src) {
            let s = s as u16;
            *d = ((((s << 7) | (s >> 1)) + 1) >> 1) as i16;
        }
    }

    for _ in 0..my.level {
        img = shrink(&img);
    }

    img = img.transpose();

    for _ in 0..mx.level {
        img = shrink(&img);
    }

    img = blur(&img, &mx);

    for _ in 0..mx.level {
        img = expand(&img);
    }

    img = img.transpose();
    img = blur(&img, &my);

    for _ in 0..my.level {
        img = expand(&img);
    }

    let (w, h) = (img.w, img.h);
    bm.left -= ((mx.radius as i32 + 4) << mx.level) - 4;
    bm.top -= ((my.radius as i32 + 4) << my.level) - 4;
    bm.w = w as i32;
    bm.h = h as i32;
    bm.stride = stride_for(bm.w);
    bm.data = vec![0; bm.stride * h];

    for y in 0..h {
        let src = &img.data[y * img.stride..][..w];
        let dst = &mut bm.data[y * bm.stride..][..w];

        for (x, (d, &s)) in dst.iter_mut().zip(src).enumerate() {
            let dither = match (y & 1, x & 1) {
                (0, 0) => 8,
                (0, _) => 40,
                (_, 0) => 56,
                _ => 24,
            };

            let s = s.clamp(0, 0x4000) as u16;
            *d = ((s - (s >> 8) + dither) >> 6) as u8;
        }
    }
}

/// `\be`, `passes` times, on a mask already padded for it.
pub(crate) fn be(bm: &mut Bitmap, passes: i32) {
    if passes <= 0 || bm.is_empty() {
        return;
    }

    let (w, h, stride) = (bm.w as usize, bm.h as usize, bm.stride);
    let data = &mut bm.data;

    if passes > 1 {
        // Repeated passes work at 0..64, so rounding doesn't wear the mask away.
        for row in data.chunks_exact_mut(stride) {
            for p in &mut row[..w] {
                *p = ((*p >> 1) + 1) >> 1;
            }
        }

        for _ in 1..passes {
            box_blur(data, stride, w, h);
        }

        for row in data.chunks_exact_mut(stride) {
            for p in &mut row[..w] {
                *p = (((*p as u16) << 2) - (*p > 32) as u16) as u8;
            }
        }
    }

    box_blur(data, stride, w, h);
}

#[inline(always)]
fn box_sums<const N: usize>(src: Option<&[u8]>, line: &mut [u16], out: &mut [u16]) {
    let Some(src) = src else {
        out.fill(0);
        return;
    };

    for (l, &p) in line[1..].iter_mut().zip(src) {
        *l = p as u16;
    }

    for x in (0..out.len()).step_by(N) {
        let a = Simd::<u16, N>::from_slice(&line[x..]);
        let b = Simd::<u16, N>::from_slice(&line[x + 1..]);
        let c = Simd::<u16, N>::from_slice(&line[x + 2..]);
        (a + b + b + c).copy_to_slice(&mut out[x..x + N]);
    }
}

#[inline(always)]
fn box_kernel<const N: usize>(data: &mut [u8], stride: usize, w: usize, h: usize) {
    let padded = w.div_ceil(N) * N;
    let mut line = vec![0u16; padded + 2];
    let mut sums = [vec![0u16; padded], vec![0u16; padded], vec![0u16; padded]];
    let row = |data: &[u8], y: usize| (y < h).then(|| data[y * stride..][..w].to_vec());

    box_sums::<N>(row(data, 0).as_deref(), &mut line, &mut sums[1]);
    box_sums::<N>(row(data, 1).as_deref(), &mut line, &mut sums[2]);

    for y in 0..h {
        let (above, mid, below) = (&sums[0], &sums[1], &sums[2]);
        let out = &mut data[y * stride..][..w];
        let mut x = 0;

        while x + N <= w {
            let a = Simd::<u16, N>::from_slice(&above[x..]);
            let b = Simd::<u16, N>::from_slice(&mid[x..]);
            let c = Simd::<u16, N>::from_slice(&below[x..]);
            ((a + b + b + c) >> 4).cast::<u8>().copy_to_slice(&mut out[x..x + N]);
            x += N;
        }

        for x in x..w {
            out[x] = ((above[x] + 2 * mid[x] + below[x]) >> 4) as u8;
        }

        sums.rotate_left(1);
        let next = row(data, y + 2);
        box_sums::<N>(next.as_deref(), &mut line, &mut sums[2]);
    }
}

dispatch!(fn box_blur(data: &mut [u8], stride: usize, w: usize, h: usize) = box_kernel::<16, 32>);

#[cfg(test)]
mod tests {
    use super::*;

    fn dot() -> Bitmap {
        let mut b = Bitmap::new(0, 0, 9, 9);
        b.data[4 * b.stride + 4] = 255;
        b
    }

    #[test]
    fn be_spreads_a_dot() {
        let mut b = dot();
        be(&mut b, 1);
        let px = |x: usize, y: usize| b.data[y * b.stride + x];
        assert_eq!(px(4, 4), (255 * 4 / 16) as u8);
        assert_eq!(px(3, 4), (255 * 2 / 16) as u8);
        assert_eq!(px(3, 3), (255 / 16) as u8);
        assert_eq!(px(2, 2), 0);
    }

    #[test]
    fn repeated_be_keeps_full_coverage() {
        let mut b = Bitmap::new(0, 0, 16, 16);
        b.data.fill(255);
        be(&mut b, 3);
        assert_eq!(b.data[8 * b.stride + 8], 255);
    }

    #[test]
    fn gaussian_keeps_the_mass() {
        for r2 in [0.3, 4.0, 40.0, 400.0] {
            let mut b = Bitmap::new(0, 0, 32, 32);

            for y in 8..24 {
                b.data[y * b.stride + 8..y * b.stride + 24].fill(255);
            }

            let before: u32 = b.data.iter().map(|&v| v as u32).sum();
            gaussian(&mut b, r2, r2);
            let after: u32 = b.data.iter().map(|&v| v as u32).sum();
            let ratio = after as f64 / before as f64;
            assert!((ratio - 1.0).abs() < 0.02, "r2 {r2}: {ratio}");
            assert!(b.left < 0 && b.top < 0);
        }
    }
}
