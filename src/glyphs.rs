// SPDX-License-Identifier: LGPL-3.0-or-later
//! From outlines to masks: glyphs and drawings transformed, bordered and rasterized, then a
//! run's masks added together, blurred and shadowed. Every step is cached.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::bitmap::{self, Bitmap};
use crate::event::{Frame, Glyph, Resources};
use crate::outline::{self, Path, Pt, Rect, pt};
use crate::state::{ClipDrawing, State};
use crate::{Image, ImageKind, blur, raster, stroke};

/// How far curves' control points may be from their flattened lines, in 64ths of a pixel.
const RASTERIZER_PRECISION: i64 = 16;
/// How much nearer than the text's own distance perspective may bring a point.
const MAX_PERSP_SCALE: f64 = 16.0;
const BLUR_PRECISION: f64 = 1.0 / 256.0;
const POSITION_PRECISION: f64 = 8.0;

/// A cached outline: a glyph at the size outlines are loaded at, or a drawing in its units.
#[derive(Debug)]
pub(crate) struct OutlineEntry {
    pub id: u64,
    pub path: Path,
    pub cbox: Rect,
    pub advance: f64,
    pub asc: f64,
    pub desc: f64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum OutlineKey {
    Glyph { face: u64, glyph: u32, embolden: bool, italicize: bool, deco: u32 },
    Drawing(String),
    Box,
}

/// What gets rasterized: an outline, or a border's two offsets.
enum Shape {
    Path(Arc<OutlineEntry>),
    Lines(Arc<Stroked>),
}

struct Stroked {
    id: u64,
    sides: [Vec<Vec<Pt>>; 2],
    cbox: Rect,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct StrokeKey {
    outline: u64,
    scale: (i64, i64),
    offset: (i64, i64),
    border: (i64, i64),
    tolerance: i32,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct RasterKey {
    source: u64,
    matrix: [i64; 4],
    persp: [i64; 2],
    frac: (u8, u8),
    /// What of the mask was drawn, relative to its origin, when it isn't all of it.
    clip: Option<[i32; 4]>,
}

struct Raster {
    bitmap: Option<Arc<Bitmap>>,
    pos: (i32, i32),
    residual: Pt,
}

/// A glyph's masks in a run, `pos` being where their origins go.
#[derive(Clone)]
pub(crate) struct BitmapRef {
    pub fill: Option<Arc<Bitmap>>,
    pub border: Option<Arc<Bitmap>>,
    pub pos: (i32, i32),
    pub pos_o: (i32, i32),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct Filter {
    pub flags: u32,
    pub be: i32,
    pub blur_x: i32,
    pub blur_y: i32,
    pub shadow: (i32, i32),
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct CompositeKey {
    filter: Filter,
    refs: Vec<(Held, Held, (i32, i32), (i32, i32))>,
}

/// A mask told apart by its address, which holding it keeps from going to another mask.
#[derive(Clone)]
struct Held(Option<Arc<Bitmap>>);

impl PartialEq for Held {
    fn eq(&self, other: &Held) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (a, b) => a.is_none() && b.is_none(),
        }
    }
}

impl Eq for Held {}

impl Hash for Held {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.as_ref().map_or(0, |b| Arc::as_ptr(b) as usize).hash(state);
    }
}

/// A run's masks, positioned relative to the run.
#[derive(Default)]
pub(crate) struct Composite {
    pub fill: Option<Arc<Bitmap>>,
    pub border: Option<Arc<Bitmap>>,
    pub shadow: Option<Arc<Bitmap>>,
}

struct Entry<V> {
    value: V,
    size: usize,
    used: u64,
}

/// A cache trimmed back to its budget by dropping what was used longest ago.
pub(crate) struct Cache<K, V> {
    map: FxHashMap<K, Entry<V>>,
    size: usize,
    limit: usize,
}

impl<K: Hash + Eq + Clone, V: Clone> Cache<K, V> {
    fn new(limit: usize) -> Self {
        Cache { map: FxHashMap::default(), size: 0, limit }
    }

    fn get(&mut self, k: &K, frame: u64) -> Option<V> {
        let e = self.map.get_mut(k)?;
        e.used = frame;
        Some(e.value.clone())
    }

    fn insert(&mut self, k: K, v: V, size: usize, frame: u64) {
        self.size += size;

        if let Some(old) = self.map.insert(k, Entry { value: v, size, used: frame }) {
            self.size -= old.size;
        }
    }

    /// Drops what was used longest ago, but nothing the last frame used: like libass, which keeps
    /// what's on screen, so what's bigger than the budget isn't made again every frame.
    fn trim(&mut self, frame: u64) {
        if self.size <= self.limit {
            return;
        }

        let mut entries: Vec<(u64, K)> = self.map.iter().map(|(k, e)| (e.used, k.clone())).collect();
        entries.sort_by_key(|e| e.0);

        for (used, k) in entries {
            if self.size <= self.limit * 3 / 4 || used + 1 >= frame {
                break;
            }

            if let Some(e) = self.map.remove(&k) {
                self.size -= e.size;
            }
        }
    }

