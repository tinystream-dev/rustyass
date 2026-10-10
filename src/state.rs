// SPDX-License-Identifier: LGPL-3.0-or-later
//! What override tags change while an event is read: the style its next characters take.

use crate::event::Frame;
use crate::font::{DECO_STRIKETHROUGH, DECO_UNDERLINE, FontCache};
use crate::text::{self, skip_spaces, trim_end_spaces};
use crate::track::{Event, Style, numpad_to_align};

pub(crate) const BLUR_MAX_RADIUS: f64 = 100.0;
const MAX_BE: i32 = 127;
const PARSED_FADE: u32 = 1;
const PARSED_A: u32 = 2;

pub(crate) const EVENT_POSITIONED: u32 = 1;
pub(crate) const EVENT_HSCROLL: u32 = 2;
pub(crate) const EVENT_VSCROLL: u32 = 4;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Effect {
    #[default]
    None,
    Karaoke,
    KaraokeKf,
    KaraokeKo,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Scroll {
    #[default]
    LeftToRight,
    RightToLeft,
    TopToBottom,
    BottomToTop,
}

/// A `\clip` or `\iclip` drawing, `scale` as `\p`'s.
#[derive(Clone, Debug)]
pub(crate) struct ClipDrawing {
    pub text: String,
    pub scale: i32,
    pub inverse: bool,
}

pub(crate) struct State {
    pub style: Style,
    pub font: Option<usize>,
    pub font_size: f64,
    parsed: u32,
    pub flags: u32,
    pub alignment: i32,
    pub frx: f64,
    pub fry: f64,
    pub frz: f64,
    pub fax: f64,
    pub fay: f64,
    pub pos: (f64, f64),
    pub org: (f64, f64),
    pub scale_x: f64,
    pub scale_y: f64,
    pub hspacing: f64,
    pub border_x: f64,
    pub border_y: f64,
    pub evt_type: u32,
    pub border_style: i32,
    pub c: [u32; 4],
    pub clip: [i32; 4],
    pub have_origin: bool,
    pub clip_inverse: bool,
    pub detect_collisions: bool,
    pub be: i32,
    pub fade: i32,
    pub blur: f64,
    pub shadow_x: f64,
    pub shadow_y: f64,
    pub pbo: f64,
    pub clip_drawing: Option<ClipDrawing>,
    pub drawing_scale: i32,
    pub effect: Effect,
    pub effect_timing: i32,
    pub effect_skip_timing: i32,
    pub reset_effect: bool,
    pub scroll: Scroll,
    pub scroll_shift: f64,
    pub scroll_y0: i32,
    pub scroll_y1: i32,
    pub family: String,
    pub bold: i32,
    pub italic: i32,
    pub wrap_style: i32,
    pub font_encoding: i32,
    pub explicit: bool,
    pub screen_scale: (f64, f64),
    pub border_scale: (f64, f64),
    pub blur_scale: (f64, f64),
}

/// The values of a tag's arguments.
#[derive(Clone, Copy)]
struct Arg<'a>(&'a str);

impl Arg<'_> {
    fn i32(self) -> i32 {
        text::strtoi32(self.0, 10).0
    }

    fn f64(self) -> f64 {
        text::atof(self.0)
    }
}

const MAX_ARGS: usize = 7;

