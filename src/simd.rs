// SPDX-License-Identifier: LGPL-3.0-or-later
//! Kernels are written once for any vector width; on x86_64 the wider AVX2 build is picked at
//! run time, as `std::simd` alone only gets SSE2 there.

/// Defines `$name`, running `$kernel::<$n>` or, where AVX2 is there, `$kernel::<$wide>`.
macro_rules! dispatch {
    ($vis:vis fn $name:ident($($arg:ident: $ty:ty),* $(,)?) $(-> $ret:ty)? = $kernel:ident::<$n:literal, $wide:literal>) => {
        $vis fn $name($($arg: $ty),*) $(-> $ret)? {
            #[cfg(target_arch = "x86_64")]
            {
                #[target_feature(enable = "avx2")]
                unsafe fn wide($($arg: $ty),*) $(-> $ret)? {
                    $kernel::<$wide>($($arg),*)
                }

                if std::arch::is_x86_feature_detected!("avx2") {
                    // SAFETY: the CPU has AVX2.
                    return unsafe { wide($($arg),*) };
                }
            }

            $kernel::<$n>($($arg),*)
        }
    };
}

pub(crate) use dispatch;
