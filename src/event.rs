// SPDX-License-Identifier: LGPL-3.0-or-later
//! Laying an event out: its characters, shaped, wrapped into lines, aligned and positioned, then
//! drawn into masks per run of like-styled text.

use std::sync::Arc;

use harfrust::Script;

use crate::bitmap::Bitmap;
use crate::font::{DECO_ROTATE, FontCache, Fonts, OUTLINE_SIZE};
use crate::glyphs::{self, Caches, Composite, Filter, OutlineEntry};
use crate::outline::{Pt, Rect, pt};
use crate::shape::{self, Run, Shaper};
use crate::state::{EVENT_HSCROLL, EVENT_POSITIONED, EVENT_VSCROLL, Effect, Scroll, State, apply_fade};
use crate::track::{Event, HALIGN_CENTER, HALIGN_LEFT, HALIGN_RIGHT, Track, VALIGN_CENTER, VALIGN_SUB, VALIGN_TOP};
use crate::{Image, ImageKind};

/// Everything about the frame being rendered that events are laid out against.
pub(crate) struct Frame<'a> {
    pub track: &'a Track,
    pub fonts: &'a Fonts,
    pub time: i64,
    pub width: i32,
    pub height: i32,
    /// The video's part of the frame, and how big it would be letterboxed into the frame.
    pub content: (f64, f64),
    pub fit: (f64, f64),
    /// Top, bottom, left, right.
    pub margins: (i32, i32, i32, i32),
    pub use_margins: bool,
    pub par: f64,
    pub layout_res: (i32, i32),
}

impl Frame<'_> {
    fn play_res(&self) -> (f64, f64) {
        (self.track.play_res.0 as f64, self.track.play_res.1 as f64)
    }

    pub fn x2scr_pos(&self, x: f64) -> f64 {
        x * self.content.0 / self.par / self.play_res().0 + self.margins.2 as f64
    }

    fn x2scr_left(&self, s: &State, x: f64) -> f64 {
        if s.explicit || !self.use_margins {
            return self.x2scr_pos(x);
        }

        x * self.fit.0 / self.par / self.play_res().0
    }

    fn x2scr_right(&self, s: &State, x: f64) -> f64 {
        if s.explicit || !self.use_margins {
            return self.x2scr_pos(x);
        }

        x * self.fit.0 / self.par / self.play_res().0 + (self.width as f64 - self.fit.0)
    }

    pub fn x2scr_pos_scaled(&self, x: f64) -> f64 {
        x * self.content.0 / self.play_res().0 + self.margins.2 as f64
    }

    pub fn y2scr_pos(&self, y: f64) -> f64 {
        y * self.content.1 / self.play_res().1 + self.margins.0 as f64
    }

    fn y2scr(&self, s: &State, y: f64) -> f64 {
        if s.explicit || !self.use_margins {
            return self.y2scr_pos(y);
        }

        y * self.fit.1 / self.play_res().1 + (self.height as f64 - self.fit.1) * 0.5
    }

    fn y2scr_top(&self, s: &State, y: f64) -> f64 {
        if s.explicit || !self.use_margins {
            return self.y2scr_pos(y);
        }

        y * self.fit.1 / self.play_res().1
    }

    fn y2scr_sub(&self, s: &State, y: f64) -> f64 {
        if s.explicit || !self.use_margins {
            return self.y2scr_pos(y);
        }

        y * self.fit.1 / self.play_res().1 + (self.height as f64 - self.fit.1)
    }
}

/// What a frame's events share: fonts, the shaper and the caches.
pub(crate) struct Resources {
    pub fonts: FontCache,
    pub shaper: Shaper,
    pub caches: Caches,
}

/// A drawing's commands and how it's scaled.
#[derive(Clone, Debug)]
pub(crate) struct Drawing {
    pub text: String,
    pub scale: i32,
    pub pbo: f64,
}

/// One glyph of a character's cluster (the first is the character's own).
#[derive(Clone, Debug, Default)]
pub(crate) struct Part {
    pub glyph: u32,
    pub offset: Pt,
    pub advance: Pt,
    pub pos: Pt,
    pub outline: Option<Arc<OutlineEntry>>,
    /// The outline's transform before positioning: a scale, then an offset.
    pub scale: Pt,
    pub tr_offset: Pt,
    pub bbox: Rect,
    pub asc: f64,
    pub desc: f64,
    pub shift: Pt,
}

/// A character, with the style it's drawn in.
#[derive(Clone, Debug)]
pub(crate) struct Glyph {
    pub symbol: u32,
    pub skip: bool,
    pub trimmed: bool,
    /// 1 for a wrap, 2 for a forced line break, before this character.
    pub linebreak: u8,
    pub starts_new_run: bool,
    pub font: usize,
    pub face: usize,
    pub script: Option<Script>,
    pub font_size: f64,
    pub drawing: Option<Drawing>,
    pub c: [u32; 4],
    pub a_pre_fade: [u8; 4],
    pub effect: Effect,
    pub effect_timing: i32,
    pub effect_skip_timing: i32,
    pub reset_effect: bool,
    /// Karaoke: how far right of the glyph its fill has got.
    pub karaoke: f64,
    pub be: i32,
    pub blur: f64,
    pub shadow: Pt,
    pub frx: f64,
    pub fry: f64,
    pub frz: f64,
    pub fax: f64,
    pub fay: f64,
    pub scale_x: f64,
    pub scale_y: f64,
    pub scale_fix: f64,
    pub border_style: i32,
    pub border: Pt,
    pub hspacing: f64,
    pub hspacing_scaled: f64,
    pub italic: i32,
    pub bold: i32,
    pub flags: u32,
    pub fade: i32,
    pub shape_run: usize,
    pub cluster_advance: Pt,
    pub parts: Vec<Part>,
}

impl Glyph {
    fn root(&self) -> &Part {
        &self.parts[0]
    }
}

#[derive(Clone, Copy, Default, Debug)]
struct Line {
    asc: f64,
    desc: f64,
    offset: usize,
    len: usize,
}

/// An event's images, with the box collision handling moves.
pub(crate) struct EventImages {
    pub images: Vec<Image>,
    pub top: i32,
    pub height: i32,
    pub left: i32,
    pub width: i32,
    pub detect_collisions: bool,
    pub shift_direction: i32,
    pub layer: i32,
    pub read_order: usize,
}

