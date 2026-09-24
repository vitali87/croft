//! SARIF (Static Analysis Results Interchange Format) viewer support.
//!
//! * [`model`] — the SARIF 2.1.0 object model, deserialised leniently.
//! * [`load`] — parse a log, reject anything that is not 2.1.0, and explain why.
//! * [`region`] — turn a SARIF region into a range in the local text.
//! * [`resolve`] — turn an artifact location into a file on this machine.
//! * [`render`] — paint the viewer tab.
//! * [`view`] — the viewer's list state: grouping, filters, selection.
//! * [`semantics`] — the spec rules a viewer has to apply on top of the raw
//!   objects: which rule a result points at, its effective level, whether it is
//!   suppressed, and how its message text is assembled.

// Parts of the model (fixes, stacks, the raw result) are read only by later
// viewer features; the tests exercise them meanwhile.
#![cfg_attr(not(test), allow(dead_code))]

pub mod details;
pub mod diagnostics;
pub mod fixes;
pub mod load;
pub mod model;
pub mod region;
pub mod render;
pub mod resolve;
pub mod semantics;
pub mod view;
