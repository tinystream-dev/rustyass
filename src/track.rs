// SPDX-License-Identifier: LGPL-3.0-or-later
//! Scripts: their info, styles, events and embedded fonts, read the way VSFilter reads them.

use crate::text::{self, skip_spaces, trim_end_spaces};

pub(crate) const HALIGN_LEFT: i32 = 1;
pub(crate) const HALIGN_CENTER: i32 = 2;
pub(crate) const HALIGN_RIGHT: i32 = 3;
pub(crate) const VALIGN_SUB: i32 = 0;
pub(crate) const VALIGN_TOP: i32 = 4;
pub(crate) const VALIGN_CENTER: i32 = 8;

const ASS_STYLE_FORMAT: &str = "Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, \
                                Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, \
                                Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding";
const ASS_EVENT_FORMAT: &str = "Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text";
const SSA_STYLE_FORMAT: &str = "Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, TertiaryColour, \
                                BackColour, Bold, Italic, BorderStyle, Outline, Shadow, Alignment, MarginL, \
                                MarginR, MarginV, AlphaLevel, Encoding";
const SSA_EVENT_FORMAT: &str = "Marked, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Unknown,
    Ssa,
    Ass,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    None,
    Info,
    Styles,
    Events,
    Fonts,
}

/// A style, with colours as `0xRRGGBBAA` where the alpha is transparency, as in scripts.
#[derive(Clone, Debug)]
pub struct Style {
    pub name: String,
    pub font_name: String,
    pub font_size: f64,
    pub colors: [u32; 4],
    pub bold: i32,
    pub italic: i32,
    pub underline: bool,
    pub strike_out: bool,
    pub scale_x: f64,
    pub scale_y: f64,
    pub spacing: f64,
    pub angle: f64,
    pub border_style: i32,
    pub outline: f64,
    pub shadow: f64,
    /// libass's alignment: 1, 2 or 3 horizontally, plus 0 (bottom), 4 (top) or 8 (middle).
    pub alignment: i32,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
    pub encoding: i32,
    pub blur: f64,
}

impl Style {
    fn new() -> Style {
        Style {
            name: String::new(),
            font_name: String::new(),
            font_size: 0.0,
            colors: [0; 4],
            bold: 0,
            italic: 0,
            underline: false,
            strike_out: false,
            scale_x: 100.0,
            scale_y: 100.0,
            spacing: 0.0,
            angle: 0.0,
            border_style: 0,
            outline: 0.0,
            shadow: 0.0,
            alignment: 0,
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            encoding: 0,
            blur: 0.0,
        }
    }

    /// VSFilter's own default, used when a script names a style it doesn't have.
    fn default_style() -> Style {
        Style {
            name: "Default".into(),
            font_name: "Arial".into(),
            font_size: 18.0,
            colors: [0xFFFFFF00, 0x00FFFF00, 0x00000000, 0x00000080],
            bold: 200,
            scale_x: 1.0,
            scale_y: 1.0,
            border_style: 1,
            outline: 2.0,
            shadow: 3.0,
            alignment: 2,
            margin_l: 20,
            margin_r: 20,
            margin_v: 20,
            ..Style::new()
        }
    }
}

/// A line of dialogue, `start` and `duration` in milliseconds.
#[derive(Clone, Debug)]
pub struct Event {
    pub layer: i32,
    pub start: i64,
    pub duration: i64,
    pub style: usize,
    pub name: String,
    pub margin_l: i32,
    pub margin_r: i32,
    pub margin_v: i32,
    pub effect: String,
    pub text: String,
    pub read_order: usize,
}

/// A whole script, read from memory.
#[derive(Clone, Debug)]
pub struct Track {
    kind: Kind,
    pub(crate) play_res: (i32, i32),
    pub(crate) layout_res: (i32, i32),
    pub(crate) wrap_style: i32,
    pub(crate) scaled_border_and_shadow: bool,
    pub(crate) kerning: bool,
    pub(crate) language: Option<String>,
    pub(crate) styles: Vec<Style>,
    pub(crate) events: Vec<Event>,
    default_style: usize,
    fonts: Vec<(String, Vec<u8>)>,
}

