//! Domain types, errors and configuration shared across UP-TAKE.
//!
//! This crate sits at the bottom of the dependency graph: everything may depend
//! on it, and it depends on no sibling crate. Keep it free of Windows APIs, of
//! Tauri, and of I/O — it should stay testable without a window.
//!
//! # Coordinate spaces
//!
//! The single most important convention in the project, because it is where the
//! multi-monitor bugs come from. Four coordinate spaces exist and are easy to
//! confuse:
//!
//! 1. CSS / logical pixels inside the WebView
//! 2. Tauri logical pixels
//! 3. Physical device pixels
//! 4. Virtual-desktop coordinates — which **can be negative**, since a monitor
//!    positioned to the left of the primary one starts at `x < 0`
//!
//! **The rule:** all Rust-side geometry is *physical pixels in virtual-desktop
//! space*. Conversion happens exactly once, at the IPC boundary, in one
//! function. Every multi-monitor bug in this project will trace back to a
//! violation of this rule.
//!
//! The geometry types live in [`geometry`], together with the one sanctioned
//! conversion function. They carry property tests — coordinate math is the
//! number-one bug source and pure functions are cheap to test exhaustively.
//!
//! # The area model
//!
//! [`area`] holds the product's central noun — a rectangle of screen the user
//! has claimed, its three orthogonal properties, and the z-ordered store that
//! owns area identity and stacking. [`interaction`] holds the geometry of
//! *handling* one: which part of an area a pointer grabs and how dragging it
//! changes the bounds. Both are built on [`geometry`] and, like it, are pure: no
//! window, no capture, no OS.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod area;
pub mod geometry;
pub mod interaction;
