// SPDX-License-Identifier: LGPL-3.0-or-later
//! Fonts: faces from memory or a directory, chosen for a family, weight and slant the way VSFilter
//! (through GDI) chooses them, and their glyphs at the size libass scales outlines from.

use std::collections::HashMap;
use std::path::Path as FsPath;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::{fs, io};

use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::{FileRef, FontRef, TableProvider};
use skrifa::{GlyphId, MetadataProvider};

use crate::outline::{Path, Pt, Seg, pt};

/// The size outlines are loaded at, as libass does when it isn't hinting.
pub(crate) const OUTLINE_SIZE: f64 = 256.0;

pub(crate) const DECO_UNDERLINE: u32 = 1;
pub(crate) const DECO_STRIKETHROUGH: u32 = 2;
pub(crate) const DECO_ROTATE: u32 = 4;

/// Fonts to render with: attachments, a directory of them, and what to fall back on.
#[derive(Clone, Default)]
pub struct Fonts {
    faces: Vec<Arc<Face>>,
    default_family: Option<String>,
    default: Option<Arc<Face>>,
    pub(crate) generation: u64,
}

pub(crate) struct Face {
    pub uid: u64,
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    index: u32,
    pub shaping: harfrust::Font,
    families: Vec<String>,
    fullnames: Vec<String>,
    postscript_name: Option<String>,
    pub weight: i32,
    pub italic: bool,
    pub bold: bool,
    pub postscript: bool,
    /// Ascender and descender as GDI has them, in font units; the descender positive.
    pub asc: f64,
    pub desc: f64,
    underline: Option<(f64, f64)>,
    strikeout: Option<(f64, f64)>,
    typo_desc: f64,
    chars: Mutex<HashMap<u32, u32>>,
}

/// A glyph's outline at [`OUTLINE_SIZE`], in pixels with y down, and its metrics.
pub(crate) struct Glyph {
    pub path: Path,
    pub advance: f64,
    pub asc: f64,
    pub desc: f64,
}

static UID: AtomicU64 = AtomicU64::new(1);

impl Fonts {
    pub fn new() -> Fonts {
        Fonts::default()
    }

    /// Adds every face in `data` (a font or a collection); returns how many there were.
    pub fn add(&mut self, data: impl Into<Vec<u8>>) -> usize {
        let faces = Face::load(Arc::new(data.into()));
        let n = faces.len();
        self.faces.extend(faces);
        self.generation += 1;
        n
    }

    /// Adds every font in `dir`, not looking into subdirectories.
    pub fn add_dir(&mut self, dir: &FsPath) -> io::Result<usize> {
        let mut n = 0;

        for entry in fs::read_dir(dir)? {
            let entry = entry?;

            if entry.file_name().to_string_lossy().starts_with('.') || !entry.file_type()?.is_file() {
                continue;
            }

            if let Ok(data) = fs::read(entry.path()) {
                n += self.add(data);
            }
        }

        Ok(n)
    }

    /// The family to use when a script's isn't there, or lacks a character.
    pub fn set_default_family(&mut self, family: Option<&str>) {
        self.default_family = family.map(str::to_string);
        self.generation += 1;
    }

    /// The font used when nothing else matches, whatever it covers.
    pub fn set_default(&mut self, data: impl Into<Vec<u8>>) {
        self.default = Face::load(Arc::new(data.into())).into_iter().next();
        self.generation += 1;
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty() && self.default.is_none()
    }

    /// The face for `family` that best matches `weight` and `italic` and has `code` (0 for any).
    pub(crate) fn select(&self, family: &str, weight: u32, italic: u32, code: u32) -> Option<Arc<Face>> {
        if !family.is_empty()
            && let Some(f) = self.find(family, weight, italic, code)
        {
            return Some(f);
        }

        if let Some(d) = &self.default_family
            && let Some(f) = self.find(d, weight, italic, code)
        {
            return Some(f);
        }

        self.default.clone()
    }

    fn find(&self, name: &str, weight: u32, italic: u32, code: u32) -> Option<Arc<Face>> {
        let mut best = u32::MAX;
        let mut selected = None;

        for face in &self.faces {
            let score = if face.families.iter().any(|f| f.eq_ignore_ascii_case(name)) {
                face.similarity(weight, italic)
            } else if face.matches_full_name(name) {
                0
            } else {
                continue;
            };

            if score < best {
                if code != 0 && face.glyph(code) == 0 {
                    continue;
                }

                best = score;
                selected = Some(face.clone());
            }

            if score == 0 {
                break;
            }
        }

        selected
    }
}