struct Parser {
    track: Track,
    section: Section,
    style_format: Option<String>,
    event_format: Option<String>,
    flags: u32,
    font_name: Option<String>,
    font_data: Vec<u8>,
}

const SINFO_PLAYRESX: u32 = 1 << 0;
const SINFO_PLAYRESY: u32 = 1 << 1;
const SINFO_TIMER: u32 = 1 << 2;
const SINFO_WRAPSTYLE: u32 = 1 << 3;
const SINFO_SCALEDBORDER: u32 = 1 << 4;
const SINFO_COLOURMATRIX: u32 = 1 << 5;
const SINFO_KERNING: u32 = 1 << 6;
const SINFO_SCRIPTTYPE: u32 = 1 << 7;
const SINFO_LANGUAGE: u32 = 1 << 8;
const SINFO_LAYOUTRESX: u32 = 1 << 9;
const SINFO_LAYOUTRESY: u32 = 1 << 10;
const GENBY_FFMPEG: u32 = 1 << 14;

impl Track {
    /// Reads a script; None when it's neither SSA nor ASS.
    pub fn parse(data: &[u8]) -> Option<Track> {
        let text = String::from_utf8_lossy(data);

        let mut p = Parser {
            track: Track {
                kind: Kind::Unknown,
                play_res: (0, 0),
                layout_res: (0, 0),
                wrap_style: 0,
                scaled_border_and_shadow: false,
                kerning: false,
                language: None,
                styles: vec![Style::default_style()],
                events: Vec::new(),
                default_style: 0,
                fonts: Vec::new(),
            },
            section: Section::None,
            style_format: None,
            event_format: None,
            flags: 0,
            font_name: None,
            font_data: Vec::new(),
        };

        for line in text.split(['\r', '\n']) {
            let line = line.strip_prefix('\u{FEFF}').unwrap_or(line);

            if !line.is_empty() {
                p.line(line);
            }
        }

        if p.font_name.is_some() {
            p.decode_font();
        }

        let mut track = p.track;

        if track.kind == Kind::Unknown {
            return None;
        }

        for (i, e) in track.events.iter_mut().enumerate() {
            e.read_order = i;
        }

        track.play_res = play_res(track.play_res);
        Some(track)
    }

    /// The fonts in the script's `[Fonts]` section, by name.
    pub fn fonts(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.fonts.iter().map(|(n, d)| (n.as_str(), d.as_slice()))
    }

    pub fn styles(&self) -> &[Style] {
        &self.styles
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    /// The canvas the script is laid out on.
    pub fn play_res(&self) -> (i32, i32) {
        self.play_res
    }
}

fn play_res((x, y): (i32, i32)) -> (i32, i32) {
    match (x > 0, y > 0) {
        (true, true) => (x, y),
        (false, false) => (384, 288),
        (true, false) if x == 1280 => (1280, 1024),
        (true, false) => (x, ((x as i64 * 3 / 4) as i32).max(1)),
        (false, true) if y == 1024 => (1280, 1024),
        (false, true) => (((y as i64 * 4 / 3) as i32).max(1), y),
    }
}

impl Parser {
    fn line(&mut self, line: &str) {
        let line = skip_spaces(line);

        let header = |name: &str| line.len() >= name.len() && line[..name.len()].eq_ignore_ascii_case(name);

        if header("[Script Info]") {
            self.section = Section::Info;
        } else if header("[V4 Styles]") {
            self.section = Section::Styles;
            self.track.kind = Kind::Ssa;
        } else if header("[V4+ Styles]") {
            self.section = Section::Styles;
            self.track.kind = Kind::Ass;
        } else if header("[Events]") {
            self.section = Section::Events;
        } else if header("[Fonts]") {
            self.section = Section::Fonts;
        } else {
            match self.section {
                Section::Info => self.info(line),
                Section::Styles => self.styles(line),
                Section::Events => self.events(line),
                Section::Fonts => self.fonts(line),
                Section::None => {},
            }
        }
    }

