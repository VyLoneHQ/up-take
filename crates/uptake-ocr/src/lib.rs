//! The OCR subsystem: a background inference thread, and the seam an engine
//! plugs into.
//!
//! # What this crate is for
//!
//! `architecture.md` section 3.2 states the whole contract in three sentences:
//! *"Runs on a dedicated thread. Models load once at startup and stay resident.
//! The UI thread never blocks -- if OCR takes 3 seconds, the overlay must still
//! close instantly."*
//!
//! Every one of those is a property of the **thread**, not of the recogniser.
//! That is why this crate exists before any recogniser does, and why the
//! recogniser arrives behind [`Engine`] rather than as a hard dependency.
//!
//! # How ONNX Runtime is obtained, and where the recogniser lives
//!
//! **Answered by [ADR-0032](https://github.com/VyLoneHQ/up-take), accepted
//! 2026-08-27: `ort` with `default-features = false` and `load-dynamic`.** The
//! runtime is loaded at run time from a path UP-TAKE chooses, never fetched
//! during a build. The default feature set includes `download-binaries`, a
//! build-time fetch of a prebuilt runtime from a third-party CDN, and
//! `architecture.md` section 4 treats a model file as arbitrary code while the
//! runtime is the larger surface of the two.
//!
//! Re-probed 2026-08-30 rather than carried over from the decision: `ort` is
//! still at **2.0.0-rc.13 with no stable release** (`max_stable_version: null`),
//! so the version is pinned **exactly** in the workspace manifest, per that
//! decision's fourth clause. Nothing in either repository watches crates.io, so
//! that pin is a dated observation and not a standing truth.
//!
//! [`paddle`] is PP-OCRv4 behind [`Engine`] -- roadmap 1.11. **Five of its six
//! stages are pure**: the resize, the probability-map post-processing, the
//! geometry, the crop and CTC decode, and the reading-order sort all take
//! plain data and are tested in CI with no runtime and no model file present.
//! Only session management touches `ort`.
//!
//! What that costs is nothing structural: [`Engine`] is the seam, and wiring a
//! real recogniser behind it changes no caller.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod engine;
pub mod service;

pub use engine::{Engine, EngineError, Recognition, TextBlock};
// `StopReason` joined this list with roadmap 1.26, which gave the crate its
// first host: `Outcome::Stopped` carries one, so a caller that matches on an
// outcome cannot name the thing it is holding without it. Its absence was an
// omission in the public surface rather than a decision -- nothing outside this
// crate had matched on an outcome before.
pub use service::{Outcome, RequestId, Service, ServiceError, StopReason};

pub mod paddle;
