// SPDX-License-Identifier: LGPL-3.0-or-later
//! Number and whitespace parsing that behaves like the C library calls VSFilter makes.

pub(crate) fn skip_spaces(s: &str) -> &str {
    s.trim_start_matches([' ', '\t'])
}

pub(crate) fn trim_end_spaces(s: &str) -> &str {
    s.trim_end_matches([' ', '\t'])
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | 0x0B | 0x0C | b'\r')
}

/// A decimal float and how many bytes of `s` it took, `(0.0, 0)` when there's none.
pub(crate) fn strtod(s: &str) -> (f64, usize) {
    let b = s.as_bytes();
    let mut i = 0;

    while i < b.len() && is_space(b[i]) {
        i += 1;
    }

    let start = i;

    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }

    let digits = |i: &mut usize| {
        let from = *i;

        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }

        *i - from
    };

    let mut mantissa = digits(&mut i);

    if i < b.len() && b[i] == b'.' {
        i += 1;
        mantissa += digits(&mut i);
    }

    if mantissa == 0 {
        return (0.0, 0);
    }

    let end = i;

    if i < b.len() && (b[i] == b'e' || b[i] == b'E') {
        let mut j = i + 1;

        if j < b.len() && (b[j] == b'-' || b[j] == b'+') {
            j += 1;
        }

        if digits(&mut j) > 0 {
            i = j;
        }
    }

    let value = s[start..i].parse::<f64>().or_else(|_| s[start..end].parse::<f64>()).unwrap_or(0.0);
    (value, i)
}

pub(crate) fn atof(s: &str) -> f64 {
    strtod(s).0
}

/// An integer as `strtoll` reads it in `base` (10 or 16), clamped to `i32`, and its length.
pub(crate) fn strtoi32(s: &str, base: u32) -> (i32, usize) {
    let b = s.as_bytes();
    let mut i = 0;

    while i < b.len() && is_space(b[i]) {
        i += 1;
    }

    let negative = i < b.len() && b[i] == b'-';

    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }

    if base == 16
        && b.len() >= i + 3
        && b[i] == b'0'
        && (b[i + 1] | 0x20) == b'x'
        && (b[i + 2] as char).is_ascii_hexdigit()
    {
        i += 2;
    }

    let from = i;
    let mut value: i64 = 0;

    while i < b.len() {
        let Some(d) = (b[i] as char).to_digit(base) else { break };
        value = (value * base as i64 + d as i64).min(i64::MAX / 32);
        i += 1;
    }

    if i == from {
        return (0, 0);
    }

    let value = if negative { -value } else { value };
    (value.clamp(i32::MIN as i64, i32::MAX as i64) as i32, i)
}

pub(crate) fn strtol(s: &str) -> i64 {
    strtoi32(s, 10).0 as i64
}

/// An unsigned integer reduced modulo 2^32 the way VSFilter's `scanf` does, and its length.
pub(crate) fn strtou32_modulo(s: &str, base: u32) -> (u32, usize) {
    let s = skip_spaces(s);
    let b = s.as_bytes();
    let mut i = 0;
    let mut negative = false;

    if i < b.len() && (b[i] == b'+' || b[i] == b'-') {
        negative = b[i] == b'-';
        i += 1;
    }

    if base == 16 && b.len() >= i + 2 && b[i] == b'0' && (b[i + 1] | 0x20) == b'x' {
        i += 2;
    }

    let from = i;
    let mut value = 0u32;

    while i < b.len() {
        let Some(d) = (b[i] as char).to_digit(base) else { break };
        value = value.wrapping_mul(base).wrapping_add(d);
        i += 1;
    }

    if i == from {
        return (0, 0);
    }

    (if negative { value.wrapping_neg() } else { value }, i)
}

/// `%d` of `scanf`: spaces, a sign and digits.
pub(crate) fn scan_int(s: &str) -> (i64, usize) {
    let b = s.as_bytes();
    let mut i = 0;

    while i < b.len() && is_space(b[i]) {
        i += 1;
    }

    let negative = i < b.len() && b[i] == b'-';

    if i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        i += 1;
    }

    let from = i;
    let mut value: i64 = 0;

    while i < b.len() && b[i].is_ascii_digit() {
        value = value.wrapping_mul(10).wrapping_add((b[i] - b'0') as i64);
        i += 1;
    }

    if i == from {
        return (0, 0);
    }

    (if negative { (-value) as i32 as i64 } else { value as i32 as i64 }, i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats() {
        assert_eq!(strtod(" -1.5e2x"), (-150.0, 7));
        assert_eq!(strtod(".5"), (0.5, 2));
        assert_eq!(strtod("1e"), (1.0, 1));
        assert_eq!(strtod("abc"), (0.0, 0));
        assert_eq!(strtod("-"), (0.0, 0));
    }

    #[test]
    fn integers() {
        assert_eq!(strtoi32("  42px", 10), (42, 4));
        assert_eq!(strtoi32("FF", 16), (255, 2));
        assert_eq!(strtoi32("99999999999", 10).0, i32::MAX);
        assert_eq!(strtou32_modulo("FFFFFFFF", 16), (u32::MAX, 8));
        assert_eq!(strtou32_modulo("4294967297", 10).0, 1);
        assert_eq!(strtou32_modulo("-1", 10).0, u32::MAX);
    }
}