    fn clear(&mut self) {
        self.map.clear();
        self.size = 0;
    }
}

const MB: usize = 1 << 20;

pub(crate) struct Caches {
    outlines: Cache<OutlineKey, Option<Arc<OutlineEntry>>>,
    strokes: Cache<StrokeKey, Option<Arc<Stroked>>>,
    bitmaps: Cache<RasterKey, Option<Arc<Bitmap>>>,
    composites: Cache<CompositeKey, Arc<Composite>>,
    next_id: u64,
    pub frame: u64,
}

impl Caches {
    pub fn new() -> Caches {
        Caches {
            outlines: Cache::new(10000),
            strokes: Cache::new(32 * MB),
            bitmaps: Cache::new(128 * MB),
            composites: Cache::new(64 * MB),
            next_id: 1,
            frame: 0,
        }
    }

    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Starts a frame, dropping what doesn't fit.
    pub fn start_frame(&mut self) {
        self.frame += 1;
        self.composites.trim(self.frame);
        self.bitmaps.trim(self.frame);
        self.strokes.trim(self.frame);
        self.outlines.trim(self.frame);
    }

    /// Forgets everything drawn for the old frame size.
    pub fn clear_bitmaps(&mut self) {
        self.composites.clear();
        self.bitmaps.clear();
        self.strokes.clear();
    }

    pub fn clear(&mut self) {
        self.clear_bitmaps();
        self.outlines.clear();
    }

    fn outline(
        &mut self,
        key: OutlineKey,
        make: impl FnOnce() -> Option<(Path, Rect, f64, f64, f64)>,
    ) -> Option<Arc<OutlineEntry>> {
        let frame = self.frame;

        if let Some(v) = self.outlines.get(&key, frame) {
            return v;
        }

        let v = make().map(|(path, cbox, advance, asc, desc)| {
            // libass keeps outlines in 64ths of a pixel.
            let d6 = |v: f64| (v * 64.0).round_ties_even() / 64.0;
            let path = path.map(|p| pt(d6(p.x), d6(p.y)));
            let cbox = if cbox.is_empty() {
                Rect::default()
            } else {
                Rect { x0: d6(cbox.x0), y0: d6(cbox.y0), x1: d6(cbox.x1), y1: d6(cbox.y1) }
            };
            Arc::new(OutlineEntry { id: self.id(), path, cbox, advance, asc, desc })
        });

        self.outlines.insert(key, v.clone(), 1, frame);
        v
    }

    fn stroke(&mut self, key: StrokeKey, make: impl FnOnce() -> [Vec<Vec<Pt>>; 2]) -> Option<Arc<Stroked>> {
        let frame = self.frame;

        if let Some(v) = self.strokes.get(&key, frame) {
            return v;
        }

        let sides = make();
        let mut cbox = Rect::EMPTY;

        for p in sides.iter().flatten().flatten() {
            cbox.add(*p);
        }

        let size = sides.iter().flatten().map(|l| l.len() * 16).sum::<usize>() + 64;
        let v = (!cbox.is_empty()).then(|| Arc::new(Stroked { id: self.id(), sides, cbox }));
        self.strokes.insert(key, v.clone(), size, frame);
        v
    }

    /// The mask of `shape` under `m` (None when it draws nothing), where its origin goes, and
    /// what rounding the position left over; None when the transform is unusable. Only what's
    /// within `view` on screen, if given, is drawn.
    fn raster(
        &mut self,
        shape: &Shape,
        m: &[[f64; 3]; 3],
        delta: Option<Pt>,
        view: Option<[i32; 4]>,
    ) -> Option<Raster> {
        let (id, cbox) = match shape {
            Shape::Path(o) => (o.id, o.cbox),
            Shape::Lines(s) => (s.id, s.cbox),
        };

        let q = Quantized::new(m, &cbox, delta)?;
        let clip = view.and_then(|[l, t, r, b]| {
            let local = [l - q.pos.0, t - q.pos.1, r - q.pos.0, b - q.pos.1];
            // Masks that the corners say fit aren't cut, so they're shared wherever they are.
            let corners = [(cbox.x0, cbox.y0), (cbox.x1, cbox.y0), (cbox.x0, cbox.y1), (cbox.x1, cbox.y1)]
                .map(|(x, y)| q.apply(pt(x, y)));
            let fits = corners.iter().all(|p| {
                p.x - 2.0 >= local[0] as f64
                    && p.y - 2.0 >= local[1] as f64
                    && p.x + 2.0 <= local[2] as f64
                    && p.y + 2.0 <= local[3] as f64
            });
            (!fits).then_some(local)
        });
        let key = RasterKey { source: id, matrix: q.matrix, persp: q.persp, frac: q.frac, clip };
        let frame = self.frame;

        if let Some(v) = self.bitmaps.get(&key, frame) {
            return Some(Raster { bitmap: v, pos: q.pos, residual: q.residual });
        }

        let map = |p: Pt| q.apply(p);

        let bm = match shape {
            Shape::Path(o) => {
                let mut lines = Vec::new();
                o.path.map(map).flatten_d6(RASTERIZER_PRECISION, &mut lines);
                raster::fill_groups(&[&lines], clip)
            },
            Shape::Lines(s) => {
                let sides: Vec<Vec<Vec<Pt>>> = s
                    .sides
                    .iter()
                    .map(|side| side.iter().map(|l| l.iter().map(|&p| map(p)).collect()).collect())
                    .collect();
                raster::fill_groups(&[&sides[0], &sides[1]], clip)
            },
        };

        // A mask cut to nothing is kept: it still counts as drawn.
        let bm = bm.filter(|b| !b.is_empty() || b.cut.is_some()).map(Arc::new);
        let size = bm.as_ref().map(|b| b.bytes()).unwrap_or(0) + 64;
        self.bitmaps.insert(key, bm.clone(), size, frame);
        Some(Raster { bitmap: bm, pos: q.pos, residual: q.residual })
    }

