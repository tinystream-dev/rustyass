// SPDX-License-Identifier: LGPL-3.0-or-later
//! An ASS/SSA subtitle renderer, drawing what libass and VSFilter draw.
//!
//! [`Fonts`] holds the fonts to render with, a [`Track`] is a script, and a [`Renderer`] turns a
//! track at a time into [`Image`]s: masks, each in one colour, to blend over the video.

#![feature(portable_simd)]

mod bitmap;
mod blur;
mod event;
mod font;
mod glyphs;
mod outline;
mod raster;
mod render;
mod shape;
mod simd;
mod state;
mod stroke;
mod text;
mod track;

pub use font::Fonts;
pub use render::{Image, ImageKind, Margins, Rendered, Renderer, Rgba};
pub use track::{Event, Style, Track};