impl State {
    /// A fresh state for `event`, everything from its style.
    pub fn new(f: &Frame, fonts: &mut FontCache, event: &Event) -> State {
        let track = f.track;

        let mut s = State {
            style: track.styles[event.style].clone(),
            font: None,
            font_size: 0.0,
            parsed: 0,
            flags: 0,
            alignment: 0,
            frx: 0.0,
            fry: 0.0,
            frz: 0.0,
            fax: 0.0,
            fay: 0.0,
            pos: (0.0, 0.0),
            org: (0.0, 0.0),
            scale_x: 1.0,
            scale_y: 1.0,
            hspacing: 0.0,
            border_x: 0.0,
            border_y: 0.0,
            evt_type: 0,
            border_style: 1,
            c: [0; 4],
            clip: [0, 0, track.play_res.0, track.play_res.1],
            have_origin: false,
            clip_inverse: false,
            detect_collisions: true,
            be: 0,
            fade: 0,
            blur: 0.0,
            shadow_x: 0.0,
            shadow_y: 0.0,
            pbo: 0.0,
            clip_drawing: None,
            drawing_scale: 0,
            effect: Effect::None,
            effect_timing: 0,
            effect_skip_timing: 0,
            reset_effect: false,
            scroll: Scroll::LeftToRight,
            scroll_shift: 0.0,
            scroll_y0: 0,
            scroll_y1: 0,
            family: String::new(),
            bold: 0,
            italic: 0,
            wrap_style: track.wrap_style,
            font_encoding: 0,
            explicit: false,
            screen_scale: (1.0, 1.0),
            border_scale: (1.0, 1.0),
            blur_scale: (1.0, 1.0),
        };

        s.transition_effects(f, event);
        s.explicit = s.evt_type != 0 || has_hard_overrides(&event.text);
        s.reset(f, fonts, None, event);
        s.alignment = s.style.alignment;
        s
    }

    fn init_font_scale(&mut self, f: &Frame) {
        let (w, h) = if !self.explicit && f.use_margins { f.fit } else { f.content };
        let (px, py) = (f.track.play_res.0 as f64, f.track.play_res.1 as f64);
        self.screen_scale = (w / px, h / py);
        self.blur_scale = (w / f.layout_res.0 as f64, h / f.layout_res.1 as f64);
        self.border_scale = if f.track.scaled_border_and_shadow { self.screen_scale } else { self.blur_scale };
    }

    /// Back to a style, as `\r` does: the event's own, or another.
    pub fn reset(&mut self, f: &Frame, fonts: &mut FontCache, style: Option<&Style>, event: &Event) {
        let style = style.cloned().unwrap_or_else(|| f.track.styles[event.style].clone());
        self.init_font_scale(f);
        self.c = style.colors;
        self.flags =
            if style.underline { DECO_UNDERLINE } else { 0 } | if style.strike_out { DECO_STRIKETHROUGH } else { 0 };
        self.font_size = style.font_size;
        self.family = style.font_name.clone();
        self.bold = style.bold;
        self.italic = style.italic;
        self.border_style = style.border_style;
        self.border_x = style.outline;
        self.border_y = style.outline;
        self.scale_x = style.scale_x;
        self.scale_y = style.scale_y;
        self.hspacing = style.spacing;
        self.be = 0;
        self.blur = style.blur;
        self.shadow_x = style.shadow;
        self.shadow_y = style.shadow;
        self.frx = 0.0;
        self.fry = 0.0;
        self.frz = style.angle;
        self.fax = 0.0;
        self.fay = 0.0;
        self.font_encoding = style.encoding;
        self.style = style;
        self.update_font(f, fonts);
    }

    pub fn update_font(&mut self, f: &Frame, fonts: &mut FontCache) {
        let (vertical, family) = match self.family.strip_prefix('@') {
            Some(rest) => (true, rest),
            None => (false, self.family.as_str()),
        };

        let weight = match self.bold {
            1 | -1 => 700,
            b if b <= 0 => 400,
            b => b as u32,
        };

        let italic = match self.italic {
            1 => 100,
            i if i <= 0 => 0,
            i => i as u32,
        };

        self.font = fonts.get(f.fonts, family, weight, italic, vertical);
    }

