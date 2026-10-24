// SPDX-License-Identifier: LGPL-3.0-or-later
//! Builds libass statically, with the FreeType, FriBidi and HarfBuzz it needs and the nasm it's
//! assembled with, each into its own prefix under `.native` (or `RUSTYASS_NATIVE_DIR`). A library
//! is only built again when how it's configured changes.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::SystemTime;

const NASM: &str = "2.16.03";
const FREETYPE: &str = "2.13.3";
const FRIBIDI: &str = "1.0.16";
const HARFBUZZ: &str = "11.2.1";
const LIBASS: &str = env!("CARGO_PKG_VERSION");

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=RUSTYASS_NATIVE_DIR");

    let nasm = Build::new("nasm", NASM, &[]);

    nasm.once("", |b| {
        b.fetch(&format!("https://www.nasm.us/pub/nasm/releasebuilds/{NASM}/nasm-{NASM}.tar.gz"));
        b.configure(&[]);
    });

    let freetype = Build::new("freetype", FREETYPE, &[]);
    let options = [
        "--default-library=static",
        "-Dbrotli=disabled",
        "-Dbzip2=disabled",
        "-Dharfbuzz=disabled",
        "-Dpng=disabled",
        "-Dzlib=disabled",
    ];

    freetype.once(&options.join(" "), |b| {
        b.fetch(&format!("https://download.savannah.gnu.org/releases/freetype/freetype-{FREETYPE}.tar.gz"));
        b.meson(&options);
    });

    let fribidi = Build::new("fribidi", FRIBIDI, &[]);
    let options = ["--default-library=static", "-Ddocs=false", "-Dbin=false", "-Dtests=false"];

    fribidi.once(&options.join(" "), |b| {
        b.fetch(&format!("https://github.com/fribidi/fribidi/archive/refs/tags/v{FRIBIDI}.tar.gz"));
        b.meson(&options);
    });

    let harfbuzz = Build::new("harfbuzz", HARFBUZZ, &[&freetype]);
    let options = [
        "--default-library=static",
        "-Dfreetype=enabled",
        "-Dglib=disabled",
        "-Dgobject=disabled",
        "-Dcairo=disabled",
        "-Dicu=disabled",
        "-Dchafa=disabled",
        "-Dtests=disabled",
        "-Ddocs=disabled",
        "-Dutilities=disabled",
        "-Dintrospection=disabled",
    ];

    harfbuzz.once(&options.join(" "), |b| {
        b.fetch(&format!("https://github.com/harfbuzz/harfbuzz/archive/refs/tags/{HARFBUZZ}.tar.gz"));
        b.meson(&options);
    });

    let libass = Build::new("libass", LIBASS, &[&nasm, &freetype, &fribidi, &harfbuzz]);
    let args = [
        "--enable-static",
        "--disable-shared",
        "--with-pic",
        "--disable-fontconfig",
        "--disable-require-system-font-provider",
        "--disable-libunibreak",
        "CFLAGS=-O3 -fPIC",
    ];

    libass.once(&args.join(" "), |b| {
        b.fetch(&format!("https://github.com/libass/libass/releases/download/{LIBASS}/libass-{LIBASS}.tar.gz"));
        b.configure(&args);
    });

    println!("cargo::metadata=pkgconfig={}", env::join_paths(&libass.pkgconfig).unwrap().to_string_lossy());
}

struct Build {
    name: String,
    version: String,
    work: PathBuf,
    downloads: PathBuf,
    src: PathBuf,
    prefix: PathBuf,
    pkgconfig: Vec<PathBuf>,
    path: Vec<PathBuf>,
    started: SystemTime,
}

impl Build {
    fn new(name: &str, version: &str, deps: &[&Build]) -> Build {
        let started = SystemTime::now();
        let manifest = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());

        let root =
            env::var_os("RUSTYASS_NATIVE_DIR").map(PathBuf::from).unwrap_or_else(|| manifest.join("../../.native"));

        let work = root.join(format!("{name}-{version}"));
        let downloads = root.join("downloads");
        fs::create_dir_all(&work).expect("can't create the native build directory");
        fs::create_dir_all(&downloads).unwrap();
        let work = work.canonicalize().unwrap();
        let downloads = downloads.canonicalize().unwrap();
        let prefix = work.join("prefix");

        let mut pkgconfig = vec![prefix.join("lib/pkgconfig")];
        let mut path = vec![prefix.join("bin")];

