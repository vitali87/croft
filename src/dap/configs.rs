//! Debug configurations (#250): `.vscode/launch.json` and `.croft/launch.json`.
//!
//! VS Code has carried program arguments, environment variables, working
//! directories, build steps, and attach targets in `launch.json` since 1.0;
//! croft's zero-config "Debug active file" covers none of that. This module
//! reads both files (JSONC tolerated, like tasks.json), resolves the
//! `${...}` substitution variables, and builds the DAP `launch`/`attach`
//! request for whichever adapter family the config's `type` names. Fields
//! croft doesn't know are passed through to the adapter verbatim — that is
//! how VS Code behaves, and it keeps adapter-specific keys (`sourceMaps`,
//! `justMyCode`, …) working without croft modelling them.
//!
//! Everything here is pure (filesystem in, values out); the App owns the
//! side effects (spawning adapters, running the preLaunchTask in a pane).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::dap::session::AdapterKind;

mod add_configuration;
mod compounds;
mod debug_test;
mod discovery;
mod env_files;
mod go;
mod inputs;
mod js;
mod lldb;
mod presentation;
mod python;
mod resolve;
mod substitution;
pub use add_configuration::ConfigDraft;
pub use add_configuration::DRAFT_TYPES;
pub use add_configuration::DraftField;
pub use add_configuration::add_to_croft_launch_json;
#[cfg(test)]
use add_configuration::*;
pub use compounds::Compound;
pub use compounds::resolve_compound;
use compounds::*;
pub use debug_test::debug_test_config;
pub use debug_test::with_debug_test_config;
#[cfg(test)]
use debug_test::*;
pub use discovery::discover_compounds;
pub use discovery::discover_configs;
pub use discovery::discover_configs_all;
#[cfg(test)]
pub use discovery::parse_launch_json;
pub use discovery::workspace_folders;
use discovery::*;
use env_files::*;
pub use go::delve_remote_address;
pub use go::delve_request;
use go::*;
pub use inputs::InputKind;
pub use inputs::LaunchInput;
pub use inputs::inputs_to_ask;
pub use inputs::parse_inputs;
pub use inputs::text_input_refs;
pub use js::browser_type;
pub use js::js_config_error;
pub use js::js_request;
pub use js::with_headless_browser;
pub use js::with_workspace_folder;
use js::*;
pub use lldb::CargoSpec;
pub use lldb::build_cargo_program;
pub use lldb::cargo_spec;
pub use lldb::lldb_attach_has_target;
pub use lldb::lldb_request;
pub use lldb::parse_ps_processes;
#[cfg(test)]
use lldb::*;
pub use presentation::Presentation;
pub use presentation::visible_and_sorted;
use presentation::*;
pub use python::debugpy_connect_address;
pub use python::debugpy_request;
pub use resolve::ResolvedConfig;
pub use resolve::absolute_in;
pub use resolve::resolve;
use resolve::*;
pub use substitution::SubstCtx;
pub use substitution::substitute;
use substitution::*;

/// launch/attach — the two DAP session-starting requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestKind {
    Launch,
    Attach,
}

/// One configuration as read from a launch.json, unresolved: substitution
/// variables still in place, every field kept in `raw`. Resolution happens at
/// F5 time (in [`resolve`]) because `${file}` depends on the active editor.
// `Eq` is intentionally not derived: `presentation.order` is a `f64`, and
// floats break total equality. (`ResolvedConfig` below DOES derive `Eq` - it
// carries no float - so it is not the precedent this looks like.) None of
// these is ever a map key, so `PartialEq` is all the call sites and tests
// need.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugConfig {
    pub name: String,
    /// The `type` as written (`python`, `lldb`, `node`, …); mapped to an
    /// [`AdapterKind`] only at resolve time so an unsupported type shows a
    /// per-config error instead of being silently dropped from the picker.
    pub type_name: String,
    pub request: RequestKind,
    /// Which file declared it, for the picker's detail column.
    pub source: &'static str,
    /// The configuration's `presentation` (#318), when it declares a usable
    /// one. VS Code reads this on configurations as well as compounds and
    /// sorts BOTH kinds in one list, so a config that asks for a group must
    /// be able to sort against a compound that asks for the same one.
    pub presentation: Option<Presentation>,
    /// The file's `inputs` (#1424): what a `${input:id}` in this
    /// configuration asks for.
    pub inputs: Vec<LaunchInput>,
    /// The answers given for this launch, by input id; filled in once the
    /// user has answered every input the configuration uses.
    pub answers: BTreeMap<String, String>,
    /// The workspace folder this configuration belongs to when it is not
    /// the active one: a compound member named with `"folder"` (#1425)
    /// resolves `${workspaceFolder}` against its own folder.
    pub root: Option<PathBuf>,
    raw: Map<String, Value>,
}

impl DebugConfig {
    /// The `preLaunchTask` label, as written.
    pub fn pre_launch_task(&self) -> Option<&str> {
        self.raw.get("preLaunchTask").and_then(Value::as_str)
    }
}

#[cfg(test)]
mod tests;
