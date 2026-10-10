// SPDX-License-Identifier: LGPL-3.0-or-later
//! rustyass and libass side by side: the same script and fonts rendered by both, composited and
//! compared pixel by pixel.

use std::ffi::{CString, c_char, c_int};
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use std::ptr;

use libass_src as _;
use rustyass::{Fonts, Margins, Renderer, Track};

#[repr(C)]
struct AssLibrary {
    _private: [u8; 0],
}

#[repr(C)]
struct AssRenderer {
    _private: [u8; 0],
}

#[repr(C)]
struct AssTrack {
    _private: [u8; 0],
}

#[repr(C)]
struct AssImage {
    w: c_int,
    h: c_int,
    stride: c_int,
    bitmap: *const u8,
    color: u32,
    dst_x: c_int,
    dst_y: c_int,
    next: *const AssImage,
    kind: c_int,
}

unsafe extern "C" {
    fn ass_library_init() -> *mut AssLibrary;
    fn ass_library_done(lib: *mut AssLibrary);
    fn ass_set_extract_fonts(lib: *mut AssLibrary, extract: c_int);
    fn ass_add_font(lib: *mut AssLibrary, name: *const c_char, data: *const c_char, size: c_int);
    fn ass_renderer_init(lib: *mut AssLibrary) -> *mut AssRenderer;
    fn ass_renderer_done(renderer: *mut AssRenderer);
    fn ass_set_frame_size(renderer: *mut AssRenderer, w: c_int, h: c_int);
    fn ass_set_storage_size(renderer: *mut AssRenderer, w: c_int, h: c_int);
    fn ass_set_margins(renderer: *mut AssRenderer, t: c_int, b: c_int, l: c_int, r: c_int);
    fn ass_set_use_margins(renderer: *mut AssRenderer, on: c_int);
    fn ass_set_fonts(
        renderer: *mut AssRenderer,
        default_font: *const c_char,
        default_family: *const c_char,
        provider: c_int,
        config: *const c_char,
        update: c_int,
    );
    fn ass_read_memory(lib: *mut AssLibrary, buf: *mut c_char, size: usize, codepage: *const c_char) -> *mut AssTrack;
    fn ass_free_track(track: *mut AssTrack);
    fn ass_render_frame(
        renderer: *mut AssRenderer,
        track: *mut AssTrack,
        now: i64,
        changed: *mut c_int,
    ) -> *const AssImage;
}

/// An image either renderer drew: a mask, packed, with its colour as `0xRRGGBBAA` (the alpha being
/// transparency).
#[derive(Clone, Debug)]
pub struct Img {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub color: u32,
    pub mask: Vec<u8>,
}

pub struct Libass {
    lib: *mut AssLibrary,
    renderer: *mut AssRenderer,
    track: *mut AssTrack,
}

impl Libass {
    /// libass with `fonts` added and `family` the default family; the script's own fonts are used.
    pub fn new(script: &[u8], fonts: &[Vec<u8>], family: Option<&str>) -> Option<Libass> {
        unsafe {
            let lib = ass_library_init();
            ass_set_extract_fonts(lib, 1);

            for (i, f) in fonts.iter().enumerate() {
                let name = CString::new(format!("font{i}")).unwrap();
                ass_add_font(lib, name.as_ptr(), f.as_ptr().cast(), f.len() as c_int);
            }

            let renderer = ass_renderer_init(lib);
            let family = family.map(|f| CString::new(f).unwrap());
            ass_set_fonts(
                renderer,
                ptr::null(),
                family.as_ref().map_or(ptr::null(), |f| f.as_ptr()),
                0,
                ptr::null(),
                0,
            );
            let mut buf = script.to_vec();
            let track = ass_read_memory(lib, buf.as_mut_ptr().cast(), buf.len(), ptr::null());

            if track.is_null() {
                ass_renderer_done(renderer);
                ass_library_done(lib);
                return None;
            }

            Some(Libass { lib, renderer, track })
        }
    }

    pub fn set_frame(&mut self, frame: (i32, i32), storage: (i32, i32), m: Margins) {
        unsafe {
            ass_set_frame_size(self.renderer, frame.0, frame.1);
            ass_set_storage_size(self.renderer, storage.0, storage.1);
            ass_set_margins(self.renderer, m.top, m.bottom, m.left, m.right);
            ass_set_use_margins(self.renderer, (m.top < 0 || m.bottom < 0 || m.left < 0 || m.right < 0) as c_int);
        }
    }

    pub fn render(&mut self, ms: i64) -> (Vec<Img>, bool) {
        let mut changed = 0;
        let mut img = unsafe { ass_render_frame(self.renderer, self.track, ms, &mut changed) };
        let mut out = Vec::new();

        while let Some(i) = unsafe { img.as_ref() } {
            if i.w > 0 && i.h > 0 {
                let mut mask = Vec::with_capacity((i.w * i.h) as usize);

                for y in 0..i.h {
                    let row =
                        unsafe { std::slice::from_raw_parts(i.bitmap.add((y * i.stride) as usize), i.w as usize) };
                    mask.extend_from_slice(row);
                }

                out.push(Img { x: i.dst_x, y: i.dst_y, w: i.w, h: i.h, color: i.color, mask });
            }

            img = i.next;
        }

        (out, changed != 0)
    }
}