    /// Scrolling effects from the event's Effect field.
    fn transition_effects(&mut self, f: &Frame, event: &Event) {
        let effect = event.effect.as_str();

        if effect.is_empty() {
            return;
        }

        let mut v = [0i32; 4];
        let mut cnt = 0;
        let mut rest = effect;

        while cnt < 4 {
            let Some(i) = rest.find(';') else { break };
            rest = &rest[i + 1..];
            v[cnt] = text::strtoi32(rest, 10).0;
            cnt += 1;
        }

        let track = f.track;

        if effect.starts_with("Banner;") {
            if cnt < 1 {
                return;
            }

            self.scroll = if cnt >= 2 && v[1] != 0 { Scroll::LeftToRight } else { Scroll::RightToLeft };
            // VSFilter scales the delay to the storage size and truncates it there.
            let scale_x = f.layout_res.0 as f64 / track.play_res.0 as f64;
            let delay = ((v[0] as f64 / scale_x).max(1.0) as i32) as f64 * scale_x;
            self.scroll_shift = (f.time - event.start) as f64 / delay;
            self.evt_type |= EVENT_HSCROLL;
            self.detect_collisions = false;
            self.wrap_style = 2;
            return;
        }

        if effect.starts_with("Scroll up;") {
            self.scroll = Scroll::BottomToTop;
        } else if effect.starts_with("Scroll down;") {
            self.scroll = Scroll::TopToBottom;
        } else {
            return;
        }

        if cnt < 3 {
            return;
        }

        let scale_y = f.layout_res.1 as f64 / track.play_res.1 as f64;
        let delay = ((v[2] as f64 / scale_y).max(1.0) as i32) as f64 * scale_y;
        self.scroll_shift = (f.time - event.start) as f64 / delay;
        (self.scroll_y0, self.scroll_y1) = if v[0] < v[1] { (v[0], v[1]) } else { (v[1], v[0]) };
        self.evt_type |= EVENT_VSCROLL;
        self.detect_collisions = false;
    }