    /// A run's masks combined, blurred and shadowed.
    pub fn composite(&mut self, filter: &Filter, refs: &[BitmapRef]) -> Arc<Composite> {
        let key = CompositeKey {
            filter: *filter,
            refs: refs.iter().map(|r| (Held(r.fill.clone()), Held(r.border.clone()), r.pos, r.pos_o)).collect(),
        };

        let frame = self.frame;

        if let Some(v) = self.composites.get(&key, frame) {
            return v;
        }

        let v = Arc::new(composite(filter, refs));
        let size =
            [&v.fill, &v.border, &v.shadow].iter().map(|b| b.as_ref().map(|b| b.bytes()).unwrap_or(0)).sum::<usize>()
                + 64 * refs.len();
        self.composites.insert(key, v.clone(), size, frame);
        v
    }
}

/// A transform quantized as libass quantizes it: where it takes the outline's centre, in eighths
/// of a pixel, and what it does around there, in steps that move the box's edges by an eighth.
/// Masks are shared by every transform that rounds the same.
struct Quantized {
    matrix: [i64; 4],
    persp: [i64; 2],
    frac: (u8, u8),
    pos: (i32, i32),
    /// What the centre's rounding left over, in eighths.
    residual: Pt,
    center: Pt,
    a: [[f64; 2]; 2],
    z: [f64; 2],
    z_center: f64,
    offset: Pt,
}

impl Quantized {
    /// `delta` is the first glyph of the run's residual, which the rest of the run is rounded
    /// with.
    fn new(m: &[[f64; 3]; 3], cbox: &Rect, delta: Option<Pt>) -> Option<Quantized> {
        const MAX: f64 = 1e6;
        let c = pt((cbox.x0 + cbox.x1) / 2.0, (cbox.y0 + cbox.y1) / 2.0);
        let d = [(cbox.x1 - cbox.x0) / 2.0 + 1.0, (cbox.y1 - cbox.y0) / 2.0 + 1.0];
        let mut m = *m;

        for row in &mut m {
            row[2] += row[0] * c.x + row[1] * c.y;
        }

        if !(m[2][2] > 0.0) {
            return None;
        }

        let center = [m[0][2] / m[2][2], m[1][2] / m[2][2]];

        for i in 0..2 {
            for j in 0..2 {
                m[i][j] -= m[2][j] * center[i];
            }
        }

        let delta = delta.unwrap_or_default();
        let center = [center[0] * 8.0 - delta.x, center[1] * 8.0 - delta.y];

        if !(center[0].abs() < MAX && center[1].abs() < MAX) {
            return None;
        }

        let qr = [center[0].round_ties_even() as i64, center[1].round_ties_even() as i64];
        let z0 = m[2][2] - m[2][0].abs() * d[0] - m[2][1].abs() * d[1];
        let zz = z0.max(m[2][2] / MAX_PERSP_SCALE);
        let mut matrix = [0i64; 4];

        for i in 0..2 {
            for j in 0..2 {
                let v = m[i][j] * d[j] * 8.0 / zz;

                if !(v.abs() < MAX) {
                    return None;
                }

                matrix[i * 2 + j] = v.round_ties_even() as i64;
            }
        }

        let qmax = (matrix[0].abs() + matrix[1].abs()).max(matrix[2].abs() + matrix[3].abs());
        let mut persp = [0i64; 2];

        for j in 0..2 {
            let v = m[2][j] * d[j] * qmax as f64 / zz;

            if !(v.abs() < MAX) {
                return None;
            }

            persp[j] = v.round_ties_even() as i64;
        }

        let frac = ((qr[0] & 7) as u8, (qr[1] & 7) as u8);
        let pos = ((qr[0] >> 3) as i32, (qr[1] >> 3) as i32);

        // What the rounded transform does, so equal keys draw equal masks.
        let mut a = [[0.0; 2]; 2];

        for i in 0..2 {
            for j in 0..2 {
                a[i][j] = matrix[i * 2 + j] as f64 / (8.0 * d[j]);
            }
        }

        let z = match qmax {
            0 => [0.0; 2],
            q => [persp[0] as f64 / (d[0] * q as f64), persp[1] as f64 / (d[1] * q as f64)],
        };

        let z_center = (1.0 + z[0].abs() * d[0] + z[1].abs() * d[1]).min(MAX_PERSP_SCALE);

        Some(Quantized {
            matrix,
            persp,
            frac,
            pos,
            residual: pt(center[0] - qr[0] as f64, center[1] - qr[1] as f64),
            center: c,
            a,
            z,
            z_center,
            offset: pt(frac.0 as f64 / 8.0, frac.1 as f64 / 8.0),
        })
    }

