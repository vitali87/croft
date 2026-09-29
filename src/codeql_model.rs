//! The CodeQL Model Editor's endpoints (#578): the methods and classes of
//! a library that a model can make sources, sinks or summaries.
//!
//! VS Code's model editor gets them from the language's query pack: a
//! `@kind table` query tagged `modeleditor` and `endpoints`. For Python
//! that is `utils/modeleditor/FrameworkModeEndpoints.ql`, the library's own
//! public API ("framework mode"). Each row names the endpoint, its
//! namespace (the top package), its class (the module path, with the class
//! when it has one), its function and parameters, whether CodeQL already
//! models it, its file and what kind of endpoint it is.

use crate::codeql_query::CellLoc;
use std::collections::HashSet;

/// A query suite selecting `lang`'s model-editor endpoints query.
pub fn endpoints_suite(lang: &str) -> String {
    format!(
        "- from: codeql/{lang}-queries\n  queries: .\n- include:\n    kind: table\n    tags contain all:\n      - modeleditor\n      - endpoints\n"
    )
}

/// One endpoint a model can describe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// The top package: the `type` column a model names.
    pub namespace: String,
    /// The module path, with the class when there is one (`core.Runner`).
    pub class: String,
    /// The function's name; empty for a class itself.
    pub function: String,
    /// The parameter list as the query prints it: `(self,what)`.
    pub params: String,
    /// Whether CodeQL already models it.
    pub supported: bool,
    /// `Function`, `InstanceMethod`, `Class`, …
    pub kind: String,
    pub location: Option<CellLoc>,
}

impl Endpoint {
    /// The group it is listed under: `mylib.core` or `mylib.core.Runner`.
    pub fn group(&self) -> String {
        if self.class.is_empty() {
            self.namespace.clone()
        } else {
            format!("{}.{}", self.namespace, self.class)
        }
    }

    /// Its line in the list: `run(cmd,shell)`, or `class Runner`.
    pub fn label(&self) -> String {
        if self.function.is_empty() {
            let name = self.class.rsplit('.').next().unwrap_or(&self.class);
            format!("class {name}")
        } else {
            format!("{}{}", self.function, self.params)
        }
    }
}

/// Read the endpoints query's `#select`, decoded as JSON with entity
/// locations (`codeql_query::decode_locations_args`).
pub fn parse_endpoints(json: &str) -> Vec<Endpoint> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let text = |c: Option<&serde_json::Value>| -> String {
        match c {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Object(o)) => o
                .get("label")
                .and_then(|l| l.as_str())
                .unwrap_or_default()
                .to_string(),
            _ => String::new(),
        }
    };
    let rows = v
        .get("tuples")
        .and_then(|t| t.as_array())
        .cloned()
        .unwrap_or_default();
    let located = crate::codeql_query::parse_row_locations(json);
    rows.iter()
        .enumerate()
        .filter_map(|(i, r)| {
            let r = r.as_array()?;
            Some(Endpoint {
                namespace: text(r.get(1)),
                class: text(r.get(2)),
                function: text(r.get(3)),
                params: text(r.get(4)),
                supported: r.get(5).and_then(|b| b.as_bool()).unwrap_or(false),
                kind: text(r.get(8)),
                location: located
                    .get(i)
                    .and_then(|l| l.locs.first().cloned().flatten()),
            })
        })
        .collect()
}

/// Which extensible predicate a model row goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Which {
    Source,
    Sink,
    Summary,
}

impl Which {
    pub fn extensible(self) -> &'static str {
        match self {
            Which::Source => "sourceModel",
            Which::Sink => "sinkModel",
            Which::Summary => "summaryModel",
        }
    }

    /// The kind a new model of this sort starts from.
    pub fn default_kind(self) -> &'static str {
        match self {
            Which::Source => "remote",
            Which::Sink => "command-injection",
            Which::Summary => "taint",
        }
    }
}

/// One model the user made: a row of `sourceModel` / `sinkModel` (`type,
/// path, kind`) or `summaryModel` (`type, path, input, output, kind`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Model {
    pub which: Which,
    /// The `type` column: the endpoint's top package.
    pub ty: String,
    /// The access path to the endpoint (`Member[core].Member[run]`); a
    /// source or sink's own step (`ReturnValue`, `Argument[0,cmd:]`) is
    /// in `input` / `output`.
    pub path: String,
    pub input: String,
    pub output: String,
    pub kind: String,
}