impl Face {
    fn load(data: Arc<Vec<u8>>) -> Vec<Arc<Face>> {
        let data: Arc<dyn AsRef<[u8]> + Send + Sync> = data;

        let count = match FileRef::new(data.as_ref().as_ref()) {
            Ok(FileRef::Font(_)) => 1,
            Ok(FileRef::Collection(c)) => c.len(),
            Err(_) => 0,
        };

        (0..count).filter_map(|i| Face::open(data.clone(), i).map(Arc::new)).collect()
    }

    fn open(data: Arc<dyn AsRef<[u8]> + Send + Sync>, index: u32) -> Option<Face> {
        let bytes: &[u8] = data.as_ref().as_ref();
        let font = FontRef::from_index(bytes, index).ok()?;

        // Only outlines.
        if font.glyf().is_err() && font.cff().is_err() && font.cff2().is_err() {
            return None;
        }

        let (mut families, mut fullnames) = (Vec::new(), Vec::new());
        let mut postscript_name = None;

        if let Ok(name) = font.name() {
            let strings = name.string_data();

            for record in name.name_record() {
                let id = record.name_id().to_u16();

                let Ok(s) = record.string(strings) else { continue };

                if id == 6 && postscript_name.is_none() {
                    postscript_name = Some(s.chars().collect::<String>());
                }

                if record.platform_id() != 3 {
                    continue;
                }

                match id {
                    1 => families.push(s.chars().collect()),
                    4 => fullnames.push(s.chars().collect()),
                    _ => {},
                }
            }

            if families.is_empty() {
                let any = name.name_record().iter().find(|r| r.name_id().to_u16() == 1);

                if let Some(s) = any.and_then(|r| r.string(strings).ok()) {
                    families.push(s.chars().collect());
                }
            }
        }

        if families.is_empty() {
            return None;
        }

        let os2 = font.os2().ok();
        let head = font.head().ok()?;

        let (italic, bold) = match &os2 {
            Some(os2) => {
                let s = os2.fs_selection().bits();
                (s & 1 != 0, s & (1 << 5) != 0)
            },
            None => {
                let s = head.mac_style().bits();
                (s & 2 != 0, s & 1 != 0)
            },
        };

        let weight = match os2.as_ref().map(|o| o.us_weight_class()).unwrap_or(0) {
            0 => 400 + 300 * bold as i32,
            1 => 100,
            2 => 200,
            3 => 300,
            4 => 350,
            5 => 400,
            6 => 600,
            7 => 700,
            8 => 800,
            9 => 900,
            w => w as i32,
        };

        let (asc, desc) = metrics(&font);

        let underline = font
            .post()
            .ok()
            .map(|p| (p.underline_position().to_i16() as f64, p.underline_thickness().to_i16() as f64))
            .filter(|&(pos, size)| pos <= 0.0 && size > 0.0);

        let strikeout = os2
            .as_ref()
            .map(|o| (o.y_strikeout_position() as f64, o.y_strikeout_size() as f64))
            .filter(|&(pos, size)| pos >= 0.0 && size > 0.0);

        let typo_desc = os2.as_ref().map(|o| o.s_typo_descender() as f64).unwrap_or(0.0);
        let postscript = font.cff().is_ok();
        let shaping = harfrust::Font::new(harfrust::font::Blob::from(data.clone()), index)?;

        Some(Face {
            uid: UID.fetch_add(1, Ordering::Relaxed),
            data,
            index,
            shaping,
            families,
            fullnames,
            postscript_name,
            weight,
            italic,
            bold,
            postscript,
            asc,
            desc,
            underline,
            strikeout,
            typo_desc,
            chars: Mutex::new(HashMap::new()),
        })
    }