    /// Applies the tags in `s` (an override block's inside), `pwr` being how far through a `\t`
    /// transition they are.
    pub fn parse_tags(&mut self, f: &Frame, fonts: &mut FontCache, event: &Event, s: &str, pwr: f64, nested: bool) {
        let mut p = 0;
        let b = s.as_bytes();
        let mut pwr = pwr;
        let mut nested = nested;

        while p < b.len() {
            while p < b.len() && b[p] != b'\\' {
                p += 1;
            }

            if p >= b.len() {
                break;
            }

            p += 1;
            p += s[p..].len() - skip_spaces(&s[p..]).len();

            let mut q = p;

            while q < b.len() && b[q] != b'(' && b[q] != b'\\' {
                q += 1;
            }

            if q == p {
                continue;
            }

            let name_end = q;
            let mut args: Vec<Arg> = Vec::new();
            let mut has_backslash_arg = false;

            // Arguments in parentheses come first, for every tag, as in VSFilter.
            if q < b.len() && b[q] == b'(' {
                q += 1;

                loop {
                    q += s[q..].len() - skip_spaces(&s[q..]).len();
                    let mut r = q;

                    while r < b.len() && b[r] != b',' && b[r] != b'\\' && b[r] != b')' {
                        r += 1;
                    }

                    if r < b.len() && b[r] == b',' {
                        push_arg(&mut args, &s[q..r]);
                        q = r + 1;
                    } else {
                        // A backslash means tags: everything to the closing parenthesis is one.
                        if r < b.len() && b[r] == b'\\' {
                            has_backslash_arg = true;
                            r = s[r..].find(')').map(|i| r + i).unwrap_or(b.len());
                        }

                        push_arg(&mut args, &s[q..r]);
                        q = r;

                        if q < b.len() {
                            q += 1;
                        }

                        break;
                    }
                }
            }

            let name = &s[p..name_end];
            let mut rest_p = q;

            // A plain tag takes what follows its name as its argument.

            let complex = |tagname: &str| name.starts_with(tagname);
            let style = self.style.clone();
            let mix = |old: f64, new: f64| old * (1.0 - pwr) + new * pwr;

            if tag(name, "xbord", &mut args) {
                self.border_x = match args.first() {
                    Some(a) => mix(self.border_x, a.f64()).max(0.0),
                    None => style.outline,
                };
            } else if tag(name, "ybord", &mut args) {
                self.border_y = match args.first() {
                    Some(a) => mix(self.border_y, a.f64()).max(0.0),
                    None => style.outline,
                };
            } else if tag(name, "xshad", &mut args) {
                self.shadow_x = match args.first() {
                    Some(a) => mix(self.shadow_x, a.f64()),
                    None => style.shadow,
                };
            } else if tag(name, "yshad", &mut args) {
                self.shadow_y = match args.first() {
                    Some(a) => mix(self.shadow_y, a.f64()),
                    None => style.shadow,
                };
            } else if tag(name, "fax", &mut args) {
                self.fax = args.first().map(|a| a.f64() * pwr + self.fax * (1.0 - pwr)).unwrap_or(0.0);
            } else if tag(name, "fay", &mut args) {
                self.fay = args.first().map(|a| a.f64() * pwr + self.fay * (1.0 - pwr)).unwrap_or(0.0);
            } else if complex("iclip") {
                self.clip_tag(&args, pwr, true);
            } else if tag(name, "blur", &mut args) {
                self.blur = match args.first() {
                    Some(a) => mix(self.blur, a.f64()).clamp(0.0, BLUR_MAX_RADIUS),
                    None => 0.0,
                };
            } else if tag(name, "fscx", &mut args) {
                self.scale_x = match args.first() {
                    Some(a) => mix(self.scale_x, a.f64() / 100.0).max(0.0),
                    None => style.scale_x,
                };
            } else if tag(name, "fscy", &mut args) {
                self.scale_y = match args.first() {
                    Some(a) => mix(self.scale_y, a.f64() / 100.0).max(0.0),
                    None => style.scale_y,
                };
            } else if tag(name, "fsc", &mut args) {
                self.scale_x = style.scale_x;
                self.scale_y = style.scale_y;
            } else if tag(name, "fsp", &mut args) {
                self.hspacing = match args.first() {
                    Some(a) => mix(self.hspacing, a.f64()),
                    None => style.spacing,
                };
            } else if tag(name, "fs", &mut args) {
                let mut val = 0.0;

                if let Some(a) = args.first() {
                    val = a.f64();

                    if a.0.starts_with('+') || a.0.starts_with('-') {
                        val = self.font_size * (1.0 + pwr * val / 10.0);
                    } else {
                        val = mix(self.font_size, val);
                    }
                }

                self.font_size = if val <= 0.0 { style.font_size } else { val };
            } else if tag(name, "bord", &mut args) {
                (self.border_x, self.border_y) = match args.first() {
                    Some(a) => {
                        let v = a.f64();
                        (mix(self.border_x, v).max(0.0), mix(self.border_y, v).max(0.0))
                    },
                    None => (style.outline, style.outline),
                };
            } else if complex("move") {
                if args.len() != 4 && args.len() != 6 {
                    p = rest_p;
                    continue;
                }

                let (x1, y1, x2, y2) = (args[0].f64(), args[1].f64(), args[2].f64(), args[3].f64());
                let (mut t1, mut t2) = (0i32, 0i32);

                if args.len() == 6 {
                    t1 = args[4].i32();
                    t2 = args[5].i32();

                    if t1 > t2 {
                        std::mem::swap(&mut t1, &mut t2);
                    }
                }

                if t1 <= 0 && t2 <= 0 {
                    t1 = 0;
                    t2 = event.duration as i32;
                }

                let delta = t2.wrapping_sub(t1);
                let t = (f.time - event.start) as i32;

                let k = if t <= t1 {
                    0.0
                } else if t >= t2 {
                    1.0
                } else {
                    t.wrapping_sub(t1) as f64 / delta as f64
                };

                if self.evt_type & EVENT_POSITIONED == 0 {
                    self.pos = (k * (x2 - x1) + x1, k * (y2 - y1) + y1);
                    self.detect_collisions = false;
                    self.evt_type |= EVENT_POSITIONED;
                }
            } else if tag(name, "frx", &mut args) {
                self.frx = args.first().map(|a| a.f64() * pwr + self.frx * (1.0 - pwr)).unwrap_or(0.0);
            } else if tag(name, "fry", &mut args) {
                self.fry = args.first().map(|a| a.f64() * pwr + self.fry * (1.0 - pwr)).unwrap_or(0.0);
            } else if tag(name, "frz", &mut args) || tag(name, "fr", &mut args) {
                self.frz = args.first().map(|a| a.f64() * pwr + self.frz * (1.0 - pwr)).unwrap_or(style.angle);
            } else if tag(name, "fn", &mut args) {
                self.family = match args.first() {
                    Some(a) if a.0 != "0" => skip_spaces(a.0).to_string(),
                    _ => style.font_name.clone(),
                };

                self.update_font(f, fonts);
            } else if tag(name, "alpha", &mut args) {
                match args.first() {
                    Some(a) => {
                        let v = alpha_tag(a.0);

                        for c in &mut self.c {
                            change_alpha(c, v, pwr);
                        }
                    },
                    None => {
                        for (c, s) in self.c.iter_mut().zip(style.colors) {
                            change_alpha(c, (s & 0xFF) as i32, 1.0);
                        }
                    },
                }
            } else if tag(name, "an", &mut args) {
                let v = args.first().map(|a| a.i32()).unwrap_or(0);

                if self.parsed & PARSED_A == 0 {
                    self.alignment = if (1..=9).contains(&v) { numpad_to_align(v) } else { style.alignment };
                    self.parsed |= PARSED_A;
                }
            } else if tag(name, "a", &mut args) {
                let v = args.first().map(|a| a.i32()).unwrap_or(0);

                if self.parsed & PARSED_A == 0 {
                    // VSFilter reads the illegal \a4 and \a8 as \a5.
                    self.alignment =
                        if (1..=11).contains(&v) { if v & 3 == 0 { 5 } else { v } } else { style.alignment };
                    self.parsed |= PARSED_A;
                }
            } else if complex("pos") {
                if args.len() != 2 {
                    p = rest_p;
                    continue;
                }

                if self.evt_type & EVENT_POSITIONED == 0 {
                    self.evt_type |= EVENT_POSITIONED;
                    self.detect_collisions = false;
                    self.pos = (args[0].f64(), args[1].f64());
                }
            } else if complex("fade") || complex("fad") {
                let (a1, a2, a3, mut t1, t2, mut t3, mut t4) = match args.len() {
                    2 => (0xFF, 0, 0xFF, -1, args[0].i32(), args[1].i32(), -1),
                    7 => (
                        args[0].i32(),
                        args[1].i32(),
                        args[2].i32(),
                        args[3].i32(),
                        args[4].i32(),
                        args[5].i32(),
                        args[6].i32(),
                    ),
                    _ => {
                        p = rest_p;
                        continue;
                    },
                };

                if t1 == -1 && t4 == -1 {
                    t1 = 0;
                    t4 = event.duration as i32;
                    t3 = t4.wrapping_sub(t3);
                }

                if self.parsed & PARSED_FADE == 0 {
                    self.fade = interpolate_alpha(f.time - event.start, t1, t2, t3, t4, a1, a2, a3);
                    self.parsed |= PARSED_FADE;
                }
            } else if complex("org") {
                if args.len() != 2 {
                    p = rest_p;
                    continue;
                }

                if !self.have_origin {
                    self.org = (args[0].f64(), args[1].f64());
                    self.have_origin = true;
                    self.detect_collisions = false;
                }
            } else if complex("t") {
                let cnt = args.len() as i32 - 1;

                let (t1, mut t2, accel) = match cnt {
                    3 => (args[0].i32(), args[1].i32(), args[2].f64()),
                    2 => (dtoi32(args[0].f64()), dtoi32(args[1].f64()), 1.0),
                    1 => (0, 0, args[0].f64()),
                    _ => (0, 0, 1.0),
                };

                self.detect_collisions = false;

                if t2 == 0 {
                    t2 = event.duration as i32;
                }

                let delta = t2.wrapping_sub(t1);
                let t = (f.time - event.start) as i32;

                let k = if t < t1 {
                    0.0
                } else if t >= t2 {
                    1.0
                } else {
                    (t.wrapping_sub(t1) as f64 / delta as f64).powf(accel)
                };

                if nested {
                    pwr = k;
                }

                if !(0..=3).contains(&cnt) || !has_backslash_arg {
                    p = rest_p;
                    continue;
                }

                let inner = args[cnt as usize].0;
                let start = inner.as_ptr() as usize - s.as_ptr() as usize;
                let end = start + inner.len();

                if end < s.len() {
                    self.parse_tags(f, fonts, event, &s[start..end], k, true);
                } else {
                    // Nothing can follow: carry on in this loop instead of recursing.
                    pwr = k;
                    nested = true;
                    rest_p = start;
                }
            } else if complex("clip") {
                self.clip_tag(&args, pwr, false);
            } else if tag(name, "c", &mut args) || tag(name, "1c", &mut args) {
                self.color_tag(0, args.first().copied(), pwr);
            } else if tag(name, "2c", &mut args) {
                self.color_tag(1, args.first().copied(), pwr);
            } else if tag(name, "3c", &mut args) {
                self.color_tag(2, args.first().copied(), pwr);
            } else if tag(name, "4c", &mut args) {
                self.color_tag(3, args.first().copied(), pwr);
            } else if tag(name, "1a", &mut args) {
                self.alpha_index(0, args.first().copied(), pwr);
            } else if tag(name, "2a", &mut args) {
                self.alpha_index(1, args.first().copied(), pwr);
            } else if tag(name, "3a", &mut args) {
                self.alpha_index(2, args.first().copied(), pwr);
            } else if tag(name, "4a", &mut args) {
                self.alpha_index(3, args.first().copied(), pwr);
            } else if tag(name, "r", &mut args) {
                match args.first() {
                    Some(a) => {
                        let found = f.track.styles.iter().rev().find(|st| st.name == a.0).cloned();
                        self.reset(f, fonts, found.as_ref(), event);
                    },
                    None => self.reset(f, fonts, None, event),
                }
            } else if tag(name, "be", &mut args) {
                self.be = match args.first() {
                    // VSFilter always adds 0.5, even to negative values.
                    Some(a) => dtoi32(self.be as f64 * (1.0 - pwr) + a.f64() * pwr + 0.5).clamp(0, MAX_BE),
                    None => 0,
                };
            } else if tag(name, "b", &mut args) {
                let v = args.first().map(|a| a.i32()).unwrap_or(0);
                self.bold = if args.is_empty() || !(v == 0 || v == 1 || v >= 100) { style.bold } else { v };
                self.update_font(f, fonts);
            } else if tag(name, "i", &mut args) {
                let v = args.first().map(|a| a.i32()).unwrap_or(0);
                self.italic = if args.is_empty() || !(v == 0 || v == 1) { style.italic } else { v };
                self.update_font(f, fonts);
            } else if tag(name, "kt", &mut args) {
                let v = args.first().map(|a| a.f64() * 10.0).unwrap_or(0.0);
                self.effect_skip_timing = dtoi32(v);
                self.effect_timing = 0;
                self.reset_effect = true;
            } else if tag(name, "kf", &mut args) || tag(name, "K", &mut args) {
                self.karaoke(Effect::KaraokeKf, args.first().copied());
            } else if tag(name, "ko", &mut args) {
                self.karaoke(Effect::KaraokeKo, args.first().copied());
            } else if tag(name, "k", &mut args) {
                self.karaoke(Effect::Karaoke, args.first().copied());
            } else if tag(name, "shad", &mut args) {
                (self.shadow_x, self.shadow_y) = match args.first() {
                    // Clamped for \shad, but not for \xshad and \yshad, as in VSFilter.
                    Some(a) => {
                        let v = a.f64();
                        (mix(self.shadow_x, v).max(0.0), mix(self.shadow_y, v).max(0.0))
                    },
                    None => (style.shadow, style.shadow),
                };
            } else if tag(name, "s", &mut args) {
                let v = args.first().map(|a| a.i32()).unwrap_or(0);
                let on = if args.is_empty() || !(v == 0 || v == 1) { style.strike_out } else { v == 1 };
                self.flags = if on { self.flags | DECO_STRIKETHROUGH } else { self.flags & !DECO_STRIKETHROUGH };
            } else if tag(name, "u", &mut args) {
                let v = args.first().map(|a| a.i32()).unwrap_or(0);
                let on = if args.is_empty() || !(v == 0 || v == 1) { style.underline } else { v == 1 };
                self.flags = if on { self.flags | DECO_UNDERLINE } else { self.flags & !DECO_UNDERLINE };
            } else if tag(name, "pbo", &mut args) {
                self.pbo = args.first().map(|a| a.f64()).unwrap_or(0.0);
            } else if tag(name, "p", &mut args) {
                self.drawing_scale = args.first().map(|a| a.i32()).unwrap_or(0).max(0);
            } else if tag(name, "q", &mut args) {
                let v = args.first().map(|a| a.i32()).unwrap_or(-1);
                self.wrap_style = if args.is_empty() || !(0..=3).contains(&v) { f.track.wrap_style } else { v };
            } else if tag(name, "fe", &mut args) {
                self.font_encoding = args.first().map(|a| a.i32()).unwrap_or(style.encoding);
            }

            p = rest_p;
        }

        let _ = nested;
    }

