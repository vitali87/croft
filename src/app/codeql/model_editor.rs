//! The model editor: reading a database's endpoints, modeling an endpoint, and writing
//! the models to the database's model pack.

use super::*;

impl App {
    /// CodeQL: Open Model Editor (#578): read the current database's
    /// library endpoints with its language's model-editor query, off the
    /// UI thread; [`Self::drain_codeql_model`] lists them under Method
    /// Modeling.
    pub(in crate::app) fn open_codeql_model_editor(&mut self) {
        use crate::codeql_query as cq;
        if self.codeql_model_job.is_some() {
            self.status = String::from("The model editor is already reading endpoints");
            return;
        }
        let store = crate::codeql_db::DatabaseStore::load(&Self::codeql_db_store_path());
        let Some(db) = store.current.and_then(|i| store.databases.get(i)).cloned() else {
            self.status = String::from("Select a CodeQL database first");
            return;
        };
        let Some(lang) = db.language.clone() else {
            self.status = format!("CodeQL database {} does not say its language", db.name);
            return;
        };
        let dir = Self::codeql_contextual_dir("model");
        let program = self.codeql_program.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let lang_for = lang.clone();
        std::thread::spawn(move || {
            let read = || -> Result<Vec<crate::codeql_model::Endpoint>, String> {
                let io = |e: std::io::Error| format!("{}: {e}", dir.display());
                std::fs::create_dir_all(&dir).map_err(io)?;
                let suite = dir.join("endpoints.qls");
                std::fs::write(&suite, crate::codeql_model::endpoints_suite(&lang_for))
                    .map_err(io)?;
                let listed =
                    Self::codeql_stdout(&program, &crate::codeql_ast::resolve_args(&suite))?;
                let query = crate::codeql_ast::first_query(&listed).ok_or_else(|| {
                    format!(
                        "no model-editor query for {lang_for}; download the \
                         codeql/{lang_for}-queries pack with CodeQL: Download Packs"
                    )
                })?;
                let bqrs = dir.join("endpoints.bqrs");
                Self::codeql_command(&program, &cq::run_args(&query, &db.path, &bqrs))?;
                let json = dir.join("endpoints.json");
                Self::codeql_command(
                    &program,
                    &cq::decode_locations_args(&bqrs, &json, "#select"),
                )?;
                let endpoints = crate::codeql_model::parse_endpoints(
                    &std::fs::read_to_string(&json).map_err(io)?,
                );
                if endpoints.is_empty() {
                    return Err(String::from(
                        "the database has no library endpoints to model (is it a library?)",
                    ));
                }
                Ok(endpoints)
            };
            let _ = tx.send(read());
        });
        self.status = format!("Reading the endpoints of {}\u{2026}", db.name);
        self.codeql_model_job = Some((rx, db.name, lang));
    }

    /// Model endpoint `endpoint` of the Model Editor (#578): pick what it
    /// is, as a source at its return value, a sink at an argument, or a
    /// summary, or remove the models made for it.
    pub(in crate::app) fn model_codeql_endpoint(&mut self, endpoint: Option<usize>) {
        use crate::widgets::list_picker::{ListPicker, ListPurpose, ListRow};
        let Some(view) = self.codeql.model.as_ref() else {
            self.status = String::from("Open the model editor first");
            return;
        };
        let Some(e) = endpoint.and_then(|i| view.endpoints.get(i)) else {
            self.status = String::from("Select an endpoint in Method Modeling first");
            return;
        };
        let choices = e.choices();
        if choices.is_empty() {
            self.status = format!("{} is a class; model its methods", e.label());
            return;
        }
        let i = endpoint.unwrap_or(0);
        let mut rows: Vec<ListRow> = choices
            .iter()
            .enumerate()
            .map(|(j, m)| ListRow {
                id: format!("{i}:{j}"),
                label: m.describe(),
            })
            .collect();
        let path = e.access_path();
        if view
            .models
            .iter()
            .any(|m| m.ty == e.namespace && Some(&m.path) == path.as_ref())
        {
            rows.push(ListRow {
                id: format!("{i}:remove"),
                label: String::from("remove the models made for it"),
            });
        }
        let title = format!("Model {}", e.label());
        self.open_list_picker(ListPicker::new(ListPurpose::CodeqlModel, title, rows), "");
    }

