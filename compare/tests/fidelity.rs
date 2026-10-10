// SPDX-License-Identifier: LGPL-3.0-or-later
//! rustyass against libass on scripts that each exercise a feature. Side-by-side PNGs of every
//! case go to `target/rustyass-compare`.

use std::path::PathBuf;

use rustyass::Margins;
use rustyass_compare::{Libass, Rusty, composite, diff, scripts, write_png};

const W: i32 = 640;
const H: i32 = 360;

fn font() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/NotoSans-Regular.ttf");
    std::fs::read(path).unwrap()
}

fn script(style: &str, events: &[&str]) -> String {
    let mut s = String::from(
        "[Script Info]\nScriptType: v4.00+\nPlayResX: 640\nPlayResY: 360\nScaledBorderAndShadow: yes\n\n\
         [V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, \
         Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, \
         Alignment, MarginL, MarginR, MarginV, Encoding\n",
    );
    s += &format!("Style: Default,Noto Sans,{style}\n\n");
    s += "[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

    for e in events {
        s += &format!("Dialogue: 0,0:00:00.00,0:00:10.00,Default,,0,0,0,,{e}\n");
    }

    s
}

const PLAIN: &str = "40,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,0,0,2,20,20,20,1";
const BORDERED: &str = "40,&H00FFFFFF,&H000000FF,&H00000000,&H80000000,0,0,0,0,100,100,0,0,1,3,2,2,20,20,20,1";

/// Renders both at `ms` and checks how far apart they are.
fn check(name: &str, script: &str, ms: i64, max_over16: f32) {
    check_at(name, script, ms, (W, H), max_over16);
}

fn check_at(name: &str, script: &str, ms: i64, (w, h): (i32, i32), max_over16: f32) {
    let fonts = [font()];
    let mut a = Libass::new(script.as_bytes(), &fonts, Some("Noto Sans")).unwrap();
    let mut b = Rusty::new(script.as_bytes(), &fonts, Some("Noto Sans")).unwrap();
    a.set_frame((w, h), (w, h), Margins::default());
    b.set_frame((w, h), (w, h), Margins::default());

    let (ia, _) = a.render(ms);
    let (ib, _) = b.render(ms);
    let (ca, cb) = (composite(w, h, &ia), composite(w, h, &ib));
    let d = diff(&ca, &cb);

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/rustyass-compare");
    std::fs::create_dir_all(&dir).unwrap();
    write_png(&dir.join(format!("{name}.png")), w, h, &ca, &cb);
    eprintln!("{name}: libass {} images, rustyass {}; {d:?}", ia.len(), ib.len());

    assert!(d.drawn > 0, "{name}: nothing drawn");
    assert!(d.over16 <= max_over16, "{name}: {:.2}% of pixels off by more than 16", d.over16 * 100.0);
}

#[test]
fn plain() {
    check("plain", &script(PLAIN, &["Hello, world!"]), 1000, 0.03);
}

#[test]
fn border_and_shadow() {
    check("border", &script(BORDERED, &["Hello, world!"]), 1000, 0.05);
}

#[test]
fn wrapping() {
    let long = "This line is long enough that it has to wrap onto a second line, and maybe a third one too.";
    check("wrap", &script(PLAIN, &[long]), 1000, 0.05);
}

#[test]
fn positioning_and_alignment() {
    check("pos", &script(PLAIN, &["{\\an7\\pos(40,40)}Top left", "{\\an5}Middle", "{\\an3}Bottom right"]), 1000, 0.05);
}

#[test]
fn blur() {
    check("blur", &script(BORDERED, &["{\\blur4}Blurred", "{\\an8\\be3}Edges"]), 1000, 0.08);
}

#[test]
fn rotation() {
    check("rotation", &script(BORDERED, &["{\\an5\\frz30}Rotated", "{\\an8\\fry40\\frx20}Perspective"]), 1000, 0.08);
}

#[test]
fn colors_and_alpha() {
    check("colors", &script(BORDERED, &["{\\c&H0000FF&\\3c&HFF0000&\\alpha&H40&}Coloured"]), 1000, 0.08);
}

#[test]
fn drawing() {
    check("drawing", &script(BORDERED, &["{\\an7\\pos(100,100)\\p1}m 0 0 l 100 0 100 100 0 100{\\p0}"]), 1000, 0.03);
}

#[test]
fn clips() {
    check(
        "clip",
        &script(
            PLAIN,
            &["{\\clip(0,300,320,360)}Clipped in half", "{\\an8\\iclip(m 300 0 l 340 0 340 100 300 100)}Inverse"],
        ),
        1000,
        0.05,
    );
}

#[test]
fn karaoke() {
    check("karaoke", &script(BORDERED, &["{\\k50}Ka{\\kf100}ra{\\ko100}o{\\k50}ke"]), 1250, 0.06);
}

#[test]
fn fade_and_move() {
    check("fade", &script(BORDERED, &["{\\fad(500,500)\\move(100,100,500,300)}Moving"]), 300, 0.08);
}

#[test]
fn collisions() {
    check("collisions", &script(PLAIN, &["First", "Second", "Third"]), 1000, 0.05);
}

#[test]
fn benchmark_scripts() {
    check_at("bench-dialogue", &scripts::dialogue(10), 5200, (1920, 1080), 0.05);
    check_at("bench-typesetting", &scripts::typesetting(10), 5600, (1920, 1080), 0.1);
    check_at("bench-karaoke", &scripts::karaoke(10), 5300, (1920, 1080), 0.15);
}
