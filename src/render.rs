// SPDX-License-Identifier: LGPL-3.0-or-later
//! The renderer: a frame's events laid out, kept from overlapping, and handed out as images.

use std::collections::HashMap;
use std::sync::Arc;

use crate::bitmap::Bitmap;
use crate::event::{self, EventImages, Frame, Resources};
use crate::font::{FontCache, Fonts};
use crate::glyphs::Caches;
use crate::shape::Shaper;
use crate::track::Track;

/// How far the video is inside the frame, in pixels: negative where it's cropped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Margins {
    pub top: i32,
    pub bottom: i32,
    pub left: i32,
    pub right: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Character,
    Outline,
    Shadow,
}

/// A colour, the alpha being opacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

/// A mask to draw in one colour at `(x, y)`; its pixels are coverage, 0 to 255.
#[derive(Clone)]
pub struct Image {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub kind: ImageKind,
    color: u32,
    bitmap: Arc<Bitmap>,
    offset: usize,
}

impl Image {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        bitmap: Arc<Bitmap>,
        sx: i32,
        sy: i32,
        w: i32,
        h: i32,
        x: i32,
        y: i32,
        color: u32,
        kind: ImageKind,
    ) -> Image {
        let offset = sy as usize * bitmap.stride + sx as usize;
        Image { x, y, w, h, kind, color, bitmap, offset }
    }

    pub fn color(&self) -> Rgba {
        let c = self.color;
        Rgba { r: (c >> 24) as u8, g: (c >> 16) as u8, b: (c >> 8) as u8, a: 255 - c as u8 }
    }

    pub(crate) fn color_raw(&self) -> u32 {
        self.color
    }

    /// Bytes from one row of the mask to the next.
    pub fn stride(&self) -> usize {
        self.bitmap.stride
    }

    /// The mask, from its first pixel, rows [`Image::stride`] apart.
    pub fn mask(&self) -> &[u8] {
        &self.bitmap.data[self.offset..]
    }

    /// Row `y` of the mask, `w` pixels.
    pub fn row(&self, y: usize) -> &[u8] {
        &self.bitmap.data[self.offset + y * self.bitmap.stride..][..self.w as usize]
    }

    fn same(&self, o: &Image) -> bool {
        Arc::ptr_eq(&self.bitmap, &o.bitmap)
            && self.offset == o.offset
            && (self.w, self.h, self.color) == (o.w, o.h, o.color)
    }
}

/// What a frame shows.
pub struct Rendered<'a> {
    pub images: &'a [Image],
    /// Whether that's different from the last frame rendered.
    pub changed: bool,
}

#[derive(Clone, Copy, Default)]
struct Placed {
    top: i32,
    height: i32,
    left: i32,
    width: i32,
}

/// Renders tracks into frames of a given size.
pub struct Renderer {
    fonts: Fonts,
    generation: u64,
    frame: (i32, i32),
    storage: (i32, i32),
    margins: Margins,
    content: (f64, f64),
    fit: (f64, f64),
    res: Resources,
    placed: HashMap<usize, Placed>,
    track: usize,
    images: Vec<Image>,
}

impl Renderer {
    pub fn new(fonts: Fonts) -> Renderer {
        Renderer {
            generation: fonts.generation,
            fonts,
            frame: (0, 0),
            storage: (0, 0),
            margins: Margins::default(),
            content: (0.0, 0.0),
            fit: (0.0, 0.0),
            res: Resources { fonts: FontCache::default(), shaper: Shaper::new(), caches: Caches::new() },
            placed: HashMap::new(),
            track: 0,
            images: Vec::new(),
        }
    }

    pub fn fonts(&self) -> &Fonts {
        &self.fonts
    }

    pub fn fonts_mut(&mut self) -> &mut Fonts {
        &mut self.fonts
    }

    /// Lays frames out: `frame` is what's drawn into, `storage` the video's own size (what
    /// scripts' borders and blurs are relative to), and `margins` how far the video is inside the
    /// frame. With negative margins, where the video is cropped, dialogue is kept within what
    /// shows.
    pub fn set_frame(&mut self, frame: (i32, i32), storage: (i32, i32), margins: Margins) {
        let valid = |(w, h): (i32, i32)| if w > 0 && h > 0 { (w, h) } else { (0, 0) };
        let (frame, storage) = (valid(frame), valid(storage));

        if (frame, storage, margins) == (self.frame, self.storage, self.margins) {
            return;
        }

        self.frame = frame;
        self.storage = storage;
        self.margins = margins;
        self.res.caches.clear_bitmaps();
        self.placed.clear();

        let (w, h) = (frame.0 as f64, frame.1 as f64);
        let cw = (frame.0 - margins.left - margins.right) as f64;
        let ch = (frame.1 - margins.top - margins.bottom) as f64;
        self.content = (cw, ch);
        self.fit = (if cw * h >= ch * w { w } else { cw * h / ch }, if cw * h <= ch * w { h } else { ch * w / cw });
    }

    fn layout_res(&self, track: &Track) -> (i32, i32) {
        if track.layout_res.0 > 0 && track.layout_res.1 > 0 {
            track.layout_res
        } else if self.storage.0 > 0 && self.storage.1 > 0 {
            self.storage
        } else {
            track.play_res
        }
    }