        for dep in deps {
            pkgconfig.extend(dep.pkgconfig.iter().cloned());
            path.extend(dep.path.iter().cloned());
        }

        pkgconfig.dedup();
        path.dedup();

        Build {
            name: name.into(),
            version: version.into(),
            src: work.join("src"),
            work,
            downloads,
            prefix,
            pkgconfig,
            path,
            started,
        }
    }

    fn once(&self, config: &str, build: impl FnOnce(&Build)) {
        let marker = self.work.join(".done");
        println!("cargo::rerun-if-changed={}", marker.display());

        let config = format!("{config}\n{}", env::join_paths(&self.pkgconfig).unwrap().to_string_lossy());

        if fs::read_to_string(&marker).ok().as_deref() != Some(config.as_str()) {
            let _ = fs::remove_file(&marker);
            let _ = fs::remove_dir_all(&self.src);
            let _ = fs::remove_dir_all(&self.prefix);
            let _ = fs::remove_file(self.log());
            build(self);
            fs::write(&marker, &config).unwrap();

            File::options().write(true).open(&marker).unwrap().set_modified(self.started).unwrap();
        }
    }

    fn fetch(&self, url: &str) {
        let archive = self.downloads.join(format!("{}-{}.tar.gz", self.name, self.version));

        if !archive.exists() {
            let part = archive.with_extension("part");
            self.exec(Command::new("curl").args(["-fL", "--retry", "3", "-o"]).arg(&part).arg(url));

            if !fs::read(&part).is_ok_and(|bytes| bytes.starts_with(&[0x1F, 0x8B])) {
                let _ = fs::remove_file(&part);
                panic!("{url} didn't serve a gzip archive (maybe a bot check page) for {}", self.name);
            }

            fs::rename(&part, &archive).unwrap();
        }

        fs::create_dir_all(&self.src).unwrap();

        self.exec(Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&self.src).arg("--strip-components=1"));
    }

    fn cmd(&self, program: impl AsRef<OsStr>) -> Command {
        let mut cmd = Command::new(program);

        cmd.current_dir(&self.src)
            .env("PATH", prepend(&self.path, "PATH"))
            .env("PKG_CONFIG_PATH", prepend(&self.pkgconfig, "PKG_CONFIG_PATH"));

        cmd
    }

    fn exec(&self, cmd: &mut Command) {
        let log_path = self.log();
        let mut log = File::options().create(true).append(true).open(&log_path).unwrap();
        writeln!(log, "\n$ {cmd:?}").unwrap();

        let status = cmd
            .stdin(Stdio::null())
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .status()
            .unwrap_or_else(|e| panic!("can't run {:?} (needed to build {}): {e}", cmd.get_program(), self.name));

        if !status.success() {
            let log = fs::read_to_string(&log_path).unwrap_or_default();
            let lines: Vec<_> = log.lines().collect();

            panic!(
                "building {} failed: {cmd:?} exited with {status}. The end of {}:\n\n{}",
                self.name,
                log_path.display(),
                lines[lines.len().saturating_sub(40)..].join("\n"),
            );
        }
    }

    fn meson(&self, options: &[&str]) {
        self.exec(
            self.cmd("meson")
                .args(["setup", "build", "--libdir=lib", "--buildtype=release"])
                .arg(format!("--prefix={}", self.prefix.display()))
                .args(options),
        );

        self.exec(self.cmd("ninja").args(["-C", "build", &format!("-j{}", jobs()), "install"]));
    }

    fn configure(&self, args: &[&str]) {
        self.exec(self.cmd("./configure").arg(format!("--prefix={}", self.prefix.display())).args(args));
        self.make(&[]);
        self.make(&["install"]);
    }

    fn make(&self, args: &[&str]) {
        let mut cmd = self.cmd("make");

        match env::var_os("CARGO_MAKEFLAGS") {
            Some(flags) => cmd.env("MAKEFLAGS", flags),
            None => cmd.arg(format!("-j{}", jobs())),
        };

        self.exec(cmd.args(args));
    }

    fn log(&self) -> PathBuf {
        self.work.join("build.log")
    }
}

fn jobs() -> String {
    env::var("NUM_JOBS").unwrap_or_else(|_| "1".into())
}

fn prepend(dirs: &[PathBuf], var: &str) -> OsString {
    let mut all = dirs.to_vec();
    all.extend(env::split_paths(&env::var_os(var).unwrap_or_default()));
    env::join_paths(all).unwrap()
}