    /// A model picked for an endpoint (`<endpoint>:<choice>` or
    /// `<endpoint>:remove`): ask for its kind, or remove its models.
    pub(in crate::app) fn choose_codeql_model(&mut self, id: &str) {
        use crate::widgets::input_prompt::{InputPrompt, InputPurpose};
        let Some((endpoint, choice)) = id.split_once(':') else {
            return;
        };
        let Ok(endpoint) = endpoint.parse::<usize>() else {
            return;
        };
        if choice == "remove" {
            let Some(view) = self.codeql.model.as_mut() else {
                return;
            };
            let Some(e) = view.endpoints.get(endpoint).cloned() else {
                return;
            };
            let path = e.access_path();
            view.models
                .retain(|m| !(m.ty == e.namespace && Some(&m.path) == path.as_ref()));
            self.status = match self.write_codeql_models() {
                Ok(file) => format!(
                    "Removed the models of {} from {}",
                    e.label(),
                    file.display()
                ),
                Err(why) => format!("Could not save the models: {why}"),
            };
            return;
        }
        let Ok(choice) = choice.parse::<usize>() else {
            return;
        };
        let Some(model) = self
            .codeql
            .model
            .as_ref()
            .and_then(|v| v.endpoints.get(endpoint))
            .and_then(|e| e.choices().into_iter().nth(choice))
        else {
            return;
        };
        self.open_input_prompt(
            InputPrompt::new(
                InputPurpose::CodeqlModelKind { endpoint, choice },
                format!("Kind of {}", model.describe()),
                String::from("remote, command-injection, sql-injection, taint, value, \u{2026}"),
            )
            .with_value(model.which.default_kind()),
        );
    }

    /// Save choice `choice` of endpoint `endpoint` with kind `kind` in the
    /// database's model pack (#578).
    pub(in crate::app) fn save_codeql_model(&mut self, endpoint: usize, choice: usize, kind: &str) {
        let kind = kind.trim();
        if kind.is_empty() {
            self.status = String::from("A model needs a kind");
            return;
        }
        let Some(view) = self.codeql.model.as_mut() else {
            return;
        };
        let Some(mut model) = view
            .endpoints
            .get(endpoint)
            .and_then(|e| e.choices().into_iter().nth(choice))
        else {
            return;
        };
        model.kind = kind.to_string();
        let label = view.endpoints[endpoint].label();
        let described = model.describe();
        view.models.retain(|m| *m != model);
        view.models.push(model);
        self.status = match self.write_codeql_models() {
            Ok(file) => format!("Modeled {label} as a {described} in {}", file.display()),
            Err(why) => format!("Could not save the model: {why}"),
        };
    }

    /// Write the Model Editor's models to the database's model pack: its
    /// pack file, the data extension CodeQL reads, and croft's own list.
    /// Returns the data extension's path.
    pub(in crate::app) fn write_codeql_models(&mut self) -> Result<PathBuf, String> {
        use crate::codeql_model as cm;
        let view = self
            .codeql
            .model
            .as_ref()
            .ok_or("no model editor is open")?;
        let (dir, name) = cm::model_pack(self.workspace_root(), &view.database, &view.language);
        let io = |e: std::io::Error| format!("{}: {e}", dir.display());
        std::fs::create_dir_all(dir.join("models")).map_err(io)?;
        std::fs::write(
            dir.join("codeql-pack.yml"),
            cm::pack_yml(&name, &view.language),
        )
        .map_err(io)?;
        let slug = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let file = dir.join("models").join(format!("{slug}.model.yml"));
        std::fs::write(&file, cm::models_yml(&view.language, &view.models)).map_err(io)?;
        let json = serde_json::to_string_pretty(&view.models).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("croft-models.json"), json).map_err(io)?;
        Ok(file
            .strip_prefix(self.workspace_root())
            .unwrap_or(&file)
            .to_path_buf())
    }

    /// List a finished endpoints read under Method Modeling (#578), or say
    /// why there is none.
    pub fn drain_codeql_model(&mut self) -> bool {
        let Some((rx, ..)) = self.codeql_model_job.as_ref() else {
            return false;
        };
        let outcome = match rx.try_recv() {
            Ok(outcome) => outcome,
            Err(std::sync::mpsc::TryRecvError::Empty) => return false,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err(String::from("the endpoints read stopped unexpectedly"))
            }
        };
        let Some((_, database, language)) = self.codeql_model_job.take() else {
            return false;
        };
        match outcome {
            Ok(endpoints) => {
                let modeled = endpoints.iter().filter(|e| e.supported).count();
                self.status = format!(
                    "{} endpoints of {database}, {modeled} modeled by CodeQL",
                    endpoints.len()
                );
                // The models made before, kept beside the pack.
                let (dir, _) =
                    crate::codeql_model::model_pack(self.workspace_root(), &database, &language);
                let models = std::fs::read_to_string(dir.join("croft-models.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_default();
                self.codeql.model = Some(crate::codeql_model::ModelView {
                    database,
                    language,
                    endpoints,
                    models,
                    ..Default::default()
                });
                self.codeql
                    .collapsed
                    .remove(&crate::widgets::codeql::Section::MethodModeling);
            }
            Err(why) => self.status = format!("Could not read the endpoints: {why}"),
        }
        true
    }
}