impl Model {
    /// Its row in the model file, as the language's extensible predicate
    /// reads it.
    pub fn row(&self) -> Vec<String> {
        let at = |step: &str| format!("{}.{step}", self.path);
        match self.which {
            Which::Source => vec![self.ty.clone(), at(&self.output), self.kind.clone()],
            Which::Sink => vec![self.ty.clone(), at(&self.input), self.kind.clone()],
            Which::Summary => vec![
                self.ty.clone(),
                self.path.clone(),
                self.input.clone(),
                self.output.clone(),
                self.kind.clone(),
            ],
        }
    }

    /// A line saying what it models.
    pub fn describe(&self) -> String {
        match self.which {
            Which::Source => format!("source {} ({})", self.output, self.kind),
            Which::Sink => format!("sink {} ({})", self.input, self.kind),
            Which::Summary => format!(
                "summary {} \u{2192} {} ({})",
                self.input, self.output, self.kind
            ),
        }
    }
}

impl Endpoint {
    /// The access path to the endpoint from its package, in the form the
    /// Python models use: each module and class a `Member[..]`, an
    /// instance method reached through `Instance`. `None` for a class
    /// itself, which is not modeled here.
    pub fn access_path(&self) -> Option<String> {
        if self.function.is_empty() {
            return None;
        }
        let mut steps: Vec<String> = self
            .class
            .split('.')
            .filter(|p| !p.is_empty())
            .map(|p| format!("Member[{p}]"))
            .collect();
        if self.kind == "InstanceMethod" {
            steps.push(String::from("Instance"));
        }
        steps.push(format!("Member[{}]", self.function));
        Some(steps.join("."))
    }

    /// Its arguments as model steps: `Argument[0,cmd:]` reaches the first
    /// argument by position or by keyword. A method's `self` or `cls` is
    /// not an argument of the call.
    pub fn arguments(&self) -> Vec<String> {
        let inner = self
            .params
            .trim()
            .trim_start_matches('(')
            .trim_end_matches(')');
        let method = matches!(self.kind.as_str(), "InstanceMethod" | "ClassMethod");
        inner
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .skip(usize::from(method))
            .map(|p| p.trim_start_matches('*').to_string())
            .enumerate()
            .map(|(i, name)| format!("Argument[{i},{name}:]"))
            .collect()
    }

    /// The models this endpoint can take, as the Model Editor offers them:
    /// a source at its return value, a sink at each argument, a summary
    /// from each argument to the return value.
    pub fn choices(&self) -> Vec<Model> {
        let Some(path) = self.access_path() else {
            return Vec::new();
        };
        let model = |which: Which, input: &str, output: &str| Model {
            which,
            ty: self.namespace.clone(),
            path: path.clone(),
            input: input.to_string(),
            output: output.to_string(),
            kind: which.default_kind().to_string(),
        };
        let mut out = vec![model(Which::Source, "", "ReturnValue")];
        for a in self.arguments() {
            out.push(model(Which::Sink, &a, ""));
        }
        for a in self.arguments() {
            out.push(model(Which::Summary, &a, "ReturnValue"));
        }
        out
    }
}

/// The model pack croft keeps the models of database `db` (language
/// `lang`) in: its folder under `.github/codeql/extensions`, where VS Code
/// keeps model packs too, and its pack name.
pub fn model_pack(root: &std::path::Path, db: &str, lang: &str) -> (std::path::PathBuf, String) {
    let slug: String = format!("{db}-{lang}")
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    (
        root.join(".github/codeql/extensions").join(&slug),
        format!("croft/{slug}"),
    )
}

/// The pack file of a model pack named `name` extending `lang`'s library.
pub fn pack_yml(name: &str, lang: &str) -> String {
    format!(
        "name: {name}\nversion: 0.0.0\nlibrary: true\nextensionTargets:\n  codeql/{lang}-all: \"*\"\ndataExtensions:\n  - models/**/*.yml\n"
    )
}

/// The data extension file holding `models` for `lang`'s library.
pub fn models_yml(lang: &str, models: &[Model]) -> String {
    let quote = |s: &str| format!("'{}'", s.replace('\'', "''"));
    let mut out = String::from("extensions:\n");
    for which in [Which::Source, Which::Sink, Which::Summary] {
        let rows: Vec<&Model> = models.iter().filter(|m| m.which == which).collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "  - addsTo:\n      pack: codeql/{lang}-all\n      extensible: {}\n    data:\n",
            which.extensible()
        ));
        for m in rows {
            let cells: Vec<String> = m.row().iter().map(|c| quote(c)).collect();
            out.push_str(&format!("      - [{}]\n", cells.join(", ")));
        }
    }
    if models.is_empty() {
        out.push_str("  []\n");
    }
    out
}

