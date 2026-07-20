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
//! The geometry types themselves land with roadmap task 1.1. They belong here,
//! and they get property tests — coordinate math is the number-one bug source
//! and pure functions are cheap to test exhaustively.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