/// A run of glyphs drawn as one: their masks added together, blurred and shadowed as one.
struct Combined {
    filter: Filter,
    c: [u32; 4],
    effect: Effect,
    karaoke: f64,
    /// Where the karaoke fill has got to on screen.
    effect_timing: i32,
    leftmost_x: f64,
    refs: Vec<glyphs::BitmapRef>,
    x: i32,
    y: i32,
    image: Option<Arc<Composite>>,
}

struct Layout<'a> {
    f: &'a Frame<'a>,
    s: State,
    event: &'a Event,
    glyphs: Vec<Glyph>,
    lines: Vec<Line>,
    height: f64,
    border_top: i32,
    border_bottom: i32,
    border_x: i32,
    levels: Vec<u8>,
    base_levels: Vec<u8>,
    cmap: Vec<usize>,
    whole_text: bool,
}

pub(crate) fn render(f: &Frame, r: &mut Resources, event: &Event) -> Option<EventImages> {
    if event.style >= f.track.styles.len() {
        return None;
    }

    let s = State::new(f, &mut r.fonts, event);

    let mut l = Layout {
        f,
        s,
        event,
        glyphs: Vec::new(),
        lines: Vec::new(),
        height: 0.0,
        border_top: 0,
        border_bottom: 0,
        border_x: 0,
        levels: Vec::new(),
        base_levels: Vec::new(),
        cmap: Vec::new(),
        whole_text: false,
    };

    if !l.parse(r) || l.glyphs.is_empty() {
        return None;
    }

    l.split_style_runs();
    l.find_runs(r);
    l.shape(r);
    l.retrieve_glyphs(r);
    l.preliminary_layout();

    let valign = l.s.alignment & 12;
    let style = &l.s.style;
    let margin_l = if event.margin_l != 0 { event.margin_l } else { style.margin_l } as f64;
    let margin_r = if event.margin_r != 0 { event.margin_r } else { style.margin_r } as f64;
    let margin_v = if event.margin_v != 0 { event.margin_v } else { style.margin_v } as f64;

    let max_text_width = f.x2scr_right(&l.s, f.track.play_res.0 as f64 - margin_r) - f.x2scr_left(&l.s, margin_l);

    l.wrap_lines(max_text_width);
    l.karaoke();
    l.reorder();
    l.align_lines(max_text_width);
    let bbox = l.string_bbox();
    l.baseline_shear();

    let mut device = pt(0.0, 0.0);

    if l.s.evt_type & EVENT_POSITIONED != 0 {
        let (bx, by) = base_point(&bbox, l.s.alignment);
        device = pt(f.x2scr_pos(l.s.pos.0) - bx, f.y2scr_pos(l.s.pos.1) - by);
    }

    if l.s.evt_type & EVENT_HSCROLL != 0 {
        match l.s.scroll {
            Scroll::RightToLeft => device.x = f.x2scr_pos(f.track.play_res.0 as f64 - l.s.scroll_shift),
            Scroll::LeftToRight => device.x = f.x2scr_pos(l.s.scroll_shift) - (bbox.x1 - bbox.x0),
            _ => {},
        }
    } else if l.s.evt_type & EVENT_POSITIONED == 0 {
        device.x = f.x2scr_left(&l.s, margin_l);
    }

    if l.s.evt_type & EVENT_VSCROLL != 0 {
        match l.s.scroll {
            Scroll::TopToBottom => {
                device.y = f.y2scr(&l.s, l.s.scroll_y0 as f64 + l.s.scroll_shift) - bbox.y1;
            },
            Scroll::BottomToTop => {
                device.y = f.y2scr(&l.s, l.s.scroll_y1 as f64 - l.s.scroll_shift) - bbox.y0;
            },
            _ => {},
        }
    } else if l.s.evt_type & EVENT_POSITIONED == 0 {
        if valign == VALIGN_TOP {
            device.y = f.y2scr_top(&l.s, margin_v) + l.lines[0].asc;
        } else if valign == VALIGN_CENTER {
            let scr_y = f.y2scr(&l.s, f.track.play_res.1 as f64 / 2.0);
            device.y = scr_y - (bbox.y1 + bbox.y0) / 2.0;
        } else {
            let scr_bottom = f.y2scr_sub(&l.s, f.track.play_res.1 as f64 - margin_v);
            device.y = scr_bottom - l.height + l.lines[0].asc;
        }
    }

    // Clip coordinates.
    let mut clip = [0i32; 4];

    if l.s.explicit || !f.use_margins {
        clip[0] = f.x2scr_pos_scaled(l.s.clip[0] as f64).round() as i32;
        clip[2] = f.x2scr_pos_scaled(l.s.clip[2] as f64).round() as i32;
        clip[1] = f.y2scr_pos(l.s.clip[1] as f64).round() as i32;
        clip[3] = f.y2scr_pos(l.s.clip[3] as f64).round() as i32;

        if l.s.explicit {
            // Still within the video.
            let (zx, zy) = (f.margins.2, f.margins.0);
            let (sx, sy) = (zx + f.content.0 as i32, zy + f.content.1 as i32);
            clip[0] = clip[0].max(zx);
            clip[1] = clip[1].max(zy);
            clip[2] = clip[2].min(sx);
            clip[3] = clip[3].min(sy);
        }
    } else {
        clip = [0, 0, f.width, f.height];
    }

    if l.s.evt_type & EVENT_VSCROLL != 0 {
        let y0 = f.y2scr_pos(l.s.scroll_y0 as f64).round() as i32;
        let y1 = f.y2scr_pos(l.s.scroll_y1 as f64).round() as i32;
        clip[1] = clip[1].max(y0);
        clip[3] = clip[3].min(y1);
    }

    l.rotation_params(&bbox, device);
    let mut combined = l.render_and_combine(r, device);

    let top = (device.y - l.lines[0].asc - l.border_top as f64) as i32;
    let height = (l.height + (l.border_bottom + l.border_top) as f64) as i32;
    let left = ((device.x + bbox.x0) * f.par - l.border_x as f64 + 0.5) as i32;
    let width = ((bbox.x1 - bbox.x0) * f.par + 2.0 * l.border_x as f64 + 0.5) as i32;

    let mut images = l.render_text(r, &mut combined, clip);

    if l.s.border_style == 4 {
        l.add_background(&mut images, left, top, width, height);
    }

    Some(EventImages {
        images,
        top,
        height,
        left,
        width,
        detect_collisions: l.s.detect_collisions,
        shift_direction: if valign == VALIGN_SUB { -1 } else { 1 },
        layer: event.layer,
        read_order: event.read_order,
    })
}