    fn clip_tag(&mut self, args: &[Arg], pwr: f64, inverse: bool) {
        if args.len() == 4 {
            let v: Vec<i32> = args.iter().map(|a| a.i32()).collect();
            let mix = |old: i32, new: i32| (old as f64 * (1.0 - pwr) + new as f64 * pwr) as i32;
            self.clip[0] = mix(self.clip[0], v[0]);
            self.clip[2] = mix(self.clip[2], v[2]);
            self.clip[1] = mix(self.clip[1], v[1]);
            self.clip[3] = mix(self.clip[3], v[3]);
            self.clip_inverse = inverse;
        } else if self.clip_drawing.is_none() && (args.len() == 1 || args.len() == 2) {
            let scale = if args.len() == 2 { args[0].i32() } else { 1 };
            let text = args[args.len() - 1].0.to_string();
            self.clip_drawing = Some(ClipDrawing { text, scale, inverse });
        }
    }

    fn color_tag(&mut self, i: usize, arg: Option<Arg>, pwr: f64) {
        match arg {
            Some(a) => change_color(&mut self.c[i], color_tag(a.0), pwr),
            None => change_color(&mut self.c[i], self.style.colors[i], 1.0),
        }
    }

    fn alpha_index(&mut self, i: usize, arg: Option<Arg>, pwr: f64) {
        match arg {
            Some(a) => change_alpha(&mut self.c[i], alpha_tag(a.0), pwr),
            None => change_alpha(&mut self.c[i], (self.style.colors[i] & 0xFF) as i32, 1.0),
        }
    }

