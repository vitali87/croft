// Step 1 lands the wire layer. App-level wiring arrives in step 2, so the
// re-exports below are intentionally not yet consumed by the binary.
#![allow(dead_code, unused_imports)]

pub mod client;
pub mod config;
pub mod install;
pub mod languages;
pub mod log_file;
pub mod manager;
pub mod manifest;
pub mod registry;
pub mod runtime;
pub mod semantic_cache;

pub use client::LspClient;
pub use config::{Language, ServerConfig};
pub use manager::{CompletionItem, CompletionResult, LspManager};
pub use registry::ServerRegistry;
pub use runtime::LspRuntime;
