// SPDX-License-Identifier: LGPL-3.0-or-later

use std::env;

fn main() {
    let dirs = env::var_os("DEP_ASS_PKGCONFIG").unwrap();

    unsafe {
        env::set_var("PKG_CONFIG_PATH", dirs);
    }

    pkg_config::Config::new().statik(true).probe("libass").unwrap_or_else(|e| panic!("can't link libass: {e}"));
    // HarfBuzz is C++.
    println!("cargo::rustc-link-lib=stdc++");
}
