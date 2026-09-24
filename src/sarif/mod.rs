//! SARIF (Static Analysis Results Interchange Format) viewer support.
//!
//! * [`model`] — the SARIF 2.1.0 object model, deserialised leniently.
//! * [`load`] — parse a log, reject anything that is not 2.1.0, and explain why.
//! * [`semantics`] — the spec rules a viewer has to apply on top of the raw
//!   objects: which rule a result points at, its effective level, whether it is
//!   suppressed, and how its message text is assembled.

// The viewer that consumes this lands in a follow-up; until then only the
// tests reach most of it.
#![cfg_attr(not(test), allow(dead_code))]

pub mod load;
pub mod model;
pub mod semantics;