/// The model packs in `root`'s `.github/codeql/extensions`, by name: what
/// query runs pass as `--model-packs`.
pub fn workspace_model_packs(root: &std::path::Path) -> Vec<String> {
    let dir = root.join(".github/codeql/extensions");
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let text = std::fs::read_to_string(e.path().join("codeql-pack.yml")).ok()?;
            if !text.contains("extensionTargets") {
                return None;
            }
            text.lines()
                .find_map(|l| l.strip_prefix("name:"))
                .map(|n| n.trim().trim_matches('"').to_string())
        })
        .collect();
    names.sort();
    names
}

/// The arguments a query run needs to use `root`'s model packs, empty when
/// it has none.
pub fn model_pack_args(root: &std::path::Path) -> Vec<String> {
    let packs = workspace_model_packs(root);
    if packs.is_empty() {
        return Vec::new();
    }
    let mut args = vec![format!(
        "--additional-packs={}",
        root.join(".github/codeql/extensions").display()
    )];
    args.extend(packs.iter().map(|p| format!("--model-packs={p}")));
    args
}

/// The Model Editor the side bar's Method Modeling section shows: the
/// database and language it is for, the endpoints by group, and which
/// groups are folded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelView {
    pub database: String,
    pub language: String,
    pub endpoints: Vec<Endpoint>,
    pub folded: HashSet<String>,
    /// The models the user made, kept in the model pack.
    pub models: Vec<Model>,
}

/// One row of the section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelRow {
    /// A group header and how many of its endpoints CodeQL models.
    Group(String, usize, usize),
    /// Endpoint `.0`, and its line.
    Endpoint(usize, String),
}

impl ModelView {
    /// The groups in order of first appearance, each with its endpoints'
    /// indices, sorted by name within the group.
    fn groups(&self) -> Vec<(String, Vec<usize>)> {
        let mut out: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, e) in self.endpoints.iter().enumerate() {
            let g = e.group();
            match out.iter_mut().find(|(name, _)| *name == g) {
                Some((_, members)) => members.push(i),
                None => out.push((g, vec![i])),
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        for (_, members) in &mut out {
            members.sort_by_key(|&i| self.endpoints[i].label());
        }
        out
    }

    /// The rows to show: each group, and its endpoints unless folded.
    /// Endpoints CodeQL models are marked ✓, ones the user modeled ●, the
    /// rest ○.
    pub fn rows(&self) -> Vec<ModelRow> {
        let mut out = Vec::new();
        for (group, members) in self.groups() {
            let modeled = members
                .iter()
                .filter(|&&i| self.endpoints[i].supported)
                .count();
            let folded = self.folded.contains(&group);
            out.push(ModelRow::Group(group, modeled, members.len()));
            if folded {
                continue;
            }
            for i in members {
                let e = &self.endpoints[i];
                let yours = e.access_path().is_some_and(|p| {
                    self.models
                        .iter()
                        .any(|m| m.ty == e.namespace && m.path == p)
                });
                let mark = if e.supported {
                    '\u{2713}'
                } else if yours {
                    '\u{25cf}'
                } else {
                    '\u{25cb}'
                };
                out.push(ModelRow::Endpoint(i, format!("  {mark} {}", e.label())));
            }
        }
        out
    }

    /// Fold or unfold `group`.
    pub fn toggle(&mut self, group: &str) {
        if !self.folded.remove(group) {
            self.folded.insert(group.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn an_endpoints_models_are_written_the_way_the_cli_reads_them() {
        let eps = parse_endpoints(ROWS);
        let run = &eps[0];
        assert_eq!(
            run.access_path().as_deref(),
            Some("Member[core].Member[run]")
        );
        assert_eq!(run.arguments(), ["Argument[0,cmd:]", "Argument[1,shell:]"]);
        let go = &eps[2];
        assert_eq!(
            go.access_path().as_deref(),
            Some("Member[core].Member[Runner].Instance.Member[go]")
        );
        assert_eq!(
            go.arguments(),
            ["Argument[0,what:]"],
            "self is not an argument"
        );
        assert_eq!(eps[1].access_path(), None, "a class itself");
        let choices = run.choices();
        assert_eq!(choices.len(), 5, "a source, two sinks, two summaries");
        let sink = choices.iter().find(|m| m.which == Which::Sink).unwrap();
        // The rows the real CLI applied in #578's check: with them,
        // py/command-line-injection found the flow it missed without.
        assert_eq!(
            sink.row(),
            [
                "mylib",
                "Member[core].Member[run].Argument[0,cmd:]",
                "command-injection"
            ]
        );
        assert_eq!(
            choices[0].row(),
            ["mylib", "Member[core].Member[run].ReturnValue", "remote"]
        );
        let summary = choices.iter().find(|m| m.which == Which::Summary).unwrap();
        assert_eq!(
            summary.row(),
            [
                "mylib",
                "Member[core].Member[run]",
                "Argument[0,cmd:]",
                "ReturnValue",
                "taint"
            ]
        );
        let yml = models_yml("python", &[choices[0].clone(), sink.clone()]);
        assert_eq!(
            yml,
            "extensions:\n  - addsTo:\n      pack: codeql/python-all\n      extensible: sourceModel\n    data:\n      - ['mylib', 'Member[core].Member[run].ReturnValue', 'remote']\n  - addsTo:\n      pack: codeql/python-all\n      extensible: sinkModel\n    data:\n      - ['mylib', 'Member[core].Member[run].Argument[0,cmd:]', 'command-injection']\n"
        );
    }

    #[test]
    fn model_packs_are_named_found_and_passed_to_runs() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert!(model_pack_args(root).is_empty(), "no packs, no arguments");
        let (pack, name) = model_pack(root, "My DB", "python");
        assert_eq!(pack, root.join(".github/codeql/extensions/my-db-python"));
        assert_eq!(name, "croft/my-db-python");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("codeql-pack.yml"), pack_yml(&name, "python")).unwrap();
        assert_eq!(workspace_model_packs(root), ["croft/my-db-python"]);
        assert_eq!(
            model_pack_args(root),
            [
                format!(
                    "--additional-packs={}",
                    root.join(".github/codeql/extensions").display()
                ),
                String::from("--model-packs=croft/my-db-python")
            ]
        );
    }

    /// Real `FrameworkModeEndpoints.ql` rows (python-queries 1.8.11) for a
    /// package with `core.run`, `class core.Runner` and `Runner.go`.
    const ROWS: &str = r#"{"columns":[],"tuples":[
      [{"label":"Function run","url":{"uri":"file:///w/mylib/core.py","startLine":3,"startColumn":1,"endLine":3,"endColumn":25}},"mylib","core","run","(cmd,shell)",false,"core.py","",{"label":"Function"}],
      [{"label":"Class Runner","url":{"uri":"file:///w/mylib/core.py","startLine":6,"startColumn":1,"endLine":6,"endColumn":13}},"mylib","core.Runner","","",false,"core.py","",{"label":"Class"}],
      [{"label":"Function go","url":{"uri":"file:///w/mylib/core.py","startLine":7,"startColumn":5,"endLine":7,"endColumn":23}},"mylib","core.Runner","go","(self,what)",true,"core.py","",{"label":"InstanceMethod"}]]}"#;

