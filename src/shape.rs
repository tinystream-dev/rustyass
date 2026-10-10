// SPDX-License-Identifier: LGPL-3.0-or-later
//! Shaping with HarfRust, and the bidirectional algorithm's levels and reordering.

use std::collections::HashMap;
use std::ops::Range;

use harfrust::{Buffer, Direction, Feature, FontFuncs, GlyphId, Language, Script, ShapeOptions, ShaperFont, Tag};
use unicode_bidi::{BidiClass, BidiInfo, Level, bidi_class};

use crate::font::Face;

/// A shaped glyph, in 26.6 at the size outlines are loaded at.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Shaped {
    pub cluster: usize,
    pub glyph: u32,
    pub x_advance: i32,
    pub y_advance: i32,
    pub x_offset: i32,
    pub y_offset: i32,
}

pub(crate) struct Shaper {
    buffer: Option<Buffer>,
    scripts: HashMap<u32, Option<Script>>,
}

/// Metrics as libass hands them to HarfBuzz: advances rounded to whole pixels.
struct Metrics<'a> {
    face: &'a Face,
    vertical: bool,
}

impl FontFuncs for Metrics<'_> {
    fn nominal_glyph(&self, _: &ShaperFont, c: u32) -> Option<GlyphId> {
        match self.face.glyph(c) {
            0 => None,
            g => Some(GlyphId::new(g)),
        }
    }

    fn glyph_h_advance(&self, _: &ShaperFont, glyph: GlyphId) -> i32 {
        (self.face.advance(glyph.to_u32(), self.vertical) * 64.0) as i32
    }
}

pub(crate) struct Run<'a> {
    pub text: &'a [u32],
    pub range: Range<usize>,
    pub rtl: bool,
    pub script: Option<Script>,
    pub language: Option<&'a str>,
    pub kerning: bool,
    pub ligatures: bool,
    pub vertical: bool,
    /// Whether the characters just outside the run are shaping context for it.
    pub context: (bool, bool),
}

impl Shaper {
    pub fn new() -> Shaper {
        Shaper { buffer: Some(Buffer::new()), scripts: HashMap::new() }
    }

    /// The character's script, None when it takes its neighbours' (common or inherited).
    pub fn script(&mut self, c: u32) -> Option<Script> {
        if let Some(&s) = self.scripts.get(&c) {
            return s;
        }

        let mut buf = self.buffer.take().unwrap_or_default();
        buf.clear();
        buf.push(c, 0);
        buf.guess_segment_properties();
        let s = buf.script();
        buf.clear();
        self.buffer = Some(buf);
        self.scripts.insert(c, s);
        s
    }

    pub fn shape(&mut self, face: &Face, run: &Run) -> Vec<Shaped> {
        let mut buf = self.buffer.take().unwrap_or_default();
        buf.clear();

        let (start, end) = (run.range.start, run.range.end);

        if run.context.0 && start > 0 {
            buf.set_pre_context_codepoints(&run.text[start - 1..start]);
        }

        if run.context.1 && end < run.text.len() {
            buf.set_post_context_codepoints(&run.text[end..end + 1]);
        }

        for (i, &c) in run.text[run.range.clone()].iter().enumerate() {
            buf.push(c, i as u32);
        }

        buf.set_direction(if run.rtl { Direction::RightToLeft } else { Direction::LeftToRight });
        buf.set_script(run.script);
        buf.set_language(run.language.and_then(|l| l.parse::<Language>().ok()));
        buf.guess_segment_properties();

        let vertical = run.vertical as u32;
        let liga = run.ligatures as u32;

        let features = [
            Feature::new(Tag::new(b"vert"), vertical, ..),
            Feature::new(Tag::new(b"vkna"), vertical, ..),
            Feature::new(Tag::new(b"kern"), run.kerning as u32, ..),
            Feature::new(Tag::new(b"liga"), liga, ..),
            Feature::new(Tag::new(b"clig"), liga, ..),
        ];

        let upem = face.upem();
        let scale = (upem * face.scale() * 64.0).round() as i32;
        let metrics = Metrics { face, vertical: run.vertical };
        let font = ShaperFont::new(&face.shaping).with_scale(scale).with_font_funcs(Some(&metrics));
        let mut out = Vec::new();

        if harfrust::shape(&font, &mut buf, ShapeOptions::new().features(&features)).is_ok() {
            for (info, pos) in buf.glyph_infos().iter().zip(buf.glyph_positions()) {
                out.push(Shaped {
                    cluster: info.cluster as usize,
                    glyph: info.glyph_id,
                    x_advance: pos.x_advance,
                    y_advance: pos.y_advance,
                    x_offset: pos.x_offset,
                    y_offset: pos.y_offset,
                });
            }
        }

        buf.clear();
        self.buffer = Some(buf);
        out
    }
}

/// Embedding levels of a stretch of text, its paragraph being left to right unless `auto`.
pub(crate) fn levels(text: &[u32], auto: bool) -> (Vec<u8>, u8) {
    let s: String = text.iter().map(|&c| char::from_u32(c).unwrap_or('\u{FFFD}')).collect();
    let info = BidiInfo::new(&s, if auto { None } else { Some(Level::ltr()) });
    let base = info.paragraphs.first().map(|p| p.level.number()).unwrap_or(0);
    let levels = s.char_indices().map(|(i, _)| info.levels[i].number()).collect();
    (levels, base)
}

pub(crate) fn is_paragraph_separator(c: u32) -> bool {
    char::from_u32(c).is_some_and(|c| bidi_class(c) == BidiClass::B)
}

/// Visual order of a line, given its characters' levels: whitespace at the end goes back to the
/// paragraph's level, then runs are reversed from the highest level down.
pub(crate) fn reorder(text: &[u32], levels: &[u8], base: u8, out: &mut [usize]) {
    let n = text.len();
    let mut lv = levels.to_vec();
    let class = |c: u32| char::from_u32(c).map(bidi_class).unwrap_or(BidiClass::ON);
    let mut trailing = true;

    for i in (0..n).rev() {
        let c = class(text[i]);

        match c {
            BidiClass::S | BidiClass::B => {
                lv[i] = base;
                trailing = true;
            },
            BidiClass::WS | BidiClass::FSI | BidiClass::LRI | BidiClass::RLI | BidiClass::PDI if trailing => {
                lv[i] = base;
            },
            _ => trailing = false,
        }
    }

    for (i, o) in out.iter_mut().enumerate() {
        *o = i;
    }

    let max = lv.iter().copied().max().unwrap_or(0);
    let min_odd = lv.iter().copied().filter(|l| l % 2 == 1).min().unwrap_or(max + 1);

    let mut level = max;

    while level >= min_odd && level > 0 {
        let mut i = 0;

        while i < n {
            if lv[i] >= level {
                let start = i;

                while i < n && lv[i] >= level {
                    i += 1;
                }

                out[start..i].reverse();
                lv[start..i].reverse();
            } else {
                i += 1;
            }
        }

        level -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorders_hebrew() {
        let text: Vec<u32> = "ab \u{05D0}\u{05D1} c".chars().map(|c| c as u32).collect();
        let (levels, base) = levels(&text, false);
        assert_eq!(base, 0);
        assert_eq!(&levels[..3], &[0, 0, 0]);
        assert_eq!(levels[3], 1);

        let mut order = vec![0; text.len()];
        reorder(&text, &levels, base, &mut order);
        assert_eq!(order, vec![0, 1, 2, 4, 3, 5, 6]);
    }
}