    fn info(&mut self, line: &str) {
        let t = &mut self.track;

        if let Some(v) = line.strip_prefix("PlayResX:") {
            t.play_res.0 = int_header(v);
            self.flags |= SINFO_PLAYRESX;
        } else if let Some(v) = line.strip_prefix("PlayResY:") {
            t.play_res.1 = int_header(v);
            self.flags |= SINFO_PLAYRESY;
        } else if let Some(v) = line.strip_prefix("LayoutResX:") {
            t.layout_res.0 = int_header(v);
            self.flags |= SINFO_LAYOUTRESX;
        } else if let Some(v) = line.strip_prefix("LayoutResY:") {
            t.layout_res.1 = int_header(v);
            self.flags |= SINFO_LAYOUTRESY;
        } else if line.starts_with("Timer:") {
            self.flags |= SINFO_TIMER;
        } else if let Some(v) = line.strip_prefix("WrapStyle:") {
            t.wrap_style = int_header(v);
            self.flags |= SINFO_WRAPSTYLE;
        } else if let Some(v) = line.strip_prefix("ScaledBorderAndShadow:") {
            t.scaled_border_and_shadow = bool_header(v);
            self.flags |= SINFO_SCALEDBORDER;
        } else if let Some(v) = line.strip_prefix("Kerning:") {
            t.kerning = bool_header(v);
            self.flags |= SINFO_KERNING;
        } else if line.starts_with("YCbCr Matrix:") {
            self.flags |= SINFO_COLOURMATRIX;
        } else if let Some(v) = line.strip_prefix("Language:") {
            let v = v.trim_start_matches(|c: char| c.is_ascii_whitespace());
            t.language = Some(v.chars().take(2).collect());
            self.flags |= SINFO_LANGUAGE;
        } else if let Some(v) = line.strip_prefix("ScriptType:") {
            self.flags |= SINFO_SCRIPTTYPE;
            // VSFilter doesn't look for the leading v, only at the end of the value.
            let v = trim_end_spaces(v);

            if v.len() >= 4 {
                let (kind, v) = match v.strip_suffix('+') {
                    Some(v) => (Kind::Ass, v),
                    None => (Kind::Ssa, v),
                };

                if v.ends_with("4.00") {
                    t.kind = kind;
                }
            }
        } else if line.starts_with("; Script generated by FFmpeg/Lavc") {
            self.flags |= GENBY_FFMPEG;
        }
    }

    fn styles(&mut self, line: &str) {
        if let Some(f) = line.strip_prefix("Format:") {
            let f = skip_spaces(f).to_string();
            let std = if self.track.kind == Kind::Ass { ASS_STYLE_FORMAT } else { SSA_STYLE_FORMAT };
            self.custom_format(&f, std);
            self.style_format = Some(f);
        } else if let Some(s) = line.strip_prefix("Style:") {
            self.style(skip_spaces(s));
        }
    }

    /// Scripts with their own format lines default to scaled borders, as libass always did for
    /// them.
    fn custom_format(&mut self, format: &str, std: &str) {
        if self.flags & SINFO_SCALEDBORDER == 0 && !same_format(format, std) {
            self.track.scaled_border_and_shadow = true;
        }
    }