    /// Where a point of the outline lands, relative to the mask's origin, to a 64th of a pixel.
    fn apply(&self, p: Pt) -> Pt {
        let q = p - self.center;
        let z = self.z[0] * q.x + self.z[1] * q.y + self.z_center;
        let w = 1.0 / z.max(0.1);
        let x = (self.a[0][0] * q.x + self.a[0][1] * q.y + self.offset.x * z) * w;
        let y = (self.a[1][0] * q.x + self.a[1][1] * q.y + self.offset.y * z) * w;
        pt((x * 64.0).round_ties_even() / 64.0, (y * 64.0).round_ties_even() / 64.0)
    }
}

/// Outlines, metrics and boxes for a character's glyphs.
pub(crate) fn outlines(f: &Frame, s: &State, r: &mut Resources, g: &mut Glyph) {
    if let Some(d) = &g.drawing {
        let text = d.text.clone();

        let entry = r.caches.outline(OutlineKey::Drawing(text.clone()), || {
            let (path, cbox) = outline::parse_drawing(&text)?;
            let (advance, asc) = if cbox.is_empty() { (0.0, 0.0) } else { (cbox.x1 - cbox.x0, cbox.y1 - cbox.y0) };
            Some((path, cbox, advance, asc, 0.0))
        });

        let Some(entry) = entry else { return };
        let base = 1i64 << ((d.scale - 1) & 31);
        let w = if base > 0 { 1.0 / base as f64 } else { 0.0 };
        let scale = pt(g.scale_x * w * s.screen_scale.0 / f.par, g.scale_y * w * s.screen_scale.1);
        let desc = d.pbo;
        let asc = entry.asc - desc;
        let offset = pt(0.0, -asc * scale.y);
        let part = &mut g.parts[0];
        part.bbox = Rect {
            x0: entry.cbox.x0 * scale.x + offset.x,
            y0: entry.cbox.y0 * scale.y + offset.y,
            x1: entry.cbox.x1 * scale.x + offset.x,
            y1: entry.cbox.y1 * scale.y + offset.y,
        };
        part.advance = pt(entry.advance * scale.x, 0.0);
        g.cluster_advance = part.advance;
        part.asc = asc * scale.y;
        part.desc = desc * scale.y;
        part.scale = scale;
        part.tr_offset = offset;
        part.outline = Some(entry);
        return;
    }

    let font = r.fonts.font(g.font);
    let Some(face) = font.faces.get(g.face).cloned() else { return };
    let weight = font.desc.weight;
    let italic = font.desc.italic;
    let embolden = !face.bold && weight as i32 > face.weight + 150;
    let italicize = !face.italic && italic > 55;

    for k in 0..g.parts.len() {
        let gid = g.parts[k].glyph;
        let key = OutlineKey::Glyph { face: face.uid, glyph: gid, embolden, italicize, deco: g.flags };
        let flags = g.flags;
        let face2 = face.clone();

        let entry = r.caches.outline(key, || {
            let glyph = face2.outline(gid, embolden, italicize, flags)?;
            let cbox = glyph.path.cbox();
            Some((glyph.path, cbox, glyph.advance, glyph.asc, glyph.desc))
        });

        let Some(entry) = entry else { continue };
        let scale = pt(g.scale_x, g.scale_y);
        let part = &mut g.parts[k];
        part.bbox = Rect {
            x0: entry.cbox.x0 * scale.x,
            y0: entry.cbox.y0 * scale.y,
            x1: entry.cbox.x1 * scale.x,
            y1: entry.cbox.y1 * scale.y,
        };
        part.asc = entry.asc * scale.y;
        part.desc = entry.desc * scale.y;
        part.scale = scale;
        part.tr_offset = pt(0.0, 0.0);
        part.outline = Some(entry);
    }
}

/// The glyph's transform from its own space (scaled, before positioning) to the screen: shear,
/// the three rotations around the origin, perspective, and its position.
fn transform(f: &Frame, s: &State, g: &Glyph, k: usize) -> [[f64; 3]; 3] {
    let part = &g.parts[k];
    let rad = std::f64::consts::PI / 180.0;
    let (sx, cx) = (-(g.frx * rad).sin(), (g.frx * rad).cos());
    let (sy, cy) = ((g.fry * rad).sin(), (g.fry * rad).cos());
    let (sz, cz) = (-(g.frz * rad).sin(), (g.frz * rad).cos());

    let fax = g.fax * g.scale_x / g.scale_y;
    let fay = g.fay * g.scale_y / g.scale_x;
    let x1 = [1.0, fax, part.shift.x + part.asc * fax];
    let y1 = [fay, 1.0, part.shift.y];

    let mut x2 = [0.0; 3];
    let mut y2 = [0.0; 3];

    for i in 0..3 {
        x2[i] = x1[i] * cz - y1[i] * sz;
        y2[i] = x1[i] * sz + y1[i] * cz;
    }

    let mut y3 = [0.0; 3];
    let mut z3 = [0.0; 3];

    for i in 0..3 {
        y3[i] = y2[i] * cx;
        z3[i] = y2[i] * sx;
    }

    let mut x4 = [0.0; 3];
    let mut z4 = [0.0; 3];

    for i in 0..3 {
        x4[i] = x2[i] * cy - z3[i] * sy;
        z4[i] = x2[i] * sy + z3[i] * cy;
    }

    // The camera's distance, as VSFilter has it (20000 in 1/64ths of a pixel).
    let dist = 20000.0 / 64.0 * s.blur_scale.1;
    z4[2] += dist;

    let scale_x = dist * f.par;
    let offs_x = part.pos.x - part.shift.x * f.par;
    let offs_y = part.pos.y - part.shift.y;
    let mut m = [[0.0; 3]; 3];

    for i in 0..3 {
        m[0][i] = z4[i] * offs_x + x4[i] * scale_x;
        m[1][i] = z4[i] * offs_y + y3[i] * dist;
        m[2][i] = z4[i];
    }

    m
}

fn scaled(m: &[[f64; 3]; 3], scale: Pt, offset: Pt) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];