impl Drop for Libass {
    fn drop(&mut self) {
        unsafe {
            ass_free_track(self.track);
            ass_renderer_done(self.renderer);
            ass_library_done(self.lib);
        }
    }
}

/// rustyass set up as [`Libass::new`] sets libass up.
pub struct Rusty {
    pub renderer: Renderer,
    pub track: Track,
}

impl Rusty {
    pub fn new(script: &[u8], fonts: &[Vec<u8>], family: Option<&str>) -> Option<Rusty> {
        let track = Track::parse(script)?;
        let mut f = Fonts::new();

        for font in fonts {
            f.add(font.clone());
        }

        for (_, data) in track.fonts() {
            f.add(data.to_vec());
        }

        f.set_default_family(family);
        Some(Rusty { renderer: Renderer::new(f), track })
    }

    pub fn set_frame(&mut self, frame: (i32, i32), storage: (i32, i32), m: Margins) {
        self.renderer.set_frame(frame, storage, m);
    }

    pub fn render(&mut self, ms: i64) -> (Vec<Img>, bool) {
        let r = self.renderer.render(&self.track, ms);

        let images = r
            .images
            .iter()
            .map(|i| {
                let c = i.color();
                let mut mask = Vec::with_capacity((i.w * i.h) as usize);

                for y in 0..i.h as usize {
                    mask.extend_from_slice(i.row(y));
                }

                Img {
                    x: i.x,
                    y: i.y,
                    w: i.w,
                    h: i.h,
                    color: (c.r as u32) << 24 | (c.g as u32) << 16 | (c.b as u32) << 8 | (255 - c.a) as u32,
                    mask,
                }
            })
            .collect();

        (images, r.changed)
    }
}

/// Premultiplied RGBA, 0 to 1, of the images over nothing.
pub fn composite(w: i32, h: i32, images: &[Img]) -> Vec<[f32; 4]> {
    let mut px = vec![[0f32; 4]; (w * h) as usize];

    for i in images {
        let (r, g, b) = (
            (i.color >> 24) as f32 / 255.0,
            (i.color >> 16 & 0xFF) as f32 / 255.0,
            (i.color >> 8 & 0xFF) as f32 / 255.0,
        );
        let opacity = (255 - (i.color & 0xFF)) as f32 / 255.0;

        for y in 0..i.h {
            let py = i.y + y;

            if py < 0 || py >= h {
                continue;
            }

            for x in 0..i.w {
                let pxx = i.x + x;

                if pxx < 0 || pxx >= w {
                    continue;
                }

                let a = i.mask[(y * i.w + x) as usize] as f32 / 255.0 * opacity;

                if a == 0.0 {
                    continue;
                }

                let p = &mut px[(py * w + pxx) as usize];
                let keep = 1.0 - a;
                *p = [r * a + p[0] * keep, g * a + p[1] * keep, b * a + p[2] * keep, a + p[3] * keep];
            }
        }
    }

    px
}

/// How two composites differ, per channel in 0 to 255.
#[derive(Debug, Clone, Copy, Default)]
pub struct Diff {
    pub max: f32,
    pub mean: f32,
    /// Share of the pixels either draws on that are off by more than 16 or 64.
    pub over16: f32,
    pub over64: f32,
    pub drawn: usize,
}

pub fn diff(a: &[[f32; 4]], b: &[[f32; 4]]) -> Diff {
    let (mut max, mut sum, mut drawn, mut over16, mut over64) = (0f32, 0f64, 0usize, 0usize, 0usize);

    for (p, q) in a.iter().zip(b) {
        if p[3] == 0.0 && q[3] == 0.0 {
            continue;
        }

        drawn += 1;
        let d = (0..4).map(|c| (p[c] - q[c]).abs() * 255.0).fold(0f32, f32::max);
        max = max.max(d);
        sum += d as f64;
        over16 += (d > 16.0) as usize;
        over64 += (d > 64.0) as usize;
    }

    let n = drawn.max(1) as f32;
    Diff { max, mean: (sum / drawn.max(1) as f64) as f32, over16: over16 as f32 / n, over64: over64 as f32 / n, drawn }
}

/// libass, rustyass and their difference side by side, over grey.
pub fn write_png(path: &Path, w: i32, h: i32, a: &[[f32; 4]], b: &[[f32; 4]]) {
    let mut out = vec![0u8; (w * 3 * h * 4) as usize];
    let bg = 0.25;

    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            let over = |p: [f32; 4]| [p[0] + bg * (1.0 - p[3]), p[1] + bg * (1.0 - p[3]), p[2] + bg * (1.0 - p[3])];
            let (pa, pb) = (over(a[i]), over(b[i]));
            let d = (0..3).map(|c| (pa[c] - pb[c]).abs()).fold(0f32, f32::max);

            for (k, px) in [pa, pb, [d * 4.0, d * 4.0, d * 4.0]].iter().enumerate() {
                let o = ((y * w * 3 + k as i32 * w + x) * 4) as usize;

                for c in 0..3 {
                    out[o + c] = (px[c].clamp(0.0, 1.0) * 255.0).round() as u8;
                }

                out[o + 3] = 255;
            }
        }
    }

    let file = BufWriter::new(File::create(path).unwrap());
    let mut enc = png::Encoder::new(file, (w * 3) as u32, h as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&out).unwrap();
}