    fn style(&mut self, line: &str) {
        let ssa = self.track.kind == Kind::Ssa;

        let format = self
            .style_format
            .get_or_insert_with(|| if ssa { SSA_STYLE_FORMAT } else { ASS_STYLE_FORMAT }.to_string())
            .clone();

        let mut s = Style::new();
        let mut font_name = None;
        let mut ssa_alpha = 0;
        let (mut names, mut values) = (format.as_str(), line);

        loop {
            let Some(name) = next_token(&mut names, true) else { break };
            let Some(value) = next_token(&mut values, false) else { break };

            match name.to_ascii_lowercase().as_str() {
                "name" => s.name = value.trim_start_matches('*').to_string(),
                "fontname" => font_name = Some(value.to_string()),
                "primarycolour" => s.colors[0] = color_header(value),
                "secondarycolour" => s.colors[1] = color_header(value),
                "outlinecolour" | "tertiarycolour" => s.colors[2] = color_header(value),
                "backcolour" => {
                    s.colors[3] = color_header(value);

                    // SSA's BackColour is its outline's as well as its shadow's.
                    if ssa {
                        s.colors[2] = s.colors[3];
                    }
                },
                "alphalevel" => ssa_alpha = int_header(value),
                "fontsize" => s.font_size = text::atof(value),
                "bold" => s.bold = int_header(value),
                "italic" => s.italic = int_header(value),
                "underline" => s.underline = int_header(value) != 0,
                "strikeout" => s.strike_out = int_header(value) != 0,
                "spacing" => s.spacing = text::atof(value),
                "angle" => s.angle = text::atof(value),
                "borderstyle" => s.border_style = int_header(value),
                "alignment" => {
                    let a = int_header(value);

                    s.alignment = match a {
                        _ if !ssa => numpad_to_align(a),
                        // VSFilter's own reading of SSA's illegal values.
                        8 => 3,
                        4 => 11,
                        a => a,
                    };
                },
                "marginl" => s.margin_l = int_header(value),
                "marginr" => s.margin_r = int_header(value),
                "marginv" => s.margin_v = int_header(value),
                "encoding" => s.encoding = int_header(value),
                "scalex" => s.scale_x = text::atof(value),
                "scaley" => s.scale_y = text::atof(value),
                "outline" => s.outline = text::atof(value),
                "shadow" => s.shadow = text::atof(value),
                _ => {},
            }
        }

        if ssa {
            let front = ssa_alpha.clamp(0, 0xFF) as u32;

            for c in &mut s.colors[..3] {
                *c = (*c & 0xFFFFFF00) | front;
            }

            s.colors[3] = (s.colors[3] & 0xFFFFFF00) | 0x80;
        }

        s.scale_x = s.scale_x.max(0.0) / 100.0;
        s.scale_y = s.scale_y.max(0.0) / 100.0;
        s.spacing = s.spacing.max(0.0);
        s.outline = s.outline.max(0.0);
        s.shadow = s.shadow.max(0.0);
        s.bold = (s.bold != 0) as i32;
        s.italic = (s.italic != 0) as i32;

        if s.name.is_empty() {
            s.name = "Default".into();
        }

        s.font_name = font_name.unwrap_or_else(|| "Arial".into());

        if s.name == "Default" {
            self.track.default_style = self.track.styles.len();
        }

        self.track.styles.push(s);
    }

    fn events(&mut self, line: &str) {
        if let Some(f) = line.strip_prefix("Format:") {
            let f = skip_spaces(f).to_string();
            let std = if self.track.kind == Kind::Ass { ASS_EVENT_FORMAT } else { SSA_EVENT_FORMAT };
            self.custom_format(&f, std);
            self.event_format = Some(f);

            if self.legacy_ffmpeg() {
                self.track.scaled_border_and_shadow = true;
            }
        } else if let Some(rest) = line.strip_prefix("Dialogue:") {
            let ssa = self.track.kind == Kind::Ssa;

            let format = self
                .event_format
                .get_or_insert_with(|| if ssa { SSA_EVENT_FORMAT } else { ASS_EVENT_FORMAT }.to_string())
                .clone();

            if let Some(e) = self.event(&format, skip_spaces(rest)) {
                self.track.events.push(e);
            }
        }
    }

    /// FFmpeg's conversions from before it wrote ScaledBorderAndShadow expected it to be on.
    fn legacy_ffmpeg(&self) -> bool {
        self.flags == SINFO_SCRIPTTYPE | SINFO_PLAYRESX | SINFO_PLAYRESY | GENBY_FFMPEG
            && self.track.styles.len() == 2
            && self.track.styles[1].name.starts_with("Default")
    }