    fn karaoke(&mut self, effect: Effect, arg: Option<Arg>) {
        let v = arg.map(|a| a.f64()).unwrap_or(100.0);
        self.effect = effect;
        self.effect_skip_timing = self.effect_skip_timing.wrapping_add(self.effect_timing);
        self.effect_timing = dtoi32(v * 10.0);
    }

    /// The next character at `*p`, escapes resolved; 0 at the end.
    pub fn next_char(&self, s: &str, p: &mut usize) -> u32 {
        let rest = &s[*p..];
        let b = rest.as_bytes();

        if b.is_empty() {
            return 0;
        }

        if b[0] == b'\t' {
            *p += 1;
            return ' ' as u32;
        }

        if b[0] == b'\\' && b.len() > 1 {
            let c = match b[1] {
                b'N' => Some('\n' as u32),
                b'n' if self.wrap_style == 2 => Some('\n' as u32),
                b'n' => Some(' ' as u32),
                b'h' => Some(0xA0),
                b'{' => Some('{' as u32),
                b'}' => Some('}' as u32),
                _ => None,
            };

            if let Some(c) = c {
                *p += 2;
                return c;
            }
        }

        let c = rest.chars().next().unwrap();
        *p += c.len_utf8();
        c as u32
    }
}

/// Whether the tag `name` is `tagname`; what follows the name is its argument.
fn tag<'a>(name: &'a str, tagname: &str, args: &mut Vec<Arg<'a>>) -> bool {
    match name.strip_prefix(tagname) {
        Some(arg) => {
            push_arg(args, arg);
            true
        },
        None => false,
    }
}