    for i in 0..3 {
        out[i][0] = m[i][0] * scale.x;
        out[i][1] = m[i][1] * scale.y;
        out[i][2] = m[i][0] * offset.x + m[i][1] * offset.y + m[i][2];
    }

    out
}

/// How much a transform enlarges things around a box, perspective included.
fn magnification(m: &[[f64; 3]; 3], cbox: &Rect) -> f64 {
    let c = pt((cbox.x0 + cbox.x1) / 2.0, (cbox.y0 + cbox.y1) / 2.0);
    let (dx, dy) = ((cbox.x1 - cbox.x0) / 2.0 + 1.0, (cbox.y1 - cbox.y0) / 2.0 + 1.0);
    let zc = m[2][0] * c.x + m[2][1] * c.y + m[2][2];

    if !(zc > 0.0) {
        return 1.0;
    }

    let z0 = (zc - m[2][0].abs() * dx - m[2][1].abs() * dy).max(zc / MAX_PERSP_SCALE);
    let lin = (m[0][0].hypot(m[1][0])).max(m[0][1].hypot(m[1][1]));
    let persp = (m[2][0].hypot(m[2][1])) * ((m[0][2] / zc).abs() + (m[1][2] / zc).abs() + dx + dy);
    ((lin + persp) / z0).max(1e-6)
}

fn q(v: f64, step: f64) -> i64 {
    (v / step).round() as i64
}