    /// The images showing at `ms`.
    pub fn render(&mut self, track: &Track, ms: i64) -> Rendered<'_> {
        if self.fonts.generation != self.generation {
            self.generation = self.fonts.generation;
            self.res.fonts.clear();
            self.res.caches.clear();
        }

        let id = track as *const Track as usize;

        if id != self.track {
            self.track = id;
            self.placed.clear();
        }

        if self.frame.0 == 0 || track.events.is_empty() || self.fonts.is_empty() {
            let changed = !self.images.is_empty();
            self.images.clear();
            return Rendered { images: &self.images, changed };
        }

        self.res.caches.start_frame();
        let layout_res = self.layout_res(track);
        let lr_track = track.layout_res.0 > 0 && track.layout_res.1 > 0;

        let par = if self.content.0 > 0.0 && self.content.1 > 0.0 && (lr_track || self.storage.0 > 0) {
            (self.content.0 / self.content.1) / (layout_res.0 as f64 / layout_res.1 as f64)
        } else {
            1.0
        };

        let m = self.margins;

        let f = Frame {
            track,
            fonts: &self.fonts,
            time: ms,
            width: self.frame.0,
            height: self.frame.1,
            content: self.content,
            fit: self.fit,
            margins: (m.top, m.bottom, m.left, m.right),
            use_margins: m.top < 0 || m.bottom < 0 || m.left < 0 || m.right < 0,
            par,
            layout_res,
        };

        let mut events: Vec<EventImages> = Vec::new();

        for e in &track.events {
            if e.start <= ms && ms < e.start + e.duration {
                if let Some(ei) = event::render(&f, &mut self.res, e) {
                    events.push(ei);
                }
            }
        }

        events.sort_by_key(|e| (e.layer, e.read_order));
        let mut start = 0;

        for i in 1..=events.len() {
            if i == events.len() || events[i].layer != events[start].layer {
                fix_collisions(&mut self.placed, &mut events[start..i], self.frame.1);
                start = i;
            }
        }

        let images: Vec<Image> = events.into_iter().flat_map(|e| e.images).filter(|i| i.w > 0 && i.h > 0).collect();

        let changed = images.len() != self.images.len()
            || images.iter().zip(&self.images).any(|(a, b)| !a.same(b) || (a.x, a.y) != (b.x, b.y));

        self.images = images;
        Rendered { images: &self.images, changed }
    }
}

fn overlap(a: &Placed, b: &Placed) -> bool {
    !(a.top >= b.top + b.height
        || b.top >= a.top + a.height
        || a.left >= b.left + b.width
        || b.left >= a.left + a.width)
}

fn shift_event(ei: &mut EventImages, shift: i32, height: i32) {
    for img in &mut ei.images {
        img.y += shift;

        if img.y < 0 {
            let clip = -img.y;
            img.h -= clip;
            img.offset += clip.max(0) as usize * img.bitmap.stride;
            img.y = 0;
        }

        if img.y + img.h >= height {
            img.h -= img.y + img.h - height;
        }

        if img.h <= 0 {
            img.h = 0;
            img.y = 0;
        }
    }

    ei.top += shift;
}

/// Keeps events of a layer that would overlap apart, as VSFilter does: those already placed stay
/// where they are, and new ones move up (bottom-aligned) or down until they're clear.
fn fix_collisions(placed: &mut HashMap<usize, Placed>, events: &mut [EventImages], height: i32) {
    let mut used: Vec<Placed> = Vec::new();
    let collides = |e: &EventImages| e.detect_collisions && e.height != 0 && e.width != 0;

    for e in events.iter_mut() {
        if !collides(e) {
            continue;
        }

        let p = placed.entry(e.read_order).or_default();

        if p.height > 0 {
            if p.height != e.height || used.iter().any(|u| overlap(p, u)) {
                *p = Placed::default();
                continue;
            }

            used.push(*p);
            let shift = p.top - e.top;
            shift_event(e, shift, height);
        }
    }

    used.sort_by_key(|u| u.top);

    for e in events.iter_mut() {
        if !collides(e) {
            continue;
        }

        let p = placed.entry(e.read_order).or_default();

        if p.height != 0 {
            continue;
        }

        let s = Placed { top: e.top, height: e.height, left: e.left, width: e.width };
        let mut shift = 0;

        if e.shift_direction == 1 {
            for u in &used {
                if s.top + s.height + shift <= u.top
                    || s.top + shift >= u.top + u.height
                    || s.left + s.width <= u.left
                    || s.left >= u.left + u.width
                {
                    continue;
                }

                shift = u.top + u.height - s.top;
            }
        } else {
            for u in used.iter().rev() {
                if s.top + s.height + shift <= u.top
                    || s.top + shift >= u.top + u.height
                    || s.left + s.width <= u.left
                    || s.left >= u.left + u.width
                {
                    continue;
                }

                shift = u.top - (s.top + s.height);
            }
        }

        used.push(Placed { top: s.top + shift, ..s });
        used.sort_by_key(|u| u.top);

        if shift != 0 {
            shift_event(e, shift, height);
        }

        *placed.entry(e.read_order).or_default() =
            Placed { top: e.top, height: e.height, left: e.left, width: e.width };
    }
}
