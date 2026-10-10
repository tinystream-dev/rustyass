// SPDX-License-Identifier: LGPL-3.0-or-later
//! 1080p scripts made up to look like what fansubs ship: dialogue, heavy typesetting and karaoke,
//! the same every time they're made.

use std::fmt::Write;

/// Frames a second of the video the scripts are timed to.
pub const FPS: f64 = 24000.0 / 1001.0;

const HEADER: &str = "[Script Info]\nScriptType: v4.00+\nPlayResX: 1920\nPlayResY: 1080\n\
    ScaledBorderAndShadow: yes\nWrapStyle: 0\n\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, \
    PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, \
    ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, \
    MarginV, Encoding\n\
    Style: Default,Noto Sans,64,&H00FFFFFF,&H000000FF,&H00101010,&H96000000,0,0,0,0,100,100,0,0,1,3.2,1.6,2,120,120,50,1\n\
    Style: Sign,Noto Sans,56,&H00F0F0F0,&H000000FF,&H00402020,&H00000000,1,0,0,0,100,100,0,0,1,2,0,5,0,0,0,1\n\
    Style: Kara,Noto Sans,52,&H00FFFFFF,&H00FF8000,&H00301000,&H00000000,1,0,0,0,100,100,0,0,1,2.5,0,8,30,30,30,1\n\n\
    [Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n";

const WORDS: &[&str] = &[
    "the",
    "you",
    "never",
    "said",
    "that",
    "we",
    "would",
    "come",
    "back",
    "here",
    "after",
    "everything",
    "happened",
    "tonight",
    "I",
    "know",
    "but",
    "it's",
    "not",
    "like",
    "there",
    "was",
    "another",
    "way",
    "out",
    "of",
    "this",
    "place",
    "so",
    "just",
    "trust",
    "me",
    "for",
    "once",
    "alright",
    "everyone",
];

const SYLLABLES: &[&str] =
    &["ka", "ra", "o", "ke", "shi", "n", "to", "ki", "no", "mi", "yu", "me", "ha", "na", "sa", "ku", "ri", "ta"];

/// A linear congruential generator: enough to vary the scripts, and the same on every machine.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }

    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next() as f64 / (1u64 << 31) as f64
    }

    fn pick<'a>(&mut self, of: &[&'a str]) -> &'a str {
        of[self.below(of.len() as u32) as usize]
    }
}

fn time(ms: f64) -> String {
    let cs = (ms / 10.0).round() as u64;
    format!("{}:{:02}:{:02}.{:02}", cs / 360000, cs / 6000 % 60, cs / 100 % 60, cs % 100)
}

fn event(s: &mut String, layer: u32, start: f64, end: f64, style: &str, text: &str) {
    writeln!(s, "Dialogue: {layer},{},{},{style},,0,0,0,,{text}", time(start), time(end)).unwrap();
}

fn sentence(rng: &mut Rng, words: u32) -> String {
    (0..words).map(|_| rng.pick(WORDS)).collect::<Vec<_>>().join(" ")
}

/// A line every 2.5 s, some long enough to wrap, some italic, some with a second speaker at once.
pub fn dialogue(secs: u32) -> String {
    let mut s = String::from(HEADER);
    let mut rng = Rng(1);

    for i in 0..secs * 2 / 5 {
        let start = i as f64 * 2500.0;
        let words = 4 + rng.below(14);
        let mut text = sentence(&mut rng, words);

        if rng.below(6) == 0 {
            text = format!("{{\\i1}}{text}");
        }

        event(&mut s, 0, start, start + 2400.0, "Default", &text);

        if rng.below(8) == 0 {
            let words = 3 + rng.below(5);
            let other = sentence(&mut rng, words);
            event(&mut s, 0, start + 400.0, start + 2400.0, "Default", &other);
        }
    }

    s
}