/// The masks for one glyph of a run; None when it draws nothing.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bitmaps(
    f: &Frame,
    s: &State,
    r: &mut Resources,
    g: &Glyph,
    k: usize,
    flags: u32,
    first: bool,
    residual: &mut Pt,
    leftmost_x: &mut f64,
    view: [i32; 4],
) -> Option<BitmapRef> {
    let part = &g.parts[k];
    let outline = part.outline.clone()?;

    if g.symbol == '\n' as u32 || g.symbol == 0 || g.skip {
        return None;
    }

    let m1 = transform(f, s, g, k);
    let m2 = scaled(&m1, part.scale, part.tr_offset);

    if g.effect == crate::state::Effect::KaraokeKf {
        for &p in &outline.path.points {
            let z = m2[2][0] * p.x + m2[2][1] * p.y + m2[2][2];

            if z > 0.0 {
                *leftmost_x = leftmost_x.min((m2[0][0] * p.x + m2[0][1] * p.y + m2[0][2]) / z);
            }
        }
    }

    // The run's first glyph sets how the rest of it rounds.
    let fill = r.caches.raster(&Shape::Path(outline.clone()), &m2, (!first).then_some(*residual), Some(view))?;

    if first {
        *residual = fill.residual;
    }

    let delta = Some(*residual);
    let mut out = BitmapRef { fill: fill.bitmap, border: None, pos: fill.pos, pos_o: fill.pos };

    let bs = s.border_scale;

    if flags & FILTER_BORDER_STYLE_3 != 0 {
        if flags & (FILTER_NONZERO_BORDER | FILTER_NONZERO_SHADOW) == 0 {
            return out.has_any();
        }

        let unit = r.caches.outline(OutlineKey::Box, || {
            let mut p = Path::default();
            p.add_rect(0.0, 0.0, 1.0, 1.0, true);
            Some((p, Rect { x0: 0.0, y0: 0.0, x1: 1.0, y1: 1.0 }, 0.0, 0.0, 0.0))
        })?;

        let mut bord = pt(g.border.x * bs.0 / f.par, g.border.y * bs.1);
        let mut width = g.hspacing_scaled + part.advance.x;
        let mut height = part.asc + part.desc;
        // VSFilter scales the box twice.
        let orig = pt(g.scale_x * g.scale_fix, g.scale_y * g.scale_fix);
        bord = pt(bord.x * orig.x, bord.y * orig.y);
        width *= orig.x;
        height *= orig.y;
        bord = pt(bord.x.max(1.0), bord.y.max(1.0));
        let scale = pt(width + 2.0 * bord.x, height + 2.0 * bord.y);
        let offset = pt(-bord.x, -bord.y - part.asc);
        let m = scaled(&m1, scale, offset);

        out.set_border(r.caches.raster(&Shape::Path(unit), &m, delta, Some(view)));
        return out.has_any();
    }

    if flags & FILTER_NONZERO_BORDER == 0 {
        return out.has_any();
    }

    let bx = bs.0 * g.border.x / f.par;
    let by = bs.1 * g.border.y;
    let (qbx, qby) = (q(bx, 1.0 / 64.0), q(by, 1.0 / 64.0));

    if qbx == 0 && qby == 0 {
        out.border = out.fill.clone();
        return out.has_any();
    }

    let stroke_space = Rect {
        x0: outline.cbox.x0 * part.scale.x + part.tr_offset.x,
        y0: outline.cbox.y0 * part.scale.y + part.tr_offset.y,
        x1: outline.cbox.x1 * part.scale.x + part.tr_offset.x,
        y1: outline.cbox.y1 * part.scale.y + part.tr_offset.y,
    };

    let mag = magnification(&m1, &stroke_space);
    let level = (mag * 16.0).log2().ceil().clamp(-8.0, 24.0) as i32;
    let tolerance = 2f64.powi(-level);

    let key = StrokeKey {
        outline: outline.id,
        scale: (q(part.scale.x, 1.0 / 65536.0), q(part.scale.y, 1.0 / 65536.0)),
        offset: (q(part.tr_offset.x, 1.0 / 256.0), q(part.tr_offset.y, 1.0 / 256.0)),
        border: (qbx, qby),
        tolerance: level,
    };

    let (scale, offset) = (part.scale, part.tr_offset);
    let (bx, by) = (qbx as f64 / 64.0, qby as f64 / 64.0);

    let stroked = r.caches.stroke(key, || {
        let mut lines = Vec::new();
        outline
            .path
            .map(|p| pt(p.x * scale.x + offset.x, p.y * scale.y + offset.y))
            .flatten(tolerance / 4.0, &mut lines);
        stroke::stroke(&lines, bx, by, tolerance)
    });

    if let Some(st) = stroked {
        out.set_border(r.caches.raster(&Shape::Lines(st), &m1, delta, Some(view)));
    }

    out.has_any()
}

impl BitmapRef {
    fn set_border(&mut self, r: Option<Raster>) {
        let Some(Raster { bitmap: Some(bm), pos, .. }) = r else { return };
        self.border = Some(bm);
        self.pos_o = pos;

        if self.fill.is_none() {
            self.pos = pos;
        }
    }

    fn has_any(self) -> Option<BitmapRef> {
        (self.fill.is_some() || self.border.is_some()).then_some(self)
    }
}

/// `\blur` as a quantized variance, and how finely the shadow is then worth placing.
pub(crate) fn quantize_blur(radius: f64) -> (i32, i32) {
    let scale = 64.0 * BLUR_PRECISION / POSITION_PRECISION;
    let radius = radius * scale;
    let x = (1.0 + radius) * (POSITION_PRECISION / 2.0);
    let ord = x.log2().floor() as i32 + 1;
    let mask = ((1u32 << ord.clamp(0, 31)) - 1) as i32;
    (((radius).ln_1p() / BLUR_PRECISION).round() as i32, mask)
}