    pub fn font(&self) -> FontRef<'_> {
        FontRef::from_index(self.data.as_ref().as_ref(), self.index).expect("opened before")
    }

    fn matches_full_name(&self, name: &str) -> bool {
        let full = self.fullnames.iter().any(|f| f.eq_ignore_ascii_case(name));
        let ps = self.postscript_name.as_deref().is_some_and(|p| p.eq_ignore_ascii_case(name));

        match (full, ps) {
            (a, b) if a == b => a,
            _ if self.postscript => ps,
            _ => full,
        }
    }

    /// How far the face is from a weight and slant; lower is closer.
    fn similarity(&self, weight: u32, italic: u32) -> u32 {
        let mut score = match (italic != 0, self.italic) {
            (true, false) => 1,
            (false, true) => 4,
            _ => 0,
        };

        let mut w = self.weight;

        // Faux bold makes a face heavier.
        if weight as i32 > self.weight + 150 && !self.bold {
            w += 120;
        }

        score += (73 * (w - weight as i32).unsigned_abs()) / 256;
        score
    }

    /// The glyph for a character, 0 when there's none.
    pub fn glyph(&self, code: u32) -> u32 {
        let mut chars = self.chars.lock().unwrap();

        *chars.entry(code).or_insert_with(|| self.font().charmap().map(code).map(|g| g.to_u32()).unwrap_or(0))
    }

    /// Pixels at [`OUTLINE_SIZE`] per font unit: the size is the height from descender to
    /// ascender, as VSFilter sizes fonts.
    pub fn scale(&self) -> f64 {
        OUTLINE_SIZE / (self.asc + self.desc)
    }

    pub fn upem(&self) -> f64 {
        self.font().head().map(|h| h.units_per_em()).unwrap_or(1000) as f64
    }

    /// A glyph's advance at [`OUTLINE_SIZE`], rounded to a pixel as FreeType rounds hinted ones.
    pub fn advance(&self, gid: u32, vertical: bool) -> f64 {
        let font = self.font();

        let units = if vertical {
            font.vmtx()
                .ok()
                .and_then(|v| v.advance(GlyphId::new(gid)))
                .map(|a| a as f64)
                .unwrap_or(self.asc + self.desc)
        } else {
            font.glyph_metrics(Size::unscaled(), LocationRef::default()).advance_width(GlyphId::new(gid)).unwrap_or(0.0)
                as f64
        };

        (units * self.scale()).round()
    }

    /// The glyph's outline, made bold or italic when the face isn't and the request is, with
    /// underline and strike-through in `deco`.
    pub fn outline(&self, gid: u32, embolden: bool, italicize: bool, deco: u32) -> Option<Glyph> {
        let font = self.font();
        let glyph = font.outline_glyphs().get(GlyphId::new(gid))?;
        let mut pen = Pen::default();
        glyph.draw(DrawSettings::unhinted(Size::unscaled(), LocationRef::default()), &mut pen).ok()?;
        let k = self.scale();
        let vertical = deco & DECO_ROTATE != 0;
        let advance = self.advance(gid, vertical);

        // Font units, y up.
        let mut contours = pen.contours;

        if italicize && !self.italic {
            let slant = if self.postscript { 0x02D24 as f64 / 65536.0 } else { 0x05700 as f64 / 65536.0 };

            for c in &mut contours {
                for p in &mut c.points {
                    p.x += p.y * slant;
                }
            }
        }

        if embolden && !self.bold {
            // FreeType's FT_Outline_Embolden with the em size over 64 as the strength.
            let strength = self.upem() / 64.0;
            embolden_contours(&mut contours, strength, strength);
        }

        let mut path = Path::default();

        for c in &contours {
            c.append(&mut path, |p| pt(p.x * k, -p.y * k));
        }

        let clockwise = path.area() >= 0.0;

        if vertical {
            let desc = self.typo_desc * k;
            let dv = (self.asc + self.desc) * k + desc;
            path = path.map(|p| pt(p.y + dv, -p.x - desc));
        }

        if advance > 0.0 {
            for (on, line) in [(DECO_UNDERLINE, self.underline), (DECO_STRIKETHROUGH, self.strikeout)] {
                if deco & on == 0 {
                    continue;
                }

                if let Some((pos, size)) = line {
                    let (pos, size) = ((pos * k * 64.0).round(), (size * k * 64.0).round());
                    let top = (-pos - (size as i64 >> 1) as f64) / 64.0;
                    path.add_rect(0.0, top, advance, top + size / 64.0, clockwise);
                }
            }
        }

        Some(Glyph { path, advance, asc: self.asc * k, desc: self.desc * k })
    }
}

