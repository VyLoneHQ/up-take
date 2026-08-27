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
//! # Why there is no ONNX Runtime here yet
//!
//! Roadmap 1.10 is three things: `ort` integration, model loading, and the
//! background inference thread. The first two rest on a question this crate is
//! deliberately not answering: **how ONNX Runtime is obtained.**
//!
//! Probed 2026-08-27 rather than recalled. `ort` is at **2.0.0-rc.13 with no
//! stable release at all** (`max_stable: None` from the crates.io API), it is
//! `MIT OR Apache-2.0` and so already on `deny.toml`'s allow-list, and its
//! **default features include `download-binaries`** -- a build-time fetch of
//! prebuilt ONNX Runtime from a third-party CDN. The alternative is
//! `load-dynamic`, which loads a shared library at run time from a path the
//! application chooses.
//!
//! That choice reaches CI reproducibility, what the installer carries, what
//! SignPath is asked to sign, and `architecture.md` section 4's supply-chain
//! posture -- a document whose own model-file row reads *"a poisoned ONNX model
//! is arbitrary code"*, and the runtime is a larger surface than the model. It
//! is an ADR, so it is not taken here by default. **A default taken silently is
//! the same decision made worse**, and this module comment is where a reader
//! finds out it was deferred on purpose.
//!
//! What that costs is nothing structural: [`Engine`] is the seam, and wiring a
//! real recogniser behind it changes no caller.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod engine;
pub mod service;

pub use engine::{Engine, EngineError, Recognition, TextBlock};
pub use service::{Outcome, RequestId, Service, ServiceError};