/// A tag's argument, without trailing spaces; empty ones and those past the most any tag takes
/// don't count.
fn push_arg<'a>(args: &mut Vec<Arg<'a>>, a: &'a str) {
    let a = trim_end_spaces(a);

    if args.len() <= MAX_ARGS && !a.is_empty() {
        args.push(Arg(a));
    }
}

/// Converts like x86 does: out of range values become `i32::MIN`.
pub(crate) fn dtoi32(v: f64) -> i32 {
    if v.is_nan() || v <= i32::MIN as f64 || v >= i32::MAX as f64 + 1.0 { i32::MIN } else { v as i32 }
}

fn alpha_tag(s: &str) -> i32 {
    let s = s.trim_start_matches(['&', 'H']);
    text::strtoi32(s, 16).0
}

fn color_tag(s: &str) -> u32 {
    let s = s.trim_start_matches(['&', 'H']);
    (text::strtoi32(s, 16).0 as u32).swap_bytes()
}

fn anim(new: f64, old: f64, pwr: f64) -> i32 {
    dtoi32((1.0 - pwr) * old + new * pwr)
}

/// Moves each colour channel `pwr` of the way to `new`'s, alpha untouched.
fn change_color(var: &mut u32, new: u32, pwr: f64) {
    let (co, cn) = (var.swap_bytes(), new.swap_bytes());
    let ch = |m: u32| (anim((cn & m) as f64, (co & m) as f64, pwr) as u32) & m;
    let cc = ch(0xFF0000) | ch(0x00FF00) | ch(0x0000FF);
    *var = (cc & 0xFFFFFF).swap_bytes() | (*var & 0xFF);
}

