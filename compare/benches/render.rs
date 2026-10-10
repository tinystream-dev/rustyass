// SPDX-License-Identifier: LGPL-3.0-or-later
//! rustyass against libass, frame by frame, on 1080p scripts: with warm caches (every frame drawn
//! once before) and cold ones (emptied before each frame). `cargo bench -p rustyass-compare --
//! [script] [seconds]` runs the scripts whose name has `script` in it, `seconds` of each;
//! `--only=rustyass` or `--only=libass` runs just one, for profiling.

use std::path::PathBuf;
use std::time::Instant;

use rustyass::Margins;
use rustyass_compare::scripts::{self, FPS};
use rustyass_compare::{Libass, Rusty};

const SIZE: (i32, i32) = (1920, 1080);

trait Draw {
    fn draw(&mut self, ms: i64) -> usize;
    fn clear_caches(&mut self);
}

impl Draw for Libass {
    fn draw(&mut self, ms: i64) -> usize {
        Libass::draw(self, ms)
    }

    fn clear_caches(&mut self) {
        Libass::clear_caches(self)
    }
}

impl Draw for Rusty {
    fn draw(&mut self, ms: i64) -> usize {
        Rusty::draw(self, ms)
    }

    fn clear_caches(&mut self) {
        Rusty::clear_caches(self)
    }
}

/// Milliseconds each frame took.
fn frames(r: &mut dyn Draw, times: &[i64], cold: bool) -> Vec<f64> {
    if !cold {
        for &ms in times {
            r.draw(ms);
        }
    }

    times
        .iter()
        .map(|&ms| {
            if cold {
                r.clear_caches();
            }

            let start = Instant::now();
            std::hint::black_box(r.draw(ms));
            start.elapsed().as_secs_f64() * 1000.0
        })
        .collect()
}

struct Stats {
    mean: f64,
    p50: f64,
    p95: f64,
    max: f64,
}

fn stats(mut t: Vec<f64>) -> Stats {
    t.sort_by(f64::total_cmp);
    let at = |q: f64| t[((t.len() - 1) as f64 * q).round() as usize];
    Stats { mean: t.iter().sum::<f64>() / t.len() as f64, p50: at(0.5), p95: at(0.95), max: t[t.len() - 1] }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
    let only = std::env::args().find_map(|a| a.strip_prefix("--only=").map(String::from));
    let filter = args.first().map_or("", String::as_str);
    let secs: u32 = args.get(1).map_or(60, |s| s.parse().expect("seconds"));

    let font = std::env::var_os("RUSTYASS_FONT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/NotoSans-Regular.ttf"));
    let fonts = [std::fs::read(&font).unwrap_or_else(|e| panic!("{}: {e}", font.display()))];
    let times: Vec<i64> = (0..(secs as f64 * FPS) as u32).map(|f| ((f as f64 + 0.5) * 1000.0 / FPS) as i64).collect();

    println!("{} frames of each, 1920×1080, ms per frame\n", times.len());
    println!(
        "{:<12} {:<5} {:<9} {:>7} {:>7} {:>7} {:>7} {:>16}",
        "script", "cache", "renderer", "mean", "p50", "p95", "max", "vs libass (mean, p50)"
    );

    type Make = fn(u32) -> String;
    let all: [(&str, Make); 3] =
        [("dialogue", scripts::dialogue), ("typesetting", scripts::typesetting), ("karaoke", scripts::karaoke)];

    for (name, make) in all.into_iter().filter(|(n, _)| n.contains(filter)) {
        let script = make(secs);

        for cold in [false, true] {
            let mut libass = Libass::new(script.as_bytes(), &fonts, Some("Noto Sans")).unwrap();
            let mut rusty = Rusty::new(script.as_bytes(), &fonts, Some("Noto Sans")).unwrap();
            libass.set_frame(SIZE, SIZE, Margins::default());
            rusty.set_frame(SIZE, SIZE, Margins::default());

            let run = |r: &str| only.as_ref().is_none_or(|o| o == r);
            let a = run("libass").then(|| stats(frames(&mut libass, &times, cold)));
            let b = run("rustyass").then(|| stats(frames(&mut rusty, &times, cold)));
            let cache = if cold { "cold" } else { "warm" };

            for (renderer, s) in [("libass", &a), ("rustyass", &b)] {
                let Some(s) = s else { continue };
                let ratio =
                    a.as_ref().map_or(String::new(), |a| format!("{:.2}× {:.2}×", s.mean / a.mean, s.p50 / a.p50));
                println!(
                    "{name:<12} {cache:<5} {renderer:<9} {:>7.3} {:>7.3} {:>7.3} {:>7.3} {ratio:>16}",
                    s.mean, s.p50, s.p95, s.max
                );
            }
        }
    }
}