    fn event(&self, format: &str, line: &str) -> Option<Event> {
        let mut e = Event {
            layer: 0,
            start: 0,
            duration: 0,
            style: 0,
            name: String::new(),
            margin_l: 0,
            margin_r: 0,
            margin_v: 0,
            effect: String::new(),
            text: String::new(),
            read_order: 0,
        };

        let (mut names, mut values) = (format, line);
        let mut end = 0;

        loop {
            let name = next_token(&mut names, true)?;

            if name.eq_ignore_ascii_case("Text") {
                e.text = values.trim_end_matches(['\r', '\t', ' ']).to_string();
                e.duration = end - e.start;
                return Some(e);
            }

            let value = next_token(&mut values, false)?;

            match name.to_ascii_lowercase().as_str() {
                "layer" => e.layer = int_header(value),
                "style" => e.style = self.lookup_style(value),
                "name" | "actor" => e.name = value.to_string(),
                "effect" => e.effect = value.to_string(),
                "marginl" => e.margin_l = int_header(value),
                "marginr" => e.margin_r = int_header(value),
                "marginv" => e.margin_v = int_header(value),
                "start" => e.start = timecode(value),
                "end" => end = timecode(value),
                _ => {},
            }
        }
    }

    fn lookup_style(&self, name: &str) -> usize {
        let name = name.trim_start_matches('*');
        let name = if name.eq_ignore_ascii_case("Default") { "Default" } else { name };
        self.track.styles.iter().rposition(|s| s.name == name).unwrap_or(self.track.default_style)
    }

    fn fonts(&mut self, line: &str) {
        if let Some(name) = line.strip_prefix("fontname:") {
            if self.font_name.is_some() {
                self.decode_font();
            }

            self.font_name = Some(skip_spaces(name).to_string());
        } else if self.font_name.is_some() {
            self.font_data.extend_from_slice(line.as_bytes());
        }
    }

    /// Fonts are uuencoded, without line lengths.
    fn decode_font(&mut self) {
        let name = self.font_name.take().unwrap_or_default();
        let data = std::mem::take(&mut self.font_data);

        if data.len() % 4 == 1 {
            return;
        }

        let mut out = Vec::with_capacity(data.len() / 4 * 3 + 2);

        for chunk in data.chunks(4) {
            let mut value = 0u32;

            for (i, &c) in chunk.iter().enumerate() {
                value |= ((c.wrapping_sub(33) as u32) & 63) << (6 * (3 - i));
            }

            out.push((value >> 16) as u8);

            if chunk.len() >= 3 {
                out.push((value >> 8) as u8);
            }

            if chunk.len() >= 4 {
                out.push(value as u8);
            }
        }

        self.track.fonts.push((name, out));
    }
}

/// libass's numbering of `\an` alignments.
pub(crate) fn numpad_to_align(val: i32) -> i32 {
    let val = if val < -i32::MAX { 2 } else { val.abs() };

    let h = (val - 1).rem_euclid(3) + 1;

    match val {
        ..=3 => h | VALIGN_SUB,
        4..=6 => h | VALIGN_CENTER,
        _ => h | VALIGN_TOP,
    }
}

/// The next comma-separated token, with leading spaces skipped and, for names, trailing ones too.
fn next_token<'a>(s: &mut &'a str, rtrim: bool) -> Option<&'a str> {
    let rest = skip_spaces(s);

    if rest.is_empty() {
        return None;
    }

    let (token, next) = match rest.find(',') {
        Some(i) => (&rest[..i], &rest[i + 1..]),
        None => (rest, ""),
    };

    *s = next;
    Some(if rtrim { trim_end_spaces(token) } else { token })
}

fn same_format(a: &str, b: &str) -> bool {
    let tokens = |s: &str| {
        s.split(',')
            .map(|t| {
                let t = t.trim_matches([' ', '\t']);
                if t == "Actor" { "Name".to_string() } else { t.to_ascii_lowercase() }
            })
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
    };

    tokens(a) == tokens(b)
}