pub(crate) fn change_alpha(var: &mut u32, new: i32, pwr: f64) {
    *var = (*var & 0xFFFFFF00) | (anim(new as f64, (*var & 0xFF) as f64, pwr) as u8 as u32);
}

fn mult_alpha(a: u32, b: u32) -> u32 {
    (a as u64 - (a as u64 * b as u64 + 0x7F) / 0xFF + b as u64) as u32
}

/// `\fad`'s alpha applied to a colour; VSFilter only does it when it's positive.
pub(crate) fn apply_fade(c: &mut u32, fade: i32) {
    if fade > 0 {
        change_alpha(c, mult_alpha(*c & 0xFF, fade as u32) as i32, 1.0);
    }
}

#[allow(clippy::too_many_arguments)]
fn interpolate_alpha(now: i64, t1: i32, t2: i32, t3: i32, t4: i32, a1: i32, a2: i32, a3: i32) -> i32 {
    let now32 = now as i32;

    if now < t1 as i64 {
        a1
    } else if now < t2 as i64 {
        let cf = now32.wrapping_sub(t1) as f64 / t2.wrapping_sub(t1) as f64;
        (a1 as f64 * (1.0 - cf) + a2 as f64 * cf) as i32
    } else if now < t3 as i64 {
        a2
    } else if now < t4 as i64 {
        let cf = now32.wrapping_sub(t3) as f64 / t4.wrapping_sub(t3) as f64;
        (a2 as f64 * (1.0 - cf) + a3 as f64 * cf) as i32
    } else {
        a3
    }
}

/// Whether the text positions itself or clips: then it's laid out in the video, not the frame.
fn has_hard_overrides(s: &str) -> bool {
    let b = s.as_bytes();
    let mut i = 0;

    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            i += 2;
        } else if b[i] == b'{' {
            i += 1;

            while i < b.len() && b[i] != b'}' {
                if b[i] == b'\\' {
                    let rest = &s[i + 1..];

                    if ["pos", "move", "clip", "iclip", "org", "pbo", "p"].iter().any(|t| rest.starts_with(t)) {
                        return true;
                    }
                }

                i += 1;
            }
        } else {
            i += 1;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors() {
        assert_eq!(color_tag("&H0000FF&"), 0xFF000000);
        let mut c = 0x00000000;
        change_color(&mut c, 0xFF000000, 0.5);
        assert_eq!(c, 0x7F000000);
        let mut c = 0xFFFFFF00;
        apply_fade(&mut c, 0x80);
        assert_eq!(c & 0xFF, 0x80);
    }

    #[test]
    fn fades() {
        assert_eq!(interpolate_alpha(0, 0, 200, 800, 1000, 255, 0, 255), 255);
        assert_eq!(interpolate_alpha(100, 0, 200, 800, 1000, 255, 0, 255), 127);
        assert_eq!(interpolate_alpha(500, 0, 200, 800, 1000, 255, 0, 255), 0);
    }

    #[test]
    fn hard_overrides() {
        assert!(has_hard_overrides("{\\pos(1,2)}a"));
        assert!(!has_hard_overrides("{\\b1}a\\N{\\i1}"));
    }
}