    #[test]
    fn endpoints_are_read_from_the_query_rows() {
        let eps = parse_endpoints(ROWS);
        assert_eq!(eps.len(), 3);
        assert_eq!(
            (
                eps[0].namespace.as_str(),
                eps[0].class.as_str(),
                eps[0].function.as_str()
            ),
            ("mylib", "core", "run")
        );
        assert_eq!(eps[0].label(), "run(cmd,shell)");
        assert_eq!(eps[0].group(), "mylib.core");
        assert_eq!(eps[0].kind, "Function");
        assert_eq!(
            eps[0].location,
            Some(CellLoc {
                path: PathBuf::from("/w/mylib/core.py"),
                line: 3,
                column: 1
            })
        );
        assert_eq!(eps[1].label(), "class Runner");
        assert_eq!(eps[2].group(), "mylib.core.Runner");
        assert!(eps[2].supported);
        assert!(parse_endpoints("oops").is_empty());
    }

    #[test]
    fn the_section_groups_endpoints_and_folds_groups() {
        let mut v = ModelView {
            database: String::from("lib"),
            language: String::from("python"),
            endpoints: parse_endpoints(ROWS),
            ..ModelView::default()
        };
        assert_eq!(
            v.rows(),
            [
                ModelRow::Group(String::from("mylib.core"), 0, 1),
                ModelRow::Endpoint(0, String::from("  \u{25cb} run(cmd,shell)")),
                ModelRow::Group(String::from("mylib.core.Runner"), 1, 2),
                ModelRow::Endpoint(1, String::from("  \u{25cb} class Runner")),
                ModelRow::Endpoint(2, String::from("  \u{2713} go(self,what)")),
            ]
        );
        v.toggle("mylib.core.Runner");
        assert_eq!(v.rows().len(), 3, "a folded group hides its endpoints");
    }

    #[test]
    fn the_endpoints_query_is_found_by_its_tags() {
        assert_eq!(
            endpoints_suite("python"),
            "- from: codeql/python-queries\n  queries: .\n- include:\n    kind: table\n    tags contain all:\n      - modeleditor\n      - endpoints\n"
        );
    }
}
