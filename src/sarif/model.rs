//! SARIF 2.1.0 object model (OASIS standard, `sarif-schema-2.1.0.json`).
//!
//! Deserialisation is deliberately lenient: every property is optional and
//! unknown properties are ignored, because producers routinely omit required
//! fields or add their own. Validation that matters to the viewer (the
//! version, the shape of `runs`) happens in [`super::load`], not here.
//!
//! Property bags (`properties`) are kept as raw JSON so the details pane can
//! show whatever a tool put there (tags, `security-severity`, precision, …).

use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub type PropertyBag = Map<String, Value>;

/// `null` where an array or object is expected reads as empty. Producers emit
/// it (the spec itself gives `"runs": null` a meaning) and it must not make the
/// whole log unreadable.
fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SarifLog {
    pub version: Option<String>,
    #[serde(rename = "$schema")]
    pub schema: Option<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub runs: Vec<Run>,
    pub properties: PropertyBag,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Run {
    pub tool: Tool,
    pub results: Option<Vec<SarifResult>>,
    pub artifacts: Vec<Artifact>,
    pub original_uri_base_ids: BTreeMap<String, ArtifactLocation>,
    pub invocations: Vec<Invocation>,
    pub taxonomies: Vec<ToolComponent>,
    pub automation_details: Option<RunAutomationDetails>,
    pub baseline_guid: Option<String>,
    pub column_kind: Option<String>,
    pub default_encoding: Option<String>,
    pub default_source_language: Option<String>,
    pub version_control_provenance: Vec<VersionControlDetails>,
    pub redaction_tokens: Vec<String>,
    pub properties: PropertyBag,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Tool {
    pub driver: ToolComponent,
    pub extensions: Vec<ToolComponent>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolComponent {
    pub name: String,
    pub full_name: Option<String>,
    pub guid: Option<String>,
    pub organization: Option<String>,
    pub product: Option<String>,
    pub version: Option<String>,
    pub semantic_version: Option<String>,
    pub information_uri: Option<String>,
    pub short_description: Option<MultiformatMessageString>,
    pub full_description: Option<MultiformatMessageString>,
    pub rules: Vec<ReportingDescriptor>,
    pub notifications: Vec<ReportingDescriptor>,
    pub taxa: Vec<ReportingDescriptor>,
    pub global_message_strings: BTreeMap<String, MultiformatMessageString>,
    pub properties: PropertyBag,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReportingDescriptor {
    pub id: String,
    pub guid: Option<String>,
    pub name: Option<String>,
    pub deprecated_ids: Vec<String>,
    pub short_description: Option<MultiformatMessageString>,
    pub full_description: Option<MultiformatMessageString>,
    pub message_strings: BTreeMap<String, MultiformatMessageString>,
    pub help: Option<MultiformatMessageString>,
    pub help_uri: Option<String>,
    pub default_configuration: Option<ReportingConfiguration>,
    pub relationships: Vec<ReportingDescriptorRelationship>,
    pub properties: PropertyBag,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReportingConfiguration {
    pub enabled: Option<bool>,
    pub level: Option<String>,
    pub rank: Option<f64>,
    pub properties: PropertyBag,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReportingDescriptorRelationship {
    pub target: ReportingDescriptorReference,
    pub kinds: Vec<String>,
    pub description: Option<Message>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ReportingDescriptorReference {
    pub id: Option<String>,
    pub index: Option<i64>,
    pub guid: Option<String>,
    pub tool_component: Option<ToolComponentReference>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToolComponentReference {
    pub name: Option<String>,
    pub index: Option<i64>,
    pub guid: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MultiformatMessageString {
    pub text: String,
    pub markdown: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Message {
    pub text: Option<String>,
    pub markdown: Option<String>,
    pub id: Option<String>,
    pub arguments: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SarifResult {
    pub rule_id: Option<String>,
    pub rule_index: Option<i64>,
    pub rule: Option<ReportingDescriptorReference>,
    pub kind: Option<String>,
    pub level: Option<String>,
    pub message: Message,
    pub locations: Vec<Location>,
    pub related_locations: Vec<Location>,
    pub analysis_target: Option<ArtifactLocation>,
    pub code_flows: Vec<CodeFlow>,
    pub stacks: Vec<Stack>,
    pub fixes: Vec<Fix>,
    pub suppressions: Option<Vec<Suppression>>,
    pub baseline_state: Option<String>,
    pub rank: Option<f64>,
    pub guid: Option<String>,
    pub correlation_guid: Option<String>,
    pub occurrence_count: Option<i64>,
    pub fingerprints: BTreeMap<String, String>,
    pub partial_fingerprints: BTreeMap<String, String>,
    pub hosted_viewer_uri: Option<String>,
    pub work_item_uris: Vec<String>,
    pub taxa: Vec<ReportingDescriptorReference>,
    pub provenance: Option<Value>,
    pub properties: PropertyBag,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Location {
    pub id: Option<i64>,
    pub physical_location: Option<PhysicalLocation>,
    pub logical_locations: Vec<LogicalLocation>,
    pub message: Option<Message>,
    pub annotations: Vec<Region>,
    pub properties: PropertyBag,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PhysicalLocation {
    pub artifact_location: Option<ArtifactLocation>,
    pub region: Option<Region>,
    pub context_region: Option<Region>,
    pub address: Option<Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArtifactLocation {
    pub uri: Option<String>,
    pub uri_base_id: Option<String>,
    pub index: Option<i64>,
    pub description: Option<Message>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Region {
    pub start_line: Option<i64>,
    pub start_column: Option<i64>,
    pub end_line: Option<i64>,
    pub end_column: Option<i64>,
    pub char_offset: Option<i64>,
    pub char_length: Option<i64>,
    pub byte_offset: Option<i64>,
    pub byte_length: Option<i64>,
    pub snippet: Option<ArtifactContent>,
    pub message: Option<Message>,
    pub source_language: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LogicalLocation {
    pub name: Option<String>,
    pub fully_qualified_name: Option<String>,
    pub decorated_name: Option<String>,
    pub kind: Option<String>,
    pub index: Option<i64>,
    pub parent_index: Option<i64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArtifactContent {
    pub text: Option<String>,
    pub binary: Option<String>,
    pub rendered: Option<MultiformatMessageString>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Artifact {
    pub location: Option<ArtifactLocation>,
    pub parent_index: Option<i64>,
    pub contents: Option<ArtifactContent>,
    pub mime_type: Option<String>,
    pub encoding: Option<String>,
    pub source_language: Option<String>,
    pub length: Option<i64>,
    pub roles: Vec<String>,
    pub hashes: BTreeMap<String, String>,
    pub description: Option<Message>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct CodeFlow {
    pub message: Option<Message>,
    pub thread_flows: Vec<ThreadFlow>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThreadFlow {
    pub id: Option<String>,
    pub message: Option<Message>,
    pub locations: Vec<ThreadFlowLocation>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThreadFlowLocation {
    pub index: Option<i64>,
    pub location: Option<Location>,
    pub stack: Option<Stack>,
    pub kinds: Vec<String>,
    pub module: Option<String>,
    pub state: BTreeMap<String, MultiformatMessageString>,
    pub nesting_level: Option<i64>,
    pub execution_order: Option<i64>,
    pub importance: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Stack {
    pub message: Option<Message>,
    pub frames: Vec<StackFrame>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StackFrame {
    pub location: Option<Location>,
    pub module: Option<String>,
    pub thread_id: Option<i64>,
    pub parameters: Vec<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Fix {
    pub description: Option<Message>,
    pub artifact_changes: Vec<ArtifactChange>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ArtifactChange {
    pub artifact_location: ArtifactLocation,
    pub replacements: Vec<Replacement>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Replacement {
    pub deleted_region: Region,
    pub inserted_content: Option<ArtifactContent>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Suppression {
    pub kind: Option<String>,
    pub status: Option<String>,
    pub justification: Option<String>,
    pub guid: Option<String>,
    pub location: Option<Location>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Invocation {
    pub command_line: Option<String>,
    pub arguments: Vec<String>,
    pub execution_successful: Option<bool>,
    pub exit_code: Option<i64>,
    pub start_time_utc: Option<String>,
    pub end_time_utc: Option<String>,
    pub working_directory: Option<ArtifactLocation>,
    pub tool_execution_notifications: Vec<Notification>,
    pub tool_configuration_notifications: Vec<Notification>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Notification {
    pub descriptor: Option<ReportingDescriptorReference>,
    pub associated_rule: Option<ReportingDescriptorReference>,
    pub message: Message,
    pub level: Option<String>,
    pub locations: Vec<Location>,
    pub time_utc: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RunAutomationDetails {
    pub id: Option<String>,
    pub guid: Option<String>,
    pub correlation_guid: Option<String>,
    pub description: Option<Message>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct VersionControlDetails {
    pub repository_uri: Option<String>,
    pub revision_id: Option<String>,
    pub branch: Option<String>,
    pub mapped_to: Option<ArtifactLocation>,
}
