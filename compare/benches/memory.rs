// SPDX-License-Identifier: LGPL-3.0-or-later
//! How much memory a renderer holds over a long script, frame after frame: `cargo bench -p
//! rustyass-compare --bench memory -- <libass|rustyass> [script] [minutes]`. Each renderer runs in
//! a process of its own, so the resident sizes are its alone.

use std::path::PathBuf;

use rustyass::Margins;
use rustyass_compare::scripts::{self, FPS};
use rustyass_compare::{Libass, Rusty};

/// Resident and peak resident memory, in MB.
fn resident() -> (f64, f64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = |key: &str| {
        status.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.split_whitespace().next()?.parse::<f64>().ok())
    };
    (kb("VmRSS:").unwrap_or(0.0) / 1024.0, kb("VmHWM:").unwrap_or(0.0) / 1024.0)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
    let renderer = args.first().map_or("rustyass", String::as_str);
    let script = args.get(1).map_or("typesetting", String::as_str);
    let minutes: u32 = args.get(2).map_or(24, |s| s.parse().expect("minutes"));

    let font = std::env::var_os("RUSTYASS_FONT")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/NotoSans-Regular.ttf"));
    let fonts = [std::fs::read(&font).unwrap_or_else(|e| panic!("{}: {e}", font.display()))];
    let text = match script {
        "dialogue" => scripts::dialogue(minutes * 60),
        "karaoke" => scripts::karaoke(minutes * 60),
        _ => scripts::typesetting(minutes * 60),
    };

    let (before, _) = resident();
    let mut draw: Box<dyn FnMut(i64)> = match renderer {
        "libass" => {
            let mut r = Libass::new(text.as_bytes(), &fonts, Some("Noto Sans")).unwrap();
            r.set_frame((1920, 1080), (1920, 1080), Margins::default());
            Box::new(move |ms| {
                r.draw(ms);
            })
        },
        _ => {
            let mut r = Rusty::new(text.as_bytes(), &fonts, Some("Noto Sans")).unwrap();
            r.set_frame((1920, 1080), (1920, 1080), Margins::default());
            Box::new(move |ms| {
                r.draw(ms);
            })
        },
    };

    println!("{renderer}, {script}, {minutes} min at 1920×1080; MB resident, beyond the {before:.0} before it");
    println!("{:>6} {:>9} {:>9}", "minute", "resident", "peak");

    let frames = (minutes as f64 * 60.0 * FPS) as u32;
    let per_minute = (60.0 * FPS) as u32;

    for f in 0..frames {
        draw(((f as f64 + 0.5) * 1000.0 / FPS) as i64);

        if (f + 1) % per_minute == 0 {
            let (now, peak) = resident();
            println!("{:>6} {:>9.1} {:>9.1}", (f + 1) / per_minute, now - before, peak - before);
        }
    }
}
