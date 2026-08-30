//! Checksummed acquisition of the files UP-TAKE does not ship in its binary.
//!
//! # What this crate is for
//!
//! Two decisions land on one piece of work. [`ADR-0032`] chose to load ONNX
//! Runtime at run time from a path UP-TAKE places, rather than let a build fetch
//! one from a CDN. [`ADR-0034`] chose to convert PaddleOCR's official PP-OCRv4
//! release to ONNX ourselves, rather than take a third party's conversion. Both
//! records require the identical discipline, in the same words -- *"a documented,
//! checksummed step: pinned SHA-256, verified before load, HTTPS only"* -- and
//! both deferred the same question to roadmap `1.12`: whether these files ship
//! inside the installer or are fetched on first run.
//!
//! So the runtime and the models are described by **one manifest and one
//! verification path**, not two mechanisms that could drift apart. That is the
//! whole design.
//!
//! # Why there is no HTTP client here
//!
//! A TLS-capable HTTP stack is a large dependency tree and a supply-chain
//! decision of exactly the class `ADR-0032` was. This crate therefore defines
//! **what must be true of a download** and leaves **how bytes arrive** behind
//! the [`fetch::Fetcher`] seam, the same way `uptake-ocr` put PP-OCRv4 behind
//! `Engine` before the ONNX question was settled.
//!
//! The practical consequence is that every rule in this crate is tested with no
//! network, no TLS, and none of the 25 MB of assets it exists to install:
//!
//! | Concern | Module | Needs the network? |
//! | --- | --- | --- |
//! | what to fetch, and what it must hash to | [`manifest`] | no |
//! | holding a stream to its length and digest | [`verify`] | no |
//! | never letting unverified bytes become a file | [`install`] | no |
//! | how bytes actually arrive | [`fetch`] | **the implementor's problem** |
//!
//! # The invariant
//!
//! **Unverified bytes never become a usable file.** Everything else here is in
//! service of that sentence: the digest is checked while the bytes stream, the
//! download lands in a temporary file, and the rename into place happens only
//! after [`verify::Verifier::finish`] has returned `Ok`. `architecture.md`
//! section 4 calls a poisoned model *"arbitrary code"*, and this crate is the
//! thing standing between that sentence and the disk.
//!
//! [`ADR-0032`]: https://github.com/VyLoneHQ/up-take
//! [`ADR-0034`]: https://github.com/VyLoneHQ/up-take

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod fetch;
pub mod install;
pub mod manifest;
pub mod verify;