fn base_point(bbox: &Rect, alignment: i32) -> (f64, f64) {
    let bx = match alignment & 3 {
        HALIGN_LEFT => bbox.x0,
        HALIGN_CENTER => (bbox.x1 + bbox.x0) / 2.0,
        HALIGN_RIGHT => bbox.x1,
        _ => 0.0,
    };

    let by = match alignment & 12 {
        VALIGN_TOP => bbox.y0,
        VALIGN_CENTER => (bbox.y1 + bbox.y0) / 2.0,
        VALIGN_SUB => bbox.y1,
        _ => 0.0,
    };

    (bx, by)
}

pub(crate) const FILTER_BORDER_STYLE_3: u32 = 1;
pub(crate) const FILTER_NONZERO_BORDER: u32 = 2;
pub(crate) const FILTER_NONZERO_SHADOW: u32 = 4;
pub(crate) const FILTER_FILL_IN_SHADOW: u32 = 8;
pub(crate) const FILTER_FILL_IN_BORDER: u32 = 16;

impl Layout<'_> {
    /// Reads the text into glyphs, applying tags as they come.
    fn parse(&mut self, r: &mut Resources) -> bool {
        let text = self.event.text.as_str();
        let b = text.as_bytes();
        let mut p = 0;

        loop {
            let mut drawing = None;
            let mut code = 0;

            while p < b.len() {
                if b[p] == b'{'
                    && let Some(end) = text[p..].find('}')
                {
                    let end = p + end;
                    let f = self.f;
                    self.s.parse_tags(f, &mut r.fonts, self.event, &text[p + 1..end], 1.0, false);
                    p = end + 1;
                } else if self.s.drawing_scale != 0 {
                    let mut q = p;

                    if b[p] == b'{' {
                        q += 1;
                    }

                    while q < b.len() && b[q] != b'{' {
                        q += 1;
                    }

                    drawing =
                        Some(Drawing { text: text[p..q].to_string(), scale: self.s.drawing_scale, pbo: self.s.pbo });
                    code = 0xFFFC;
                    p = q;
                    break;
                } else {
                    code = self.s.next_char(text, &mut p);
                    break;
                }
            }

            if code == 0 {
                break;
            }

            let Some(font) = self.s.font else { return false };
            let s = &self.s;
            let mut c = s.c;
            let mut a_pre_fade = [0u8; 4];

            for (i, c) in c.iter_mut().enumerate() {
                a_pre_fade[i] = (*c & 0xFF) as u8;
                apply_fade(c, s.fade);
            }

            let vertical = r.fonts.font(font).desc.vertical;
            let mut flags = s.flags;

            if vertical && code >= 0x02F1 {
                flags |= DECO_ROTATE;
            }

            let mut g = Glyph {
                symbol: code,
                skip: false,
                trimmed: false,
                linebreak: 0,
                starts_new_run: false,
                font,
                face: 0,
                script: None,
                // VSFilter sizes glyphs by PlayResY in both directions.
                font_size: (s.font_size * s.screen_scale.1).abs(),
                drawing,
                c,
                a_pre_fade,
                effect: s.effect,
                effect_timing: s.effect_timing,
                effect_skip_timing: s.effect_skip_timing,
                reset_effect: s.reset_effect,
                karaoke: 0.0,
                be: s.be,
                blur: s.blur,
                shadow: pt(s.shadow_x, s.shadow_y),
                frx: s.frx,
                fry: s.fry,
                frz: s.frz,
                fax: s.fax,
                fay: s.fay,
                scale_x: s.scale_x,
                scale_y: s.scale_y,
                scale_fix: 1.0,
                border_style: s.border_style,
                border: pt(s.border_x, s.border_y),
                hspacing: s.hspacing,
                hspacing_scaled: 0.0,
                italic: s.italic,
                bold: s.bold,
                flags,
                fade: s.fade,
                shape_run: 0,
                cluster_advance: pt(0.0, 0.0),
                parts: vec![Part::default()],
            };

            if g.drawing.is_none() {
                g.hspacing_scaled = g.hspacing * s.screen_scale.0 / self.f.par * g.scale_x;

                // Outlines come at a fixed size and are scaled from there.
                if g.font_size != 0.0 {
                    let mul = g.font_size / OUTLINE_SIZE;
                    g.scale_fix = 1.0 / mul;
                    g.scale_x *= mul;
                    g.scale_y *= mul;
                    g.font_size = OUTLINE_SIZE;
                }
            }

            self.glyphs.push(g);
            self.s.effect = Effect::None;
            self.s.effect_timing = 0;
            self.s.effect_skip_timing = 0;
            self.s.reset_effect = false;
        }

        true
    }

    fn split_style_runs(&mut self) {
        let g = &mut self.glyphs;
        let mut last_effect = g[0].effect;
        g[0].starts_new_run = true;

        for i in 1..g.len() {
            let (a, b) = (&g[i - 1], &g[i]);

            let new = b.effect_timing != 0
                || (b.effect != Effect::None && b.effect != last_effect)
                || b.drawing.is_some()
                || a.drawing.is_some()
                || a.font != b.font
                || a.font_size != b.font_size
                || a.c != b.c
                || a.be != b.be
                || a.blur != b.blur
                || a.shadow != b.shadow
                || a.frx != b.frx
                || a.fry != b.fry
                || a.frz != b.frz
                || a.fax != b.fax
                || a.fay != b.fay
                || a.scale_x != b.scale_x
                || a.scale_y != b.scale_y
                || a.border_style != b.border_style
                || a.border != b.border
                || a.hspacing != b.hspacing
                || a.italic != b.italic
                || a.bold != b.bold
                || (a.flags ^ b.flags) & !DECO_ROTATE != 0;

            if b.effect != Effect::None {
                last_effect = b.effect;
            }

            g[i].starts_new_run = new;
        }
    }

    /// Scripts, faces, and the runs that get shaped together.
    fn find_runs(&mut self, r: &mut Resources) {
        let n = self.glyphs.len();
        let mut last = None;
        let mut backwards = false;

        for g in &mut self.glyphs {
            g.script = r.shaper.script(g.symbol);

            match g.script {
                None if last.is_some() => g.script = last,
                None => backwards = true,
                s => last = s,
            }
        }

        if backwards {
            let mut last = None;

            for g in self.glyphs.iter_mut().rev() {
                match g.script {
                    None => g.script = last,
                    s => last = s,
                }
            }
        }

        for g in &mut self.glyphs {
            if ignorable(g.symbol) {
                g.skip = true;
            }
        }

        let mut run = 0;

        for i in 0..n {
            if self.glyphs[i].drawing.is_none() && !self.glyphs[i].skip {
                let (face, glyph) = r.fonts.glyph(self.f.fonts, self.glyphs[i].font, self.glyphs[i].symbol);
                self.glyphs[i].face = face;
                self.glyphs[i].parts[0].glyph = glyph;
            }

            if i > 0 {
                let (a, b) = (&self.glyphs[i - 1], &self.glyphs[i]);

                if a.font != b.font
                    || (!b.skip && a.face != b.face)
                    || a.script != b.script
                    || b.starts_new_run
                    || (!self.whole_text && b.hspacing != 0.0)
                    || a.flags != b.flags
                {
                    run += 1;
                } else if b.skip {
                    self.glyphs[i].face = self.glyphs[i - 1].face;
                }
            }

            self.glyphs[i].shape_run = run;
        }
    }

    /// Bidi levels, then each run through HarfRust.
    fn shape(&mut self, r: &mut Resources) {
        let n = self.glyphs.len();
        let auto = self.s.font_encoding == -1;
        self.whole_text = auto;
        let text: Vec<u32> = self.glyphs.iter().map(|g| g.symbol).collect();
        self.levels = vec![0; n];
        self.base_levels = vec![0; n];
        let mut last = 0;

        for i in 0..n {
            let brk = i == n - 1
                || shape::is_paragraph_separator(text[i])
                || (!self.whole_text && (self.glyphs[i + 1].starts_new_run || self.glyphs[i].hspacing != 0.0));

            if brk {
                let (levels, base) = shape::levels(&text[last..=i], auto);
                self.levels[last..=i].copy_from_slice(&levels);
                self.base_levels[last..=i].fill(base);
                last = i + 1;
            }
        }

        for g in &mut self.glyphs {
            g.skip = true;
        }

        let kerning = self.f.track.kerning;
        let language = self.f.track.language.clone();
        let mut i = 0;

        while i < n {
            if self.glyphs[i].drawing.is_some() {
                self.glyphs[i].skip = false;
                i += 1;
                continue;
            }

            let start = i;
            let (run_id, level) = (self.glyphs[i].shape_run, self.levels[i]);

            while i + 1 < n && self.glyphs[i + 1].shape_run == run_id && self.levels[i + 1] == level {
                i += 1;
            }

            let end = i + 1;
            let lead = start > 0 && !self.glyphs[start].starts_new_run && control(text[start - 1]);
            let trail = end < n && !self.glyphs[end].starts_new_run && control(text[end]);
            let g0 = &self.glyphs[start];
            let face = r.fonts.font(g0.font).faces.get(g0.face).cloned();

            if let Some(face) = face {
                let run = Run {
                    text: &text,
                    range: start..end,
                    rtl: level % 2 == 1,
                    script: g0.script,
                    language: language.as_deref(),
                    kerning,
                    ligatures: g0.hspacing == 0.0,
                    vertical: r.fonts.font(g0.font).desc.vertical,
                    context: (lead, trail),
                };

                for s in r.shaper.shape(&face, &run) {
                    let idx = start + s.cluster;
                    let g = &mut self.glyphs[idx];
                    let (sx, sy) = (g.scale_x, g.scale_y);

                    let part = Part {
                        glyph: s.glyph,
                        offset: pt(s.x_offset as f64 / 64.0 * sx, -s.y_offset as f64 / 64.0 * sy),
                        advance: pt(s.x_advance as f64 / 64.0 * sx, -s.y_advance as f64 / 64.0 * sy),
                        ..Part::default()
                    };

                    g.cluster_advance = g.cluster_advance + part.advance;

                    if g.skip {
                        g.parts[0] = part;
                        g.skip = false;
                    } else {
                        g.parts.push(part);
                    }
                }
            }

            i += 1;
        }
    }

    /// Outlines and metrics.
    fn retrieve_glyphs(&mut self, r: &mut Resources) {
        for i in 0..self.glyphs.len() {
            let f = self.f;
            let s = &self.s;
            glyphs::outlines(f, s, r, &mut self.glyphs[i]);

            // Italic text's overhang counts when it's followed by upright text.
            if i > 0 && self.glyphs[i - 1].italic != 0 && self.glyphs[i].italic == 0 {
                let mut back = i - 1;

                while back > 0
                    && self.glyphs[back].root().bbox.x1 - self.glyphs[back].root().bbox.x0 == 0.0
                    && self.glyphs[back].italic != 0
                {
                    back -= 1;
                }

                let og = &mut self.glyphs[back];

                if og.root().bbox.x1 > og.cluster_advance.x {
                    og.cluster_advance.x = og.root().bbox.x1;
                }
            }

            let g = &mut self.glyphs[i];
            g.cluster_advance.x += g.hspacing_scaled;
        }
    }

    fn preliminary_layout(&mut self) {
        let mut pen = pt(0.0, 0.0);

        for g in &mut self.glyphs {
            let mut cluster_pen = pen;

            for part in &mut g.parts {
                part.pos = cluster_pen;
                cluster_pen = cluster_pen + part.advance;
            }

            pen = pen + g.cluster_advance;
        }
    }

    fn wrap_lines(&mut self, max_width: f64) {
        self.wrap_naive(max_width);
        self.wrap_rebalance();
        self.trim_whitespace();
        self.measure_text();
        self.wrap_measure();
    }

    /// Breaks at the last space before the line gets too long.
    fn wrap_naive(&mut self, max_width: f64) {
        let n = self.glyphs.len();
        let mut s1 = 0;
        let mut last_breakable: Option<usize> = None;
        let mut lines = 1;

        for i in 0..n {
            let g = &self.glyphs[i];
            let start = self.glyphs[s1].root();
            let s_offset = start.bbox.x0 + start.pos.x;
            let len = g.root().bbox.x1 + g.root().pos.x - s_offset;
            let mut break_at = None;
            let mut kind = 0;

            if g.symbol == '\n' as u32 {
                kind = 2;
                break_at = Some(i);
            } else if len >= max_width && g.symbol != ' ' as u32 && self.s.wrap_style != 2 {
                kind = 1;
                break_at = last_breakable;
            }

            if g.symbol == ' ' as u32 {
                last_breakable = Some(i);
            }

            if let Some(b) = break_at {
                let lead = b + 1;

                if lead < n {
                    self.glyphs[lead].linebreak = kind;
                    last_breakable = None;
                    s1 = lead;
                    lines += 1;
                }
            }
        }

        self.lines = vec![Line::default(); lines];
    }

    /// Moves words down a line while that evens the lines' lengths.
    fn wrap_rebalance(&mut self) {
        if self.s.wrap_style == 1 {
            return;
        }

        let n = self.glyphs.len();
        let x_max = |g: &Glyph| g.root().bbox.x1 + g.root().pos.x;
        let x_min = |g: &Glyph| g.root().bbox.x0 + g.root().pos.x;
        let rewind = |glyphs: &[Glyph], start1: usize, start2: usize| {
            let mut g = start2;

            loop {
                g -= 1;

                if !(g > start1 && glyphs[g].symbol == ' ' as u32) {
                    break;
                }
            }

            g
        };

        loop {
            let mut changed = false;
            let (mut s2, mut s3): (Option<usize>, usize) = (None, 0);

            for i in 0..=n {
                if i == n || self.glyphs[i].linebreak != 0 {
                    let s1 = s2;
                    s2 = Some(s3);
                    s3 = i;

                    if let (Some(a), Some(b)) = (s1, s2)
                        && self.glyphs[b].linebreak == 1
                    {
                        let g = &self.glyphs;
                        let mut w = rewind(g, a, b);
                        let e1_old = w;

                        while w > a && g[w].symbol != ' ' as u32 {
                            w -= 1;
                        }

                        let mut e1 = w;

                        while e1 > a && g[e1].symbol == ' ' as u32 {
                            e1 -= 1;
                        }

                        if g[w].symbol == ' ' as u32 {
                            w += 1;
                        }

                        if w == a {
                            continue;
                        }

                        let e2 = rewind(g, b, s3);
                        let l1 = x_max(&g[e1_old]) - x_min(&g[a]);
                        let l2 = x_max(&g[e2]) - x_min(&g[b]);
                        let l1_new = x_max(&g[e1]) - x_min(&g[a]);
                        let l2_new = x_max(&g[e2]) - x_min(&g[w]);

                        if (l1_new - l2_new).abs() < (l1 - l2).abs() {
                            self.glyphs[w].linebreak = 1;
                            self.glyphs[b].linebreak = 0;
                            s2 = Some(w);
                            changed = true;
                        }
                    }
                }
            }

            if !changed {
                break;
            }
        }
    }

    fn trim_whitespace(&mut self) {
        let n = self.glyphs.len();
        let ws = |g: &Glyph| (g.symbol == ' ' as u32 || g.symbol == '\n' as u32) && g.linebreak == 0;
        let g = &mut self.glyphs;

        let mut i = n - 1;

        while i > 0 && ws(&g[i]) {
            g[i].skip = true;
            g[i].trimmed = true;
            i -= 1;
        }

        let mut i = 0;

        while i < n && ws(&g[i]) {
            g[i].skip = true;
            g[i].trimmed = true;
            i += 1;
        }

        if i < n {
            g[i].starts_new_run = true;
        }

        let mut i = 0;

        while i < n {
            if g[i].linebreak != 0 {
                if i > 0 {
                    let mut j = i - 1;

                    while j > 0 && ws(&g[j]) {
                        g[j].skip = true;
                        g[j].trimmed = true;
                        j -= 1;
                    }
                }

                let mut cur = i;

                if g[i].symbol == ' ' as u32 || g[i].symbol == '\n' as u32 {
                    g[i].skip = true;
                    g[i].trimmed = true;
                    let mut j = i + 1;

                    while j < n && ws(&g[j]) {
                        g[j].skip = true;
                        g[j].trimmed = true;
                        j += 1;
                    }

                    i = j - 1;
                    cur = j;
                }

                if cur < n {
                    g[cur].starts_new_run = true;
                }
            }

            i += 1;
        }
    }

    fn end_of_line(&mut self, scale: f64, line: usize, asc: f64, desc: f64, bx: f64, by: f64) {
        self.lines[line].asc = scale * asc;
        self.lines[line].desc = scale * desc;
        self.height += scale * asc + scale * desc;
        // VSFilter rounds these with a bias.
        self.border_bottom = (self.s.border_scale.1 * by + 0.5) as i32;

        if line == 0 {
            self.border_top = self.border_bottom;
        }

        self.border_x = self.border_x.max((self.s.border_scale.0 * bx + 0.5) as i32);
    }

    fn measure_text(&mut self) {
        self.height = 0.0;
        self.border_x = 0;
        let mut line = 0;
        let mut scale = 0.5;
        let (mut asc, mut desc, mut bx, mut by) = (0f64, 0f64, 0f64, 0f64);
        let mut empty = true;

        for i in 0..self.glyphs.len() {
            if self.glyphs[i].linebreak != 0 {
                self.end_of_line(scale, line, asc, desc, bx, by);
                empty = true;
                (asc, desc, bx, by) = (0.0, 0.0, 0.0, 0.0);
                scale = 0.5;
                line += 1;
            }

            let g = &self.glyphs[i];

            // VSFilter ignores whitespace at either end of a line, unless that's all there is.
            if empty && !g.trimmed {
                empty = false;
                (asc, desc, bx, by) = (0.0, 0.0, 0.0, 0.0);
            } else if !empty && g.trimmed {
                continue;
            }

            asc = asc.max(g.root().asc);
            desc = desc.max(g.root().desc);
            by = by.max(g.border.y);
            bx = bx.max(g.border.x);

            if g.symbol != '\n' as u32 {
                scale = 1.0;
            }
        }

        self.end_of_line(scale, line, asc, desc, bx, by);
    }

    /// Positions each line under the one before.
    fn wrap_measure(&mut self) {
        let n = self.glyphs.len();
        let mut line = 1;
        let mut i = 0;

        while i < n && self.glyphs[i].skip {
            i += 1;
        }

        let mut shift_x = -self.glyphs.get(i).map(|g| g.root().pos.x).unwrap_or(0.0);
        let mut shift_y = 0.0;
        let mut i = 0;

        while i < n {
            if self.glyphs[i].linebreak != 0 {
                while i < n && self.glyphs[i].skip && self.glyphs[i].symbol != '\n' as u32 {
                    i += 1;
                }

                let height = self.lines[line - 1].desc + self.lines[line].asc;
                self.lines[line - 1].len = i - self.lines[line - 1].offset;
                self.lines[line].offset = i;
                line += 1;
                shift_x = -self.glyphs.get(i).map(|g| g.root().pos.x).unwrap_or(0.0);
                shift_y += height;
            }

            if i < n {
                for part in &mut self.glyphs[i].parts {
                    part.pos = part.pos + pt(shift_x, shift_y);
                }
            }

            i += 1;
        }

        let last = line - 1;
        self.lines[last].len = n - self.lines[last].offset;
    }

    /// Karaoke: where each word's fill has got to, as an x for every glyph.
    fn karaoke(&mut self) {
        let n = self.glyphs.len();
        let now = self.f.time - self.event.start;
        let (mut timing, mut skip_timing) = (0i64, 0i64);
        let mut effect = Effect::None;
        let mut last_boundary: Option<usize> = None;
        let mut has_reset = false;

        for i in 0..=n {
            if i < n && !self.glyphs[i].starts_new_run {
                if self.glyphs[i].reset_effect {
                    has_reset = true;
                    skip_timing = 0;
                }

                // \k12345\k0 without a run break keeps the word going.
                skip_timing += self.glyphs[i].effect_skip_timing as u32 as i64;
                continue;
            }

            let start = last_boundary;
            last_boundary = Some(i);
            let Some(start) = start else { continue };

            if self.glyphs[start].effect != Effect::None {
                effect = self.glyphs[start].effect;
            }

            if effect == Effect::None {
                continue;
            }

            if self.glyphs[start].reset_effect {
                timing = 0;
            }

            let tm_start = timing + self.glyphs[start].effect_skip_timing as i64;
            let mut tm_end = tm_start + self.glyphs[start].effect_timing as i64;
            timing = if has_reset { 0 } else { tm_end } + skip_timing;
            skip_timing = 0;
            has_reset = false;

            if effect != Effect::KaraokeKf {
                tm_end = tm_start;
            }

            let x = if now < tm_start {
                -100000000.0 / 64.0
            } else if now >= tm_end {
                100000000.0 / 64.0
            } else {
                let (mut first, mut last) = (start, i - 1);

                while first < last && self.glyphs[first].skip {
                    first += 1;
                }

                while first < last && self.glyphs[last].skip {
                    last -= 1;
                }

                let x_start = self.glyphs[first].root().pos.x;
                let x_end = self.glyphs[last].root().pos.x + self.glyphs[last].root().advance.x;
                let mut dt = (now - tm_start) as f64 / (tm_end - tm_start) as f64;
                let frz = self.glyphs[start].frz % 360.0;

                // Upside down, it fills from the right.
                if frz > 90.0 && frz < 270.0 {
                    dt = 1.0 - dt;

                    for g in &mut self.glyphs[start..i] {
                        g.c.swap(0, 1);
                    }
                }

                x_start + ((x_end - x_start) * dt * 64.0).round() / 64.0
            };

            for g in &mut self.glyphs[start..i] {
                g.effect = effect;
                g.karaoke = x - g.root().pos.x;
            }
        }
    }

    /// Visual order: each line (and run, unless the text is laid out whole) reordered for bidi.
    fn reorder(&mut self) {
        let n = self.glyphs.len();
        let text: Vec<u32> = self.glyphs.iter().map(|g| g.symbol).collect();
        let mut cmap: Vec<usize> = (0..n).collect();
        let mut last = 0;

        for i in 0..n {
            let brk = i == n - 1
                || self.glyphs[i + 1].linebreak != 0
                || shape::is_paragraph_separator(text[i])
                || (!self.whole_text && (self.glyphs[i + 1].starts_new_run || self.glyphs[i].hspacing != 0.0));

            if brk {
                let mut order = vec![0; i + 1 - last];
                shape::reorder(&text[last..=i], &self.levels[last..=i], self.base_levels[last], &mut order);

                for (k, o) in order.into_iter().enumerate() {
                    cmap[last + k] = last + o;
                }

                last = i + 1;
            }
        }

        let mut pen = pt(0.0, 0.0);
        let mut line = 1;

        for i in 0..n {
            if self.glyphs[i].linebreak != 0 {
                pen.x = 0.0;
                pen.y += self.lines[line - 1].desc + self.lines[line].asc;
                line += 1;
            }

            let g = &mut self.glyphs[cmap[i]];

            if g.skip {
                continue;
            }

            let mut cluster_pen = pen;
            pen = pen + g.cluster_advance;

            for part in &mut g.parts {
                part.pos = part.offset + cluster_pen;
                cluster_pen = cluster_pen + part.advance;
            }
        }

        self.cmap = cmap;
    }

    fn baseline_shear(&mut self) {
        let mut shear = 0.0;

        for i in 0..self.glyphs.len() {
            let idx = self.cmap[i];

            if self.glyphs[i].linebreak != 0 || (!self.whole_text && self.glyphs[i].starts_new_run) {
                shear = 0.0;
            }

            let g = &mut self.glyphs[idx];

            if g.scale_x == 0.0 || g.scale_y == 0.0 {
                g.skip = true;
            }

            if g.skip {
                continue;
            }

            for part in &mut g.parts {
                part.pos.y += shear;
            }

            shear += g.fay / g.scale_x * g.scale_y * g.cluster_advance.x;
        }
    }

    fn align_lines(&mut self, max_text_width: f64) {
        let n = self.glyphs.len();
        let mut halign = self.s.alignment & 3;
        let mut justify = 0;

        if self.s.evt_type & EVENT_HSCROLL != 0 {
            justify = halign;
            halign = HALIGN_LEFT;
        }

        let counts = |g: &Glyph| !g.skip && g.symbol != '\n' as u32 && g.symbol != 0;
        let mut max_width = 0.0f64;
        let mut width = 0.0;

        for i in 0..=n {
            if i == n || self.glyphs[i].linebreak != 0 {
                max_width = max_width.max(width);
                width = 0.0;
            }

            if i < n && counts(&self.glyphs[i]) {
                width += self.glyphs[i].cluster_advance.x;
            }
        }

        let mut last_break: isize = -1;
        width = 0.0;

        for i in 0..=n {
            if i == n || self.glyphs[i].linebreak != 0 {
                let shift = match (halign, justify) {
                    (HALIGN_LEFT, HALIGN_RIGHT) => max_width - width,
                    (HALIGN_LEFT, HALIGN_CENTER) => (max_width - width) / 2.0,
                    (HALIGN_LEFT, _) => 0.0,
                    (HALIGN_RIGHT, HALIGN_LEFT) => max_text_width - max_width,
                    (HALIGN_RIGHT, HALIGN_CENTER) => max_text_width - max_width + (max_width - width) / 2.0,
                    (HALIGN_RIGHT, _) => max_text_width - width,
                    (HALIGN_CENTER, HALIGN_LEFT) => (max_text_width - max_width) / 2.0,
                    (HALIGN_CENTER, HALIGN_RIGHT) => (max_text_width - max_width) / 2.0 + max_width - width,
                    (HALIGN_CENTER, _) => (max_text_width - width) / 2.0,
                    _ => 0.0,
                };

                for g in &mut self.glyphs[(last_break + 1) as usize..i] {
                    for part in &mut g.parts {
                        part.pos.x += shift;
                    }
                }

                last_break = i as isize - 1;
                width = 0.0;
            }

            if i < n && counts(&self.glyphs[i]) {
                width += self.glyphs[i].cluster_advance.x;
            }
        }
    }

    fn string_bbox(&self) -> Rect {
        if self.glyphs.is_empty() {
            return Rect::default();
        }

        let y0 = -self.lines[0].asc;
        let mut b = Rect { x0: 32000.0, x1: -32000.0, y0, y1: y0 + self.height };

        for g in self.glyphs.iter().filter(|g| !g.skip) {
            let s = g.root().pos.x;
            b.x0 = b.x0.min(s);
            b.x1 = b.x1.max(s + g.cluster_advance.x);
        }

        b
    }

    fn rotation_params(&mut self, bbox: &Rect, device: Pt) {
        let f = self.f;

        let center = if self.s.have_origin {
            pt(f.x2scr_pos(self.s.org.0), f.y2scr_pos(self.s.org.1))
        } else {
            let (bx, by) = base_point(bbox, self.s.alignment);
            pt(device.x + bx, device.y + by)
        };

        let bs = self.s.border_scale;

        for g in &mut self.glyphs {
            let shadow = pt(g.shadow.x * bs.0 / f.par, g.shadow.y * bs.1);

            for part in &mut g.parts {
                part.shift = part.pos + (device - center) + shadow;
            }
        }
    }

    /// Masks for every run, added together and filtered.
    fn render_and_combine(&mut self, r: &mut Resources, device: Pt) -> Vec<Combined> {
        let f = self.f;
        let left = f.margins.2 as f64;
        let device = pt((device.x - left) * f.par + left, device.y);
        let mut combined: Vec<Combined> = Vec::new();
        let mut new_run = true;
        let mut residual = pt(0.0, 0.0);
        let bs = self.s.border_scale;
        let blur_scale = self.s.blur_scale;

        for i in 0..self.glyphs.len() {
            if self.glyphs[i].starts_new_run {
                new_run = true;
            }

            if self.glyphs[i].skip {
                continue;
            }

            let nparts = self.glyphs[i].parts.len();

            for k in 0..nparts {
                let g = &self.glyphs[i];
                let mut flags = 0;

                if g.border_style == 3 {
                    flags |= FILTER_BORDER_STYLE_3;
                }

                if g.border.x != 0.0 || g.border.y != 0.0 {
                    flags |= FILTER_NONZERO_BORDER;
                }

                if g.shadow.x != 0.0 || g.shadow.y != 0.0 {
                    flags |= FILTER_NONZERO_SHADOW;
                }

                if flags & FILTER_NONZERO_SHADOW != 0
                    && (g.effect == Effect::KaraokeKf
                        || g.effect == Effect::KaraokeKo
                        || g.a_pre_fade[0] != 0xFF
                        || g.border_style == 3)
                {
                    flags |= FILTER_FILL_IN_SHADOW;
                }

                if flags & FILTER_NONZERO_BORDER == 0 && flags & FILTER_FILL_IN_SHADOW == 0 {
                    flags &= !FILTER_NONZERO_SHADOW;
                }

                if (flags & FILTER_NONZERO_BORDER != 0 && g.a_pre_fade[0] == 0 && g.a_pre_fade[1] == 0 && g.fade == 0)
                    || g.border_style == 3
                {
                    flags |= FILTER_FILL_IN_BORDER;
                }

                if new_run {
                    // The blur is a gaussian whose radius is where it falls to 1/256.
                    let radius_scale = 2.0 / 256f64.ln().sqrt();
                    let (blur_x, mask_x) = glyphs::quantize_blur(g.blur * blur_scale.0 * radius_scale);
                    let (blur_y, mask_y) = glyphs::quantize_blur(g.blur * blur_scale.1 * radius_scale);

                    let shadow = if flags & FILTER_NONZERO_SHADOW != 0 {
                        let x = (g.shadow.x * bs.0 * 64.0).round_ties_even() as i32;
                        let y = (g.shadow.y * bs.1 * 64.0).round_ties_even() as i32;
                        ((x + (mask_x >> 1)) & !mask_x, (y + (mask_y >> 1)) & !mask_y)
                    } else {
                        (0, 0)
                    };

                    combined.push(Combined {
                        filter: Filter { flags, be: g.be, blur_x, blur_y, shadow },
                        c: g.c,
                        effect: g.effect,
                        karaoke: g.karaoke,
                        effect_timing: 0,
                        leftmost_x: f64::INFINITY,
                        refs: Vec::new(),
                        x: i32::MAX,
                        y: i32::MAX,
                        image: None,
                    });

                    new_run = false;
                }

                let cur = combined.last_mut().unwrap();
                let g = &mut self.glyphs[i];
                let part = &mut g.parts[k];
                part.pos = pt(device.x + part.pos.x * f.par, device.y + part.pos.y);
                let first = cur.refs.is_empty();
                let view = glyphs::view(f, &cur.filter);

                if let Some(b) =
                    glyphs::bitmaps(f, &self.s, r, g, k, flags, first, &mut residual, &mut cur.leftmost_x, view)
                {
                    cur.x = cur.x.min(b.pos.0);
                    cur.y = cur.y.min(b.pos.1);
                    cur.refs.push(b);
                }
            }
        }

        for c in &mut combined {
            if c.refs.is_empty() {
                continue;
            }

            c.effect_timing = if c.effect == Effect::KaraokeKf {
                (c.leftmost_x + c.karaoke * f.par).round().clamp(-1e9, 1e9) as i32
            } else if c.karaoke > 0.0 {
                1
            } else {
                -1
            };

            for b in &mut c.refs {
                b.pos.0 -= c.x;
                b.pos.1 -= c.y;
                b.pos_o.0 -= c.x;
                b.pos_o.1 -= c.y;
            }

            c.image = Some(r.caches.composite(&c.filter, &c.refs));
        }

        combined
    }

    /// The images: shadows, then borders, then the glyphs themselves, clipped.
    fn render_text(&self, r: &mut Resources, combined: &mut [Combined], clip: [i32; 4]) -> Vec<Image> {
        let mut out = Vec::new();

        for c in combined.iter() {
            let Some(img) = &c.image else { continue };

            if self.s.border_style != 4
                && let Some(bm) = &img.shadow
            {
                self.push(&mut out, bm, c.x, c.y, c.c[3], 0, 1000000, ImageKind::Shadow, clip);
            }
        }

        for c in combined.iter() {
            let Some(img) = &c.image else { continue };

            if let Some(bm) = &img.border
                && !(c.effect == Effect::KaraokeKo && c.effect_timing <= 0)
            {
                self.push(&mut out, bm, c.x, c.y, c.c[2], 0, 1000000, ImageKind::Outline, clip);
            }
        }

        for c in combined.iter() {
            let Some(img) = &c.image else { continue };
            let Some(bm) = &img.fill else { continue };

            match c.effect {
                Effect::Karaoke | Effect::KaraokeKo => {
                    let color = if c.effect_timing > 0 { c.c[0] } else { c.c[1] };
                    self.push(&mut out, bm, c.x, c.y, color, 0, 1000000, ImageKind::Character, clip);
                },
                Effect::KaraokeKf => {
                    self.push(&mut out, bm, c.x, c.y, c.c[0], c.c[1], c.effect_timing, ImageKind::Character, clip);
                },
                Effect::None => {
                    self.push(&mut out, bm, c.x, c.y, c.c[0], 0, 1000000, ImageKind::Character, clip);
                },
            }
        }

        if let Some(cd) = &self.s.clip_drawing {
            glyphs::vector_clip(self.f, &self.s, r, cd, &mut out);
        }

        out
    }

    /// One mask as images, split at the karaoke break (`color` left of it, `color2` right) and
    /// clipped.
    #[allow(clippy::too_many_arguments)]
    fn push(
        &self,
        out: &mut Vec<Image>,
        bm: &Arc<Bitmap>,
        x: i32,
        y: i32,
        color: u32,
        color2: u32,
        brk: i32,
        kind: ImageKind,
        clip: [i32; 4],
    ) {
        let f = self.f;
        let dst_x = x + bm.left;
        let dst_y = y + bm.top;
        let brk = brk - dst_x;
        let mut rects: Vec<[i32; 4]> = Vec::new();

        if self.s.clip_inverse {
            let zx = f.x2scr_pos_scaled(0.0) as i32;
            let zy = f.y2scr_pos(0.0) as i32;
            let sx = f.x2scr_pos_scaled(f.track.play_res.0 as f64) as i32;
            let sy = f.y2scr_pos(f.track.play_res.1 as f64) as i32;
            let (x1, y1) = (bm.w, bm.h);
            let (cx0, cy0, cx1, cy1) = (clip[0] - dst_x, clip[1] - dst_y, clip[2] - dst_x, clip[3] - dst_y);

            let candidates = [
                [0, 0, cx0.min(x1), y1],
                [cx0.max(0), 0, cx1.min(x1), cy0.min(y1)],
                [cx0.max(0), cy1.max(0), cx1.min(x1), y1],
                [cx1.max(0), 0, x1, y1],
            ];

            for r in candidates {
                if r[2] > r[0] && r[3] > r[1] {
                    rects.push([
                        if r[0] + dst_x < zx { zx - dst_x } else { r[0] },
                        if r[1] + dst_y < zy { zy - dst_y } else { r[1] },
                        if r[2] + dst_x > sx { sx - dst_x } else { r[2] },
                        if r[3] + dst_y > sy { sy - dst_y } else { r[3] },
                    ]);
                }
            }
        } else {
            let cx0 = clip[0].clamp(0, f.width);
            let cy0 = clip[1].clamp(0, f.height);
            let cx1 = clip[2].clamp(0, f.width);
            let cy1 = clip[3].clamp(0, f.height);
            rects.push([(cx0 - dst_x).max(0), (cy0 - dst_y).max(0), (cx1 - dst_x).min(bm.w), (cy1 - dst_y).min(bm.h)]);
        }

        for [x0, y0, x1, y1] in rects {
            if x1 <= x0 || y1 <= y0 {
                continue;
            }

            if brk > x0 {
                let b = brk.min(x1);
                out.push(Image::new(bm.clone(), x0, y0, b - x0, y1 - y0, dst_x + x0, dst_y + y0, color, kind));
            }

            if brk < x1 {
                let b = brk.max(x0);
                out.push(Image::new(bm.clone(), b, y0, x1 - b, y1 - y0, dst_x + b, dst_y + y0, color2, kind));
            }
        }
    }

    /// BorderStyle 4: a box behind the whole event, in the shadow's colour.
    fn add_background(&self, images: &mut Vec<Image>, left: i32, top: i32, width: i32, height: i32) {
        let f = self.f;
        let bs = self.s.border_scale;
        let sx = if self.s.shadow_x > 0.0 { (self.s.shadow_x * bs.0).round() as i32 } else { 0 };
        let sy = if self.s.shadow_y > 0.0 { (self.s.shadow_y * bs.1).round() as i32 } else { 0 };
        let l = (left - sx).clamp(0, f.width);
        let t = (top - sy).clamp(0, f.height);
        let r = (left + width + sx).clamp(0, f.width);
        let b = (top + height + sy).clamp(0, f.height);
        let (w, h) = (r - l, b - t);

        if w < 1 || h < 1 {
            return;
        }

        let mut bm = Bitmap::new(0, 0, w, h);
        bm.data.fill(255);
        images.insert(0, Image::new(Arc::new(bm), 0, 0, w, h, l, t, self.s.c[3], ImageKind::Shadow));
    }
}

/// Characters HarfBuzz replaces with nothing.
fn ignorable(c: u32) -> bool {
    match c >> 8 {
        0x00 => c == 0x00AD,
        0x03 => c == 0x034F,
        0x06 => c == 0x061C,
        0x17 => (0x17B4..=0x17B5).contains(&c),
        0x18 => (0x180B..=0x180E).contains(&c),
        0x20 => (0x200B..=0x200F).contains(&c) || (0x202A..=0x202E).contains(&c) || (0x2060..=0x206F).contains(&c),
        0xFE => (0xFE00..=0xFE0F).contains(&c) || c == 0xFEFF,
        0xFF => (0xFFF0..=0xFFF8).contains(&c),
        0x1D1 => (0x1D173..=0x1D17A).contains(&c),
        _ => (0xE0000..=0xE0FFF).contains(&c),
    }
}

/// ZWJ and ZWNJ affect their neighbours across runs.
fn control(c: u32) -> bool {
    c == 0x200C || c == 0x200D
}
