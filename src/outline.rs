// SPDX-License-Identifier: LGPL-3.0-or-later
//! Outlines: glyphs and drawings as contours of lines and Bézier curves, y pointing down.

use std::ops::{Add, Mul, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Pt {
    pub x: f64,
    pub y: f64,
}

pub(crate) const fn pt(x: f64, y: f64) -> Pt {
    Pt { x, y }
}

impl Add for Pt {
    type Output = Pt;

    fn add(self, o: Pt) -> Pt {
        pt(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Pt {
    type Output = Pt;

    fn sub(self, o: Pt) -> Pt {
        pt(self.x - o.x, self.y - o.y)
    }
}

impl Mul<f64> for Pt {
    type Output = Pt;

    fn mul(self, k: f64) -> Pt {
        pt(self.x * k, self.y * k)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Rect {
    pub x0: f64,
    pub y0: f64,
    pub x1: f64,
    pub y1: f64,
}

impl Rect {
    pub const EMPTY: Rect = Rect { x0: f64::INFINITY, y0: f64::INFINITY, x1: f64::NEG_INFINITY, y1: f64::NEG_INFINITY };

    pub fn add(&mut self, p: Pt) {
        self.x0 = self.x0.min(p.x);
        self.y0 = self.y0.min(p.y);
        self.x1 = self.x1.max(p.x);
        self.y1 = self.y1.max(p.y);
    }

    pub fn is_empty(&self) -> bool {
        self.x0 > self.x1 || self.y0 > self.y1
    }
}

/// How many points a segment owns: its own start and control points. Its end is the next
/// segment's start, or the contour's first point for a contour's last segment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Seg {
    Line,
    Quad,
    Cubic,
}

impl Seg {
    fn order(self) -> usize {
        match self {
            Seg::Line => 1,
            Seg::Quad => 2,
            Seg::Cubic => 3,
        }
    }
}

/// Closed polylines, their memory kept from one use to the next.
#[derive(Default)]
pub(crate) struct Polys {
    lines: Vec<Vec<Pt>>,
    len: usize,
    /// Where [`Path::flatten_d6`] works.
    d6: Vec<[i64; 2]>,
}

impl Polys {
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Starts a polyline.
    pub fn push(&mut self) -> &mut Vec<Pt> {
        if self.len == self.lines.len() {
            self.lines.push(Vec::new());
        }

        self.len += 1;
        let line = &mut self.lines[self.len - 1];
        line.clear();
        line
    }

    pub fn lines(&self) -> &[Vec<Pt>] {
        &self.lines[..self.len]
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Path {
    pub points: Vec<Pt>,
    /// Each segment, and whether it closes its contour.
    pub segments: Vec<(Seg, bool)>,
}

impl Path {
    pub fn cbox(&self) -> Rect {
        let mut r = Rect::EMPTY;

        for &p in &self.points {
            r.add(p);
        }

        r
    }

    /// Adds a point; `seg` is the segment it ends, which starts at the point before.
    pub fn push(&mut self, p: Pt, seg: Option<Seg>) {
        self.points.push(p);

        if let Some(seg) = seg {
            self.segments.push((seg, false));
        }
    }

    pub fn close(&mut self) {
        if let Some(last) = self.segments.last_mut() {
            last.1 = true;
        }
    }

    /// A rectangle, wound like the glyph it's added to (`clockwise` in y-down coordinates).
    pub fn add_rect(&mut self, x0: f64, y0: f64, x1: f64, y1: f64, clockwise: bool) {
        let (y0, y1) = if clockwise { (y0, y1) } else { (y1, y0) };

        for p in [pt(x0, y0), pt(x1, y0), pt(x1, y1), pt(x0, y1)] {
            self.push(p, Some(Seg::Line));
        }

        self.close();
    }

    pub fn map(&self, f: impl Fn(Pt) -> Pt) -> Path {
        Path { points: self.points.iter().map(|&p| f(p)).collect(), segments: self.segments.clone() }
    }

    /// The contours, each a list of segments as their start, control and end points.
    pub fn contours(&self) -> Contours<'_> {
        Contours { path: self, point: 0, segment: 0 }
    }

    /// Twice the signed area of the control polygon; positive when clockwise on screen.
    pub fn area(&self) -> f64 {
        let mut a = 0.0;

        for contour in self.contours() {
            for seg in contour {
                let (p0, p1) = seg.ends();
                a += p0.x * p1.y - p1.x * p0.y;
            }
        }

        a
    }

    /// Lines approximating the outline, its points moved by `map`, as closed polylines.
    /// Flattens as libass's rasterizer does, on points in 64ths of a pixel: curves are halved
    /// until their control points are within `error` 64ths of the chord.
    pub fn flatten_d6(&self, map: impl Fn(Pt) -> Pt, error: i64, out: &mut Polys) {
        let d6 = |p: Pt| {
            let p = map(p);
            [(p.x * 64.0).round() as i64, (p.y * 64.0).round() as i64]
        };
        let mut poly = std::mem::take(&mut out.d6);

        for contour in self.contours() {
            poly.clear();

            for seg in contour {
                match seg {
                    Segment::Line(a, _) => poly.push(d6(a)),
                    Segment::Quad(a, b, c) => quad_d6(&mut poly, [d6(a), d6(b), d6(c)], error),
                    Segment::Cubic(a, b, c, d) => cubic_d6(&mut poly, [d6(a), d6(b), d6(c), d6(d)], error),
                }
            }

            poly.dedup();

            if poly.len() >= 2 {
                out.push().extend(poly.iter().map(|&[x, y]| pt(x as f64 / 64.0, y as f64 / 64.0)));
            }
        }

        out.d6 = poly;
    }

    /// Lines approximating the outline, its points moved by `map`, to within `tolerance`, as
    /// closed polylines.
    pub fn flatten(&self, map: impl Fn(Pt) -> Pt, tolerance: f64, out: &mut Vec<Vec<Pt>>) {
        let tol2 = tolerance * tolerance;

        for contour in self.contours() {
            let mut poly = Vec::new();

            for seg in contour {
                match seg.map(&map) {
                    Segment::Line(a, _) => poly.push(a),
                    Segment::Quad(a, b, c) => {
                        poly.push(a);
                        let n = subdivisions((a - b * 2.0 + c) * 0.25, tol2);

                        for i in 1..n {
                            let t = i as f64 / n as f64;
                            let u = 1.0 - t;
                            poly.push(a * (u * u) + b * (2.0 * u * t) + c * (t * t));
                        }
                    },
                    Segment::Cubic(a, b, c, d) => {
                        poly.push(a);
                        let dd1 = a - b * 2.0 + c;
                        let dd2 = b - c * 2.0 + d;
                        let dd = pt(dd1.x.abs().max(dd2.x.abs()), dd1.y.abs().max(dd2.y.abs()));
                        let n = subdivisions(dd * 0.75, tol2);

                        for i in 1..n {
                            let t = i as f64 / n as f64;
                            let u = 1.0 - t;
                            poly.push(
                                a * (u * u * u) + b * (3.0 * u * u * t) + c * (3.0 * u * t * t) + d * (t * t * t),
                            );
                        }
                    },
                }
            }

            if poly.len() >= 2 {
                out.push(poly);
            }
        }
    }
}

/// How many pieces keep a curve with second difference `dd` within the tolerance.
fn subdivisions(dd: Pt, tol2: f64) -> usize {
    let e = dd.x * dd.x + dd.y * dd.y;
    ((e / tol2).sqrt().sqrt().ceil() as usize).clamp(1, 256)
}

pub(crate) enum Segment {
    Line(Pt, Pt),
    Quad(Pt, Pt, Pt),
    Cubic(Pt, Pt, Pt, Pt),
}

impl Segment {
    fn ends(&self) -> (Pt, Pt) {
        match *self {
            Segment::Line(a, b) | Segment::Quad(a, _, b) | Segment::Cubic(a, _, _, b) => (a, b),
        }
    }

    fn map(self, f: impl Fn(Pt) -> Pt) -> Segment {
        match self {
            Segment::Line(a, b) => Segment::Line(f(a), f(b)),
            Segment::Quad(a, b, c) => Segment::Quad(f(a), f(b), f(c)),
            Segment::Cubic(a, b, c, d) => Segment::Cubic(f(a), f(b), f(c), f(d)),
        }
    }
}

pub(crate) struct Contours<'a> {
    path: &'a Path,
    point: usize,
    segment: usize,
}

impl<'a> Iterator for Contours<'a> {
    type Item = Contour<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let segments = &self.path.segments;

        if self.segment >= segments.len() {
            return None;
        }

        let contour =
            Contour { path: self.path, first: self.point, point: self.point, segment: self.segment, done: false };

        while self.segment < segments.len() {
            let (seg, end) = segments[self.segment];
            self.point += seg.order();
            self.segment += 1;

            if end {
                break;
            }
        }

        Some(contour)
    }
}

/// A contour's segments, as their start, control and end points.
pub(crate) struct Contour<'a> {
    path: &'a Path,
    first: usize,
    point: usize,
    segment: usize,
    done: bool,
}

impl Iterator for Contour<'_> {
    type Item = Segment;

    fn next(&mut self) -> Option<Segment> {
        let path = self.path;

        if self.done || self.segment >= path.segments.len() {
            return None;
        }

        let (seg, end) = path.segments[self.segment];
        let p = self.point;
        self.point += seg.order();
        self.segment += 1;
        self.done = end;
        let next = if end || self.segment == path.segments.len() { self.first } else { self.point };
        let at = |i: usize| path.points.get(i).copied().unwrap_or_default();

        Some(match seg {
            Seg::Line => Segment::Line(at(p), at(next)),
            Seg::Quad => Segment::Quad(at(p), at(p + 1), at(next)),
            Seg::Cubic => Segment::Cubic(at(p), at(p + 1), at(p + 2), at(next)),
        })
    }
}

/// A drawing's outline from `\p` commands, in drawing units, and its control box.
pub(crate) fn parse_drawing(text: &str) -> Option<(Path, Rect)> {
    let tokens = tokenize(text)?;
    let mut path = Path::default();
    let mut cbox = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    let mut update = |p: (i32, i32)| {
        cbox.0 = cbox.0.min(p.0);
        cbox.1 = cbox.1.min(p.1);
        cbox.2 = cbox.2.max(p.0);
        cbox.3 = cbox.3.max(p.1);
    };

    let to_pt = |p: (i32, i32)| pt(p.0 as f64 / 64.0, p.1 as f64 / 64.0);
    let mut started = false;
    let mut pen = (0, 0);
    let mut i = 0;

    while i < tokens.len() {
        let (kind, p) = tokens[i];

        match kind {
            Token::MoveNc => {
                pen = p;
                update(p);
                i += 1;
            },
            Token::Move => {
                pen = p;
                update(p);

                if started {
                    path.segments.push((Seg::Line, true));
                    started = false;
                }

                i += 1;
            },
            Token::Line => {
                update(p);

                if !started {
                    path.push(to_pt(pen), None);
                }

                path.push(to_pt(p), Some(Seg::Line));
                started = true;
                i += 1;
            },
            Token::Bezier | Token::BSpline | Token::ExtendSpline => {
                let (first, step) = match kind {
                    Token::ExtendSpline => (i - 3, 1),
                    _ => (i - 1, 3),
                };

                let spline = kind != Token::Bezier;
                let mut q = [(0, 0); 4];

                for (k, q) in q.iter_mut().enumerate() {
                    *q = tokens[first + k].1;
                    update(*q);
                }

                if spline {
                    let third = |a: (i32, i32), b: (i32, i32)| ((b.0 - a.0) / 3, (b.1 - a.1) / 3);
                    let (d01, d12, d23) = (third(q[0], q[1]), third(q[1], q[2]), third(q[2], q[3]));
                    q[0] = (q[1].0 + ((d12.0 - d01.0) >> 1), q[1].1 + ((d12.1 - d01.1) >> 1));
                    q[3] = (q[2].0 + ((d23.0 - d12.0) >> 1), q[2].1 + ((d23.1 - d12.1) >> 1));
                    q[1] = (q[1].0 + d12.0, q[1].1 + d12.1);
                    q[2] = (q[2].0 - d12.0, q[2].1 - d12.1);
                }

                if !started {
                    path.push(to_pt(q[0]), None);
                }

                path.push(to_pt(q[1]), None);
                path.push(to_pt(q[2]), None);
                path.push(to_pt(q[3]), Some(Seg::Cubic));
                started = true;
                i += step;
            },
        }
    }

    if started {
        path.segments.push((Seg::Line, true));
    }

    let cbox = if cbox.0 > cbox.2 {
        Rect::default()
    } else {
        Rect { x0: cbox.0 as f64 / 64.0, y0: cbox.1 as f64 / 64.0, x1: cbox.2 as f64 / 64.0, y1: cbox.3 as f64 / 64.0 }
    };

    Some((path, cbox))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Token {
    Move,
    MoveNc,
    Line,
    Bezier,
    BSpline,
    ExtendSpline,
}

fn point(s: &mut &str) -> Option<(i32, i32)> {
    let (x, n) = crate::text::strtod(s);

    if n == 0 {
        return None;
    }

    *s = &s[n..];
    let (y, n) = crate::text::strtod(s);

    if n == 0 {
        return None;
    }

    *s = &s[n..];
    Some(((x * 64.0).round_ties_even() as i32, (y * 64.0).round_ties_even() as i32))
}

/// As VSFilter reads drawings: commands before the first move are ignored, a drawing starting
/// with `n` is rejected, and `p` needs three points to extend.
fn tokenize(text: &str) -> Option<Vec<(Token, (i32, i32))>> {
    let mut s = text;
    let mut tokens: Vec<(Token, (i32, i32))> = Vec::new();
    let mut spline_start: Option<usize> = None;
    let mut m_seen = false;

    let many = |s: &mut &str, tokens: &mut Vec<(Token, (i32, i32))>, kind: Token, batch: usize| {
        let mut buf = Vec::with_capacity(batch);

        while !s.is_empty() {
            let Some(p) = point(s) else { break };
            buf.push(p);

            if buf.len() == batch {
                tokens.extend(buf.drain(..).map(|p| (kind, p)));
            }
        }
    };

    while let Some(cmd) = s.chars().next() {
        s = &s[cmd.len_utf8()..];

        match cmd {
            'm' => {
                m_seen = true;

                if tokens.is_empty() {
                    let Some(p) = point(&mut s) else { continue };
                    tokens.push((Token::Move, p));
                }

                many(&mut s, &mut tokens, Token::Move, 1);
            },
            'n' => {
                if tokens.is_empty() {
                    let Some(p) = point(&mut s) else { continue };

                    if !m_seen {
                        return None;
                    }

                    tokens.push((Token::MoveNc, p));
                }

                many(&mut s, &mut tokens, Token::MoveNc, 1);
            },
            'l' if !tokens.is_empty() => many(&mut s, &mut tokens, Token::Line, 1),
            'b' if !tokens.is_empty() => many(&mut s, &mut tokens, Token::Bezier, 3),
            's' | 'p' if !tokens.is_empty() => {
                if cmd == 's' {
                    let mut three = Vec::new();

                    for _ in 0..3 {
                        match point(&mut s) {
                            Some(p) => three.push(p),
                            None => break,
                        }
                    }

                    if three.len() < 3 {
                        spline_start = None;
                        continue;
                    }

                    spline_start = Some(tokens.len() - 1);
                    tokens.extend(three.into_iter().map(|p| (Token::BSpline, p)));
                }

                if tokens.len() < 3 {
                    continue;
                }

                many(&mut s, &mut tokens, Token::ExtendSpline, 1);
            },
            'c' => {
                let Some(start) = spline_start.take() else { continue };

                for k in 0..3 {
                    let p = tokens[start + k].1;
                    tokens.push((Token::ExtendSpline, p));
                }
            },
            _ => {},
        }
    }

    Some(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn square() {
        let (path, cbox) = parse_drawing("m 0 0 l 10 0 10 10 0 10").unwrap();
        assert_eq!(path.points.len(), 4);
        assert_eq!(path.segments.len(), 4);
        assert!(path.segments[3].1);
        assert_eq!(cbox, Rect { x0: 0.0, y0: 0.0, x1: 10.0, y1: 10.0 });
        assert!(path.area() > 0.0);
    }

    #[test]
    fn bezier() {
        let (path, _) = parse_drawing("m 0 0 b 10 0 10 10 0 10").unwrap();
        assert_eq!(path.segments, vec![(Seg::Cubic, false), (Seg::Line, true)]);
        let mut lines = Vec::new();
        path.flatten(|p| p, 0.01, &mut lines);
        assert!(lines[0].len() > 4);
    }

    #[test]
    fn ignores_commands_before_m() {
        assert_eq!(parse_drawing("l 5 5 m 0 0 l 1 0 1 1").unwrap().0.points.len(), 3);
        assert!(parse_drawing("n 0 0 l 1 1").is_none());
    }
}

/// Whether `p` is further than libass allows from the chord `a`..`b`.
fn needs_split(a: [i64; 2], b: [i64; 2], p: [i64; 2], error: i64) -> bool {
    let r = [b[0] - a[0], b[1] - a[1]];
    let er = error * r[0].abs().max(r[1].abs());
    let v = [p[0] - a[0], p[1] - a[1]];
    let dot = r[0] * v[0] + r[1] * v[1];
    let cross = r[0] * v[1] - r[1] * v[0];
    dot < -er || dot > r[0] * r[0] + r[1] * r[1] + er || cross.abs() > er
}

/// Pushes a quadratic's points but its end.
fn quad_d6(out: &mut Vec<[i64; 2]>, p: [[i64; 2]; 3], error: i64) {
    if !needs_split(p[0], p[2], p[1], error) {
        out.push(p[0]);
        return;
    }

    let n1 = [p[0][0] + p[1][0], p[0][1] + p[1][1]];
    let n3 = [p[1][0] + p[2][0], p[1][1] + p[2][1]];
    let n2 = [(n1[0] + n3[0] + 2) >> 2, (n1[1] + n3[1] + 2) >> 2];
    let (n1, n3) = ([n1[0] >> 1, n1[1] >> 1], [n3[0] >> 1, n3[1] >> 1]);
    quad_d6(out, [p[0], n1, n2], error);
    quad_d6(out, [n2, n3, p[2]], error);
}

/// Pushes a cubic's points but its end.
fn cubic_d6(out: &mut Vec<[i64; 2]>, p: [[i64; 2]; 4], error: i64) {
    if !needs_split(p[0], p[3], p[1], error) && !needs_split(p[0], p[3], p[2], error) {
        out.push(p[0]);
        return;
    }

    let add = |a: [i64; 2], b: [i64; 2]| [a[0] + b[0], a[1] + b[1]];
    let n1 = add(p[0], p[1]);
    let center = [p[1][0] + p[2][0] + 2, p[1][1] + p[2][1] + 2];
    let n5 = add(p[2], p[3]);
    let n2 = add(n1, center);
    let n4 = add(center, n5);
    let n3 = [(n2[0] + n4[0] - 1) >> 3, (n2[1] + n4[1] - 1) >> 3];
    let (n2, n4) = ([n2[0] >> 2, n2[1] >> 2], [n4[0] >> 2, n4[1] >> 2]);
    let (n1, n5) = ([n1[0] >> 1, n1[1] >> 1], [n5[0] >> 1, n5[1] >> 1]);
    cubic_d6(out, [p[0], n1, n2, n3], error);
    cubic_d6(out, [n3, n4, n5, p[3]], error);
}
