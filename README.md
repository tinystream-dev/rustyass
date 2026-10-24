# rustyass

An ASS/SSA subtitle renderer in pure Rust, drawing what libass and VSFilter draw. It was written
for [tinystream](https://github.com/tinystream-dev/tinystream), where it renders subtitles for
burn-in, the web player (in WebAssembly) and the Android app.

## Nightly

rustyass is written for SIMD with `std::simd`, so it builds on nightly only. It's tested on
x86_64, aarch64 (Android) and wasm32 with `simd128` (`-C target-feature=+simd128`).

## Fidelity

`compare/` renders the same scripts with rustyass and libass 0.17.3 and checks how far apart they
are; `cargo test -p rustyass-compare` writes side-by-side PNGs of every case to
`target/rustyass-compare/`. The first run builds libass, FreeType, FriBidi, HarfBuzz and nasm into
`.native/` (or `RUSTYASS_NATIVE_DIR`), which needs curl, make, meson and ninja.

The same crate benchmarks both for speed and memory:

```sh
cargo bench -p rustyass-compare --bench render -- [script] [seconds] [--only=rustyass]
cargo bench -p rustyass-compare --bench memory -- <libass|rustyass> [script] [minutes]
```

## License

rustyass is free software under the GNU Lesser General Public License, version 3 or later (see
`COPYING.LESSER` and `COPYING`). The Noto Sans in `compare/fonts/`, used by the tests, is under the
SIL Open Font License (`compare/fonts/OFL.txt`).