/// Where a run's masks can still show, as left, top, right and bottom on screen: the frame, as
/// far around it as the run's blur, `\be` and shadow reach, and a frame's height more above and
/// below, as overlapping events are moved up or down after they're drawn.
pub(crate) fn view(f: &Frame, filter: &Filter) -> [i32; 4] {
    // The blur cascade spreads a mask by less than 9 standard deviations.
    let sigma = restore_blur(filter.blur_x.max(filter.blur_y)).sqrt();
    let shadow = filter.shadow.0.abs().max(filter.shadow.1.abs()) >> 6;
    let reach =
        ((9.0 * sigma).ceil() as i64 + 16 + 2 * filter.be.max(0) as i64 + shadow as i64 + 1).min(1 << 24) as i32;
    [-reach, -f.height - reach, f.width + reach, 2 * f.height + reach]
}

fn restore_blur(qblur: i32) -> f64 {
    let scale = 64.0 * BLUR_PRECISION / POSITION_PRECISION;
    let sigma = (BLUR_PRECISION * qblur as f64).exp_m1() / scale;
    sigma * sigma
}

use crate::event::{
    FILTER_BORDER_STYLE_3, FILTER_FILL_IN_BORDER, FILTER_FILL_IN_SHADOW, FILTER_NONZERO_BORDER, FILTER_NONZERO_SHADOW,
};

/// Adds the glyphs' masks together, with room around them for `\be`. When some were cut, only
/// what they drew is kept, its corner a multiple of `grid` from where the whole would start, so
/// the blur's steps fall where they would have.
fn combine(parts: &[(&Arc<Bitmap>, (i32, i32))], pad: i32, grid: (i32, i32)) -> Option<Bitmap> {
    if parts.is_empty() {
        return None;
    }

    let cut = parts.iter().any(|(b, _)| b.cut.is_some());

    if pad == 0 && parts.len() == 1 && !cut {
        let (b, pos) = parts[0];
        let mut b = Bitmap::clone(b);
        b.left += pos.0;
        b.top += pos.1;
        return Some(b);
    }

    let union = |boxes: &mut dyn Iterator<Item = [i32; 4]>| {
        boxes.fold([i32::MAX, i32::MAX, i32::MIN, i32::MIN], |u, b| {
            [u[0].min(b[0]), u[1].min(b[1]), u[2].max(b[2]), u[3].max(b[3])]
        })
    };

    let at = |b: [i32; 4], pos: (i32, i32)| [b[0] + pos.0, b[1] + pos.1, b[2] + pos.0, b[3] + pos.1];
    let drawn = |b: &Bitmap| [b.left, b.top, b.right(), b.bottom()];
    let whole = union(&mut parts.iter().map(|(b, pos)| at(b.cut.unwrap_or(drawn(b)), *pos)));
    let [mut x0, mut y0, x1, y1] = [whole[0] - pad, whole[1] - pad, whole[2] + pad, whole[3] + pad];
    let (mut x1, mut y1) = (x1, y1);

    if cut {
        let kept = union(&mut parts.iter().filter(|(b, _)| !b.is_empty()).map(|(b, pos)| at(drawn(b), *pos)));

        if kept[0] > kept[2] {
            return None;
        }

        x0 += (kept[0] - pad - x0).max(0) / grid.0 * grid.0;
        y0 += (kept[1] - pad - y0).max(0) / grid.1 * grid.1;
        x1 = x1.min(kept[2] + pad);
        y1 = y1.min(kept[3] + pad);
    }

    let mut dst = Bitmap::new(x0, y0, x1 - x0, y1 - y0);

    for (b, pos) in parts {
        dst.add_at(b, *pos);
    }

    Some(dst)
}