/// GDI's ascender and descender: the OS/2 table's Windows metrics, or failing those whatever is
/// there.
fn metrics(font: &FontRef) -> (f64, f64) {
    let os2 = font.os2().ok();

    if let Some(o) = &os2 {
        let (a, d) = (o.us_win_ascent() as i16 as f64, o.us_win_descent() as i16 as f64);

        if a + d != 0.0 {
            return (a, d);
        }
    }

    if let Ok(h) = font.hhea() {
        let (a, d) = (h.ascender().to_i16() as f64, h.descender().to_i16() as f64);

        if a - d != 0.0 {
            return (a, -d);
        }
    }

    if let Some(o) = &os2 {
        let (a, d) = (o.s_typo_ascender() as f64, o.s_typo_descender() as f64);

        if a - d != 0.0 {
            return (a, -d);
        }
    }

    match font.head() {
        Ok(h) if h.y_max() != h.y_min() => (h.y_max() as f64, -(h.y_min() as f64)),
        _ => (800.0, 200.0),
    }
}

/// A contour as FreeType has it: its points, and which ones end each segment.
#[derive(Default, Clone)]
struct Contour {
    points: Vec<Pt>,
    /// For each point after the first: the segment it ends, if any.
    segs: Vec<Option<Seg>>,
}

impl Contour {
    fn append(&self, path: &mut Path, f: impl Fn(Pt) -> Pt) {
        let n = self.points.len();

        if n < 2 {
            return;
        }

        path.push(f(self.points[0]), None);

        for i in 1..n {
            path.push(f(self.points[i]), self.segs[i]);
        }

        // The contour comes back to its start: that's implied.
        if self.points[n - 1] == self.points[0] && self.segs[n - 1].is_some() {
            path.points.pop();
        } else {
            path.segments.push((Seg::Line, false));
        }

        path.close();
    }
}

#[derive(Default)]
struct Pen {
    contours: Vec<Contour>,
    current: Contour,
}

impl Pen {
    fn finish(&mut self) {
        let c = std::mem::take(&mut self.current);

        if c.points.len() > 1 {
            self.contours.push(c);
        }
    }
}

impl OutlinePen for Pen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.finish();
        self.current.points.push(pt(x as f64, y as f64));
        self.current.segs.push(None);
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.current.points.push(pt(x as f64, y as f64));
        self.current.segs.push(Some(Seg::Line));
    }

    fn quad_to(&mut self, cx0: f32, cy0: f32, x: f32, y: f32) {
        self.current.points.push(pt(cx0 as f64, cy0 as f64));
        self.current.segs.push(None);
        self.current.points.push(pt(x as f64, y as f64));
        self.current.segs.push(Some(Seg::Quad));
    }

    fn curve_to(&mut self, cx0: f32, cy0: f32, cx1: f32, cy1: f32, x: f32, y: f32) {
        self.current.points.push(pt(cx0 as f64, cy0 as f64));
        self.current.segs.push(None);
        self.current.points.push(pt(cx1 as f64, cy1 as f64));
        self.current.segs.push(None);
        self.current.points.push(pt(x as f64, y as f64));
        self.current.segs.push(Some(Seg::Cubic));
    }

    fn close(&mut self) {
        self.finish();
    }
}