/// An integer as VSFilter reads one: decimal, or hexadecimal after `&H` or `0x`, wrapping around.
pub(crate) fn int_header(s: &str) -> i32 {
    let (s, base) = match s.get(..2) {
        Some(p) if p.eq_ignore_ascii_case("&h") || p.eq_ignore_ascii_case("0x") => (&s[2..], 16),
        _ => (s, 10),
    };

    text::strtou32_modulo(s, base).0 as i32
}

/// A colour as `0xRRGGBBAA`, from the script's `&HAABBGGRR`.
fn color_header(s: &str) -> u32 {
    (int_header(s) as u32).swap_bytes()
}

fn bool_header(s: &str) -> bool {
    let s = skip_spaces(s);
    s.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("yes")) || text::strtol(s) > 0
}

fn timecode(s: &str) -> i64 {
    let mut rest = s;
    let mut parts = [0i64; 4];

    for (i, part) in parts.iter_mut().enumerate() {
        if i > 0 {
            match rest.strip_prefix(if i == 3 { '.' } else { ':' }) {
                Some(r) => rest = r,
                None => return 0,
            }
        }

        let (value, len) = text::scan_int(rest);

        if len == 0 {
            return 0;
        }

        *part = value;
        rest = &rest[len..];
    }

    ((parts[0] * 60 + parts[1]) * 60 + parts[2]) * 1000 + parts[3] * 10
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCRIPT: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\n\n[V4+ Styles]\n\
                          Format: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, \
                          BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, \
                          BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\n\
                          Style: Default,Arial,72,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,-1,0,0,0,100,100,0,\
                          0,1,3,2,2,40,40,40,1\n\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, \
                          MarginR, MarginV, Effect, Text\nDialogue: 1,0:00:01.50,0:00:04.00,Default,,0,0,0,,\
                          Hello, {\\b1}world  \n";

    #[test]
    fn reads_a_script() {
        let t = Track::parse(SCRIPT.as_bytes()).unwrap();
        assert_eq!(t.play_res, (1920, 1080));
        assert_eq!(t.styles.len(), 2);

        let s = &t.styles[1];
        assert_eq!(s.font_size, 72.0);
        assert_eq!(s.colors[0], 0xFFFFFF00);
        assert_eq!(s.colors[3], 0x00000080);
        assert_eq!(s.bold, 1);
        assert_eq!(s.alignment, 2);

        let e = &t.events[0];
        assert_eq!((e.start, e.duration, e.layer, e.style), (1500, 2500, 1, 1));
        assert_eq!(e.text, "Hello, {\\b1}world");
        assert!(!t.scaled_border_and_shadow);
    }

    #[test]
    fn fills_in_play_res() {
        assert_eq!(play_res((0, 0)), (384, 288));
        assert_eq!(play_res((1280, 0)), (1280, 1024));
        assert_eq!(play_res((640, 0)), (640, 480));
        assert_eq!(play_res((0, 720)), (960, 720));
    }

    #[test]
    fn numpad_alignment() {
        assert_eq!(numpad_to_align(2), HALIGN_CENTER | VALIGN_SUB);
        assert_eq!(numpad_to_align(7), HALIGN_LEFT | VALIGN_TOP);
        assert_eq!(numpad_to_align(6), HALIGN_RIGHT | VALIGN_CENTER);
    }

    #[test]
    fn uudecodes_fonts() {
        // "Cat" uuencoded the way scripts embed fonts.
        let t = Track::parse(b"[Script Info]\nScriptType: v4.00+\n[Fonts]\nfontname: a.ttf\n1W&U\n").unwrap();
        let (name, data) = t.fonts().next().unwrap();
        assert_eq!(name, "a.ttf");
        assert_eq!(&data[..3], b"Cat");
    }

    #[test]
    fn rejects_other_files() {
        assert!(Track::parse(b"1\n00:00:01,000 --> 00:00:02,000\nHi\n").is_none());
    }
}