/// A dozen signs at once, blurred, rotated, clipped and animated, a motion-tracked one redone
/// every frame, and shapes behind them.
pub fn typesetting(secs: u32) -> String {
    let mut s = String::from(HEADER);
    let mut rng = Rng(2);

    for sec in 0..secs {
        let t = sec as f64 * 1000.0;

        for _ in 0..4 {
            let (x, y) = (rng.range(200.0, 1720.0), rng.range(100.0, 980.0));
            let mut tags = format!(
                "\\pos({x:.1},{y:.1})\\frz{:.1}\\blur{:.1}\\bord{:.1}\\fad(200,200)\\1c&H{:06X}&",
                rng.range(-30.0, 30.0),
                rng.range(0.6, 3.0),
                rng.range(0.0, 4.0),
                rng.next() & 0xFFFFFF,
            );

            match rng.below(8) {
                0..=3 => {
                    let (w, h) = (rng.range(80.0, 300.0), rng.range(20.0, 60.0));
                    write!(tags, "\\clip({:.0},{:.0},{:.0},{:.0})", x - w, y - h, x + w, y + h).unwrap();
                },
                4 | 5 => write!(
                    tags,
                    "\\clip(m {:.0} {:.0} l {:.0} {:.0} {:.0} {:.0} {:.0} {:.0})",
                    x - 250.0,
                    y - 40.0,
                    x + 200.0,
                    y - 60.0,
                    x + 260.0,
                    y + 50.0,
                    x - 180.0,
                    y + 30.0
                )
                .unwrap(),
                6 => write!(tags, "\\iclip({:.0},{:.0},{:.0},{:.0})", x - 40.0, y - 80.0, x + 40.0, y + 80.0).unwrap(),
                _ => {},
            }

            let words = 1 + rng.below(4);
            event(&mut s, 1, t, t + 3000.0, "Sign", &format!("{{{tags}}}{}", sentence(&mut rng, words)));
        }

        let (x, y) = (rng.range(300.0, 1600.0), rng.range(200.0, 900.0));
        event(
            &mut s,
            2,
            t,
            t + 2000.0,
            "Sign",
            &format!(
                "{{\\move({x:.0},{y:.0},{:.0},{:.0})\\blur1.5\\t(0,1500,\\frz360\\fscx150\\fscy150\\1c&H3060FF&)}}{}",
                x + 200.0,
                y - 100.0,
                sentence(&mut rng, 2)
            ),
        );

        event(
            &mut s,
            2,
            t,
            t + 1000.0,
            "Sign",
            &format!(
                "{{\\pos({:.0},{:.0})\\org(960,540)\\frx{:.0}\\fry{:.0}\\blur0.8\\be1}}{}",
                rng.range(400.0, 1500.0),
                rng.range(200.0, 900.0),
                rng.range(-40.0, 40.0),
                rng.range(-50.0, 50.0),
                sentence(&mut rng, 3)
            ),
        );

        let (x, y) = (rng.range(300.0, 1600.0), rng.range(200.0, 900.0));
        event(
            &mut s,
            0,
            t,
            t + 2000.0,
            "Sign",
            &format!(
                "{{\\an7\\pos({x:.0},{y:.0})\\blur5\\bord0\\1c&H202020&\\1a&H60&\\p1}}m 0 20 b 0 0 20 0 40 0 l 400 0 \
                 b 420 0 440 0 440 20 l 440 100 b 440 120 420 120 400 120 l 40 120 b 20 120 0 120 0 100{{\\p0}}"
            ),
        );

        let frames = (FPS * (sec + 1) as f64).floor() as u32 - (FPS * sec as f64).floor() as u32;
        let first = (FPS * sec as f64).floor() as u32;
        let words = sentence(&mut rng, 2);

        for f in first..first + frames {
            let (start, end) = (f as f64 * 1000.0 / FPS, (f + 1) as f64 * 1000.0 / FPS);
            let k = f as f64;
            event(
                &mut s,
                3,
                start,
                end,
                "Sign",
                &format!(
                    "{{\\pos({:.2},{:.2})\\fscx{:.2}\\fscy{:.2}\\frz{:.2}\\blur1}}{words}",
                    800.0 + 300.0 * (k * 0.05).sin(),
                    400.0 + 120.0 * (k * 0.03).cos(),
                    100.0 + 10.0 * (k * 0.1).sin(),
                    100.0 + 10.0 * (k * 0.1).sin(),
                    5.0 * (k * 0.07).sin()
                ),
            );
        }
    }

    s
}

/// A karaoke line every 4 s, its syllables swept with `\kf`, each highlighted by an event of its
/// own as it's sung, and a translation under it.
pub fn karaoke(secs: u32) -> String {
    let mut s = String::from(HEADER);
    let mut rng = Rng(3);

    for i in 0..secs / 4 {
        let start = i as f64 * 4000.0;
        let n = 8 + rng.below(5);
        let syllables: Vec<(&str, u32)> = (0..n).map(|_| (rng.pick(SYLLABLES), 20 + rng.below(30))).collect();
        let line: String = syllables.iter().map(|(syl, k)| format!("{{\\kf{k}}}{syl}")).collect();
        event(&mut s, 0, start, start + 3900.0, "Kara", &format!("{{\\fad(150,150)}}{line}"));

        let width: usize = syllables.iter().map(|(syl, _)| syl.len()).sum();
        let mut x = 960.0 - width as f64 * 15.0;
        let mut at = start;

        for (syl, k) in &syllables {
            let (w, dur) = (syl.len() as f64 * 30.0, *k as f64 * 10.0);
            event(
                &mut s,
                1,
                at,
                at + dur + 300.0,
                "Kara",
                &format!(
                    "{{\\an5\\pos({:.1},60)\\1c&H80FFFF&\\3c&H0060FF&\\t(0,{:.0},\\fscx135\\fscy135\\blur4)\
                     \\t({:.0},{:.0},\\fscx100\\fscy100\\blur0\\alpha&HFF&)}}{syl}",
                    x + w / 2.0,
                    dur / 2.0,
                    dur / 2.0,
                    dur + 300.0
                ),
            );
            x += w;
            at += dur;
        }

        let words = 5 + rng.below(8);
        event(&mut s, 0, start, start + 3900.0, "Default", &sentence(&mut rng, words));
    }

    s
}