/// FreeType's `FT_Outline_EmboldenXY`: each point moves out along the bisector of its edges,
/// and the whole outline up and right by half the strength. Coordinates are y up.
fn embolden_contours(contours: &mut [Contour], xstrength: f64, ystrength: f64) {
    let mut area = 0.0;

    for c in contours.iter() {
        let n = c.points.len();

        for i in 0..n {
            let (a, b) = (c.points[i], c.points[(i + 1) % n]);
            area += a.x * b.y - b.x * a.y;
        }
    }

    if area == 0.0 {
        return;
    }

    // TrueType contours run clockwise (negative area with y up).
    let truetype = area < 0.0;
    let (xs, ys) = (xstrength / 2.0, ystrength / 2.0);

    for c in contours.iter_mut() {
        let pts = &mut c.points;
        let n = pts.len();

        if n < 2 {
            continue;
        }

        let last = n - 1;
        let unit = |v: Pt| {
            let l = (v.x * v.x + v.y * v.y).sqrt();
            if l == 0.0 { (pt(0.0, 0.0), 0.0) } else { (v * (1.0 / l), l) }
        };

        let (mut inv, mut l_in) = (pt(0.0, 0.0), 0.0f64);
        let (mut anchor, mut l_anchor) = (pt(0.0, 0.0), 0.0);
        let mut i = last;
        let mut j = 0;
        let mut k: Option<usize> = None;

        while j != i && Some(i) != k {
            let (out, l_out) = if Some(j) != k {
                let (o, l) = unit(pts[j] - pts[i]);

                if l == 0.0 {
                    j = if j < last { j + 1 } else { 0 };
                    continue;
                }

                (o, l)
            } else {
                (anchor, l_anchor)
            };

            if inv.x != 0.0 || inv.y != 0.0 {
                let d = inv.x * out.x + inv.y * out.y;

                let shift = if d > -(0xF000 as f64 / 65536.0) {
                    let d = d + 1.0;
                    let mut sx = inv.y + out.y;
                    let mut sy = inv.x + out.x;

                    if truetype {
                        sx = -sx;
                    } else {
                        sy = -sy;
                    }

                    let mut q = out.x * inv.y - out.y * inv.x;

                    if truetype {
                        q = -q;
                    }

                    let l = l_in.min(l_out);
                    let sx = if xs * q <= l * d { sx * xs / d } else { sx * l / q };
                    let sy = if ys * q <= l * d { sy * ys / d } else { sy * l / q };
                    pt(sx, sy)
                } else {
                    pt(0.0, 0.0)
                };

                while i != j {
                    pts[i].x += xs + shift.x;
                    pts[i].y += ys + shift.y;
                    i = if i < last { i + 1 } else { 0 };
                }
            } else {
                k = Some(j);
                anchor = out;
                l_anchor = l_out;
            }

            inv = out;
            l_in = l_out;
            j = if j < last { j + 1 } else { 0 };
        }
    }
}

/// What a script asks for: a family at a weight and slant, maybe vertical (`@`).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct FontDesc {
    pub family: String,
    pub weight: u32,
    pub italic: u32,
    pub vertical: bool,
}

/// The faces a request has resolved to: the best match, then whatever had characters it lacked.
pub(crate) struct Font {
    pub desc: FontDesc,
    pub faces: Vec<Arc<Face>>,
}

const MAX_FACES: usize = 10;

#[derive(Default)]
pub(crate) struct FontCache {
    fonts: Vec<Font>,
    index: HashMap<FontDesc, usize>,
}

impl FontCache {
    /// The font for a request, None when no face at all fits it.
    pub fn get(&mut self, fonts: &Fonts, family: &str, weight: u32, italic: u32, vertical: bool) -> Option<usize> {
        let desc = FontDesc { family: family.to_string(), weight, italic, vertical };

        let i = match self.index.get(&desc) {
            Some(&i) => i,
            None => {
                let faces = fonts.select(family, weight, italic, 0).into_iter().collect();
                self.fonts.push(Font { desc: desc.clone(), faces });
                self.index.insert(desc, self.fonts.len() - 1);
                self.fonts.len() - 1
            },
        };

        (!self.fonts[i].faces.is_empty()).then_some(i)
    }

    pub fn font(&self, i: usize) -> &Font {
        &self.fonts[i]
    }

    /// The face that has `symbol`, adding one that does when none so far has, and the glyph.
    pub fn glyph(&mut self, fonts: &Fonts, font: usize, symbol: u32) -> (usize, u32) {
        if symbol < 0x20 {
            return (0, 0);
        }

        let f = &mut self.fonts[font];

        for (i, face) in f.faces.iter().enumerate() {
            match face.glyph(symbol) {
                0 => continue,
                g => return (i, g),
            }
        }

        if f.faces.len() >= MAX_FACES {
            return (0, 0);
        }

        match fonts.select(&f.desc.family, f.desc.weight, f.desc.italic, symbol) {
            Some(face) => match f.faces.iter().position(|x| x.uid == face.uid) {
                Some(i) => (i, 0),
                None => {
                    let g = face.glyph(symbol);
                    f.faces.push(face);
                    (f.faces.len() - 1, g)
                },
            },
            None => (0, 0),
        }
    }

    pub fn clear(&mut self) {
        self.fonts.clear();
        self.index.clear();
    }
}