fn composite(filter: &Filter, refs: &[BitmapRef]) -> Composite {
    let pad = blur::be_padding(filter.be);
    let fills: Vec<_> = refs.iter().filter_map(|r| r.fill.as_ref().map(|b| (b, r.pos))).collect();
    let borders: Vec<_> = refs.iter().filter_map(|r| r.border.as_ref().map(|b| (b, r.pos_o))).collect();
    let flags = filter.flags;
    let (r2x, r2y) = (restore_blur(filter.blur_x), restore_blur(filter.blur_y));
    let grid = blur::grid(r2x, r2y);
    let mut bm = combine(&fills, pad, grid);
    let mut bm_o = combine(&borders, pad, grid);

    let synth = |b: &mut Option<Bitmap>| {
        if let Some(b) = b {
            if r2x > 0.001 || r2y > 0.001 {
                blur::gaussian(b, r2x, r2y);
            }

            blur::be(b, filter.be);
        }
    };

    if flags & FILTER_NONZERO_BORDER == 0 || flags & FILTER_BORDER_STYLE_3 != 0 {
        synth(&mut bm);
    }

    synth(&mut bm_o);

    let fix = |g: &Option<Bitmap>, o: &mut Option<Bitmap>| {
        if let (Some(g), Some(o)) = (g, o) {
            Bitmap::fix_outline(g, o);
        }
    };

    if flags & FILTER_FILL_IN_BORDER == 0 && flags & FILTER_FILL_IN_SHADOW == 0 {
        fix(&bm, &mut bm_o);
    }

    let mut bm_s = None;

    if flags & FILTER_NONZERO_SHADOW != 0 {
        if flags & FILTER_NONZERO_BORDER != 0 {
            bm_s = bm_o.clone();

            if flags & FILTER_FILL_IN_BORDER != 0 && flags & FILTER_FILL_IN_SHADOW == 0 {
                fix(&bm, &mut bm_s);
            }
        } else if flags & FILTER_BORDER_STYLE_3 != 0 {
            bm_s = bm_o.take();
        } else {
            bm_s = bm.clone();
        }

        if let Some(s) = &mut bm_s {
            s.left += filter.shadow.0 >> 6;
            s.top += filter.shadow.1 >> 6;
            s.shift((filter.shadow.0 & 63) as u32, (filter.shadow.1 & 63) as u32);
        }
    }

    if flags & FILTER_FILL_IN_SHADOW != 0 && flags & FILTER_FILL_IN_BORDER == 0 {
        fix(&bm, &mut bm_o);
    }

    let wrap = |b: Option<Bitmap>| b.filter(|b| !b.is_empty()).map(Arc::new);
    Composite { fill: wrap(bm), border: wrap(bm_o), shadow: wrap(bm_s) }
}

/// Masks the images with a vector `\clip` (or `\iclip`).
pub(crate) fn vector_clip(f: &Frame, s: &State, r: &mut Resources, cd: &ClipDrawing, images: &mut Vec<Image>) {
    let text = cd.text.clone();

    let Some(entry) = r.caches.outline(OutlineKey::Drawing(text.clone()), || {
        let (path, cbox) = outline::parse_drawing(&text)?;
        Some((path, cbox, 0.0, 0.0, 0.0))
    }) else {
        return;
    };

    let base = 1i64 << ((cd.scale - 1) & 31);
    let w = if base > 0 { 1.0 / base as f64 } else { 0.0 };

    let m = [
        [s.screen_scale.0 * w, 0.0, f.margins.2 as f64],
        [0.0, s.screen_scale.1 * w, f.margins.0 as f64],
        [0.0, 0.0, 1.0],
    ];

    let Some(Raster { bitmap: Some(clip), pos, .. }) = r.caches.raster(&Shape::Path(entry), &m, None, None) else {
        if !cd.inverse {
            images.clear();
        }

        return;
    };

    let (bx, by) = (pos.0 + clip.left, pos.1 + clip.top);
    let mut out = Vec::with_capacity(images.len());

    for img in images.drain(..) {
        let (ax, ay, aw, ah) = (img.x, img.y, img.w, img.h);
        let left = ax.max(bx);
        let top = ay.max(by);
        let right = (ax + aw).min(bx + clip.w);
        let bottom = (ay + ah).min(by + clip.h);
        let (w, h) = (right - left, bottom - top);
        let disjoint = ax + aw < bx || ay + ah < by || ax > bx + clip.w || ay > by + clip.h || w <= 0 || h <= 0;
        let clip_at = |x: i32, y: i32| (y - by) as usize * clip.stride + (x - bx) as usize;

        if cd.inverse {
            if disjoint {
                out.push(img);
                continue;
            }

            let mut nb = Bitmap::new(0, 0, aw, ah);

            for y in 0..ah as usize {
                nb.data[y * nb.stride..][..aw as usize].copy_from_slice(&img.row(y)[..aw as usize]);
            }

            let start = (top - ay) as usize * nb.stride + (left - ax) as usize;
            let stride = nb.stride;
            bitmap::imul_rows(
                &mut nb.data[start..],
                stride,
                &clip.data[clip_at(left, top)..],
                clip.stride,
                w as usize,
                h as usize,
            );
            out.push(Image::new(Arc::new(nb), 0, 0, aw, ah, ax, ay, img.color_raw(), img.kind));
        } else {
            if disjoint {
                continue;
            }

            let mut nb = Bitmap::new(0, 0, w, h);

            for y in 0..h as usize {
                let src = &img.row(y + (top - ay) as usize)[(left - ax) as usize..][..w as usize];
                nb.data[y * nb.stride..][..w as usize].copy_from_slice(src);
            }

            let stride = nb.stride;
            bitmap::mul_rows(
                &mut nb.data,
                stride,
                &clip.data[clip_at(left, top)..],
                clip.stride,
                w as usize,
                h as usize,
            );
            out.push(Image::new(Arc::new(nb), 0, 0, w, h, left, top, img.color_raw(), img.kind));
        }
    }

    *images = out;
    let _ = ImageKind::Character;
}
