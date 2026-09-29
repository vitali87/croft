//! CodeQL databases (#578): recognising one on disk, remembering the ones the
//! user added, and fetching one from an archive, a URL or GitHub.
//!
//! A CodeQL database is a directory with a `codeql-database.yml` at its
//! root; its `primaryLanguage` names what it was extracted from. Archives,
//! including the ones GitHub's code-scanning API serves, hold that
//! directory one level down.

#![cfg_attr(not(test), allow(dead_code))]

use std::path::{Path, PathBuf};

pub const DB_MARKER: &str = "codeql-database.yml";

/// Whether `dir` is the root of a CodeQL database.
pub fn is_database_dir(dir: &Path) -> bool {
    dir.join(DB_MARKER).is_file()
}

/// The database's `primaryLanguage`, when its yml names one.
pub fn database_language(dir: &Path) -> Option<String> {
    let yml = std::fs::read_to_string(dir.join(DB_MARKER)).ok()?;
    yml.lines().find_map(|l| {
        let v = l.trim().strip_prefix("primaryLanguage:")?.trim();
        let v = v.trim_matches(|c| c == '"' || c == '\'');
        (!v.is_empty()).then(|| v.to_string())
    })
}

/// `dir` itself when it is a database, else its only child that is one
/// (how an extracted archive lays it out). `None` when there is none, or
/// more than one and the choice would be a guess.
pub fn find_database_in(dir: &Path) -> Option<PathBuf> {
    if is_database_dir(dir) {
        return Some(dir.to_path_buf());
    }
    let mut found = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && is_database_dir(p));
    let first = found.next()?;
    found.next().is_none().then_some(first)
}

/// The most a database's `src.zip` may expand to when croft extracts it to
/// browse (#578): generous for real source trees, but a crafted archive
/// cannot fill the disk.
pub const SOURCE_ZIP_LIMIT: u64 = 4 << 30;

/// Extract `zip` into `dest`, refusing any entry whose path would land
/// outside it (zip slip) and, with a `limit`, an archive whose contents
/// would exceed that many bytes. Returns `dest`.
pub fn extract_zip(zip: &Path, dest: &Path, limit: Option<u64>) -> Result<PathBuf, String> {
    let file = std::fs::File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("not a zip: {e}"))?;
    // Check every name before writing anything, so a hostile entry late in
    // the archive cannot leave the earlier ones half-extracted.
    let too_big = |limit: u64| format!("refusing it: it expands to more than {limit} bytes");
    let mut declared: u64 = 0;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.enclosed_name().is_none() {
            return Err(format!(
                "refusing {:?}: it would extract outside the folder",
                entry.name()
            ));
        }
        declared = declared.saturating_add(entry.size());
        if let Some(limit) = limit.filter(|&l| declared > l) {
            return Err(too_big(limit));
        }
    }
    // The sizes above are what the archive claims; the copy below counts
    // what it actually writes, so a lying header cannot get past either.
    let mut written: u64 = 0;
    std::fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).map_err(|e| e.to_string())?;
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(parent) = out.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut f = std::fs::File::create(&out).map_err(|e| e.to_string())?;
        match limit {
            None => {
                std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
            }
            Some(limit) => {
                let room = limit - written;
                let n = std::io::copy(&mut std::io::Read::take(&mut entry, room + 1), &mut f)
                    .map_err(|e| e.to_string())?;
                if n > room {
                    return Err(too_big(limit));
                }
                written += n;
            }
        }
    }
    Ok(dest.to_path_buf())
}

/// The `gh api` arguments that download a repository's CodeQL database for
/// `language` as a zip (GitHub's code-scanning databases endpoint).
pub fn github_database_args(owner_repo: &str, language: &str) -> Vec<String> {
    vec![
        String::from("api"),
        String::from("-H"),
        String::from("Accept: application/zip"),
        format!("/repos/{owner_repo}/code-scanning/codeql/databases/{language}"),
    ]
}

/// The [`crate::widgets::codeql::LANGUAGES`] label a database's
/// `primaryLanguage` belongs under, so the Language section can filter the
/// Databases one. CodeQL's combined extractors (`javascript` also covers
/// TypeScript, `java` Kotlin, `cpp` C) have one label each, as in VS Code.
pub fn language_label(id: &str) -> Option<&'static str> {
    let i = match id {
        "cpp" | "c" | "c-cpp" => 0,
        "csharp" => 1,
        "actions" => 2,
        "go" => 3,
        "java" | "kotlin" | "java-kotlin" => 4,
        "javascript" | "typescript" | "javascript-typescript" => 5,
        "python" => 6,
        "ruby" => 7,
        "rust" => 8,
        "swift" => 9,
        _ => return None,
    };
    Some(crate::widgets::codeql::LANGUAGES[i])
}

/// Where a database keeps the source it was extracted from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DbSource {
    /// A `src` folder, ready to browse.
    Folder(PathBuf),
    /// The `src.zip` the CLI writes by default, to extract first.
    Zip(PathBuf),
}

/// The database's source: its `src` folder when it has one, else its
/// `src.zip`, else `None`.
pub fn database_source(dir: &Path) -> Option<DbSource> {
    let folder = dir.join("src");
    if folder.is_dir() {
        return Some(DbSource::Folder(folder));
    }
    let zip = dir.join("src.zip");
    zip.is_file().then_some(DbSource::Zip(zip))
}

/// The folder name a database's extracted `src.zip` gets in croft's cache:
/// its folder name, a short hash of its canonical path (so two databases
/// with the same name never share one) and a short hash of the archive's
/// contents (so a database replaced at the same path is extracted afresh,
/// even when the new archive has the same size and timestamp). The first
/// two parts, from [`source_cache_prefix`], name every extraction of it.
pub fn source_cache_name(db: &Path, zip: &Path) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    if let Ok(mut f) = std::fs::File::open(zip) {
        let _ = std::io::copy(&mut f, &mut hasher);
    }
    let digest = format!("{:x}", hasher.finalize());
    format!("{}{}", source_cache_prefix(db), &digest[..12])
}

/// The part of [`source_cache_name`] shared by every extraction of the
/// database at `db`, whatever its archive: older ones are found and
/// removed by it.
pub fn source_cache_prefix(db: &Path) -> String {
    use sha2::Digest;
    let canon = db.canonicalize().unwrap_or_else(|_| db.to_path_buf());
    let digest = format!(
        "{:x}",
        sha2::Sha256::digest(canon.to_string_lossy().as_bytes())
    );
    let name = canon
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("database"));
    format!("{name}-{}-", &digest[..12])
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DbEntry {
    pub name: String,
    pub path: PathBuf,
    pub language: Option<String>,
    /// When it was added, in unix seconds. Lists saved before this field
    /// existed read as 0, so they sort as the oldest.
    #[serde(default)]
    pub added: u64,
    /// Names it had before being renamed. History saved before runs
    /// recorded their database's path names the database it ran on, so
    /// these keep such a run tied to it after a rename (#578).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub former_names: Vec<String>,
}

impl DbEntry {
    /// Whether it goes by `name`, now or before a rename.
    pub fn has_had_name(&self, name: &str) -> bool {
        self.name == name || self.former_names.iter().any(|n| n == name)
    }
}

/// The orders VS Code's Databases view sorts by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DbSort {
    #[default]
    Name,
    Language,
    Added,
}

impl DbSort {
    /// The next order in the cycle the side bar's sort row steps through.
    pub fn next(self) -> DbSort {
        match self {
            DbSort::Name => DbSort::Language,
            DbSort::Language => DbSort::Added,
            DbSort::Added => DbSort::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            DbSort::Name => "name",
            DbSort::Language => "language",
            DbSort::Added => "date added",
        }
    }
}

/// The databases the user added, and which one queries run against.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatabaseStore {
    #[serde(default)]
    pub databases: Vec<DbEntry>,
    #[serde(default)]
    pub current: Option<usize>,
    /// The order the user last chose; `None` keeps the order they were added.
    #[serde(default)]
    pub sort_by: Option<DbSort>,
}

impl DatabaseStore {
    pub fn load(path: &Path) -> DatabaseStore {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(path, text)
    }

    /// Add a database directory (validated), making it current. Adding one
    /// already listed selects it instead of listing it twice.
    pub fn add(&mut self, dir: &Path) -> Result<usize, String> {
        if !is_database_dir(dir) {
            return Err(format!(
                "{} has no {DB_MARKER}: not a CodeQL database",
                dir.display()
            ));
        }
        if let Some(i) = self.databases.iter().position(|d| d.path == dir) {
            self.current = Some(i);
            return Ok(i);
        }
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| dir.display().to_string());
        self.databases.push(DbEntry {
            name,
            path: dir.to_path_buf(),
            language: database_language(dir),
            added: now_secs(),
            former_names: Vec::new(),
        });
        self.current = Some(self.databases.len() - 1);
        // A new database takes its place in the chosen order.
        if let Some(by) = self.sort_by {
            self.sort(by);
        }
        Ok(self.current.unwrap_or(0))
    }

    /// Give database `index` a display name. A blank name is refused.
    pub fn rename(&mut self, index: usize, name: &str) -> Result<(), String> {
        let name = name.trim();
        if name.is_empty() {
            return Err(String::from("A database name cannot be empty"));
        }
        let db = self
            .databases
            .get_mut(index)
            .ok_or_else(|| String::from("No such database"))?;
        if db.name != name && !db.former_names.contains(&db.name) {
            let old = std::mem::take(&mut db.name);
            db.former_names.push(old);
        }
        db.name = name.to_string();
        Ok(())
    }

    /// Reorder the list and remember the order. `current` follows its
    /// entry, so sorting never changes which database queries run against.
    pub fn sort(&mut self, by: DbSort) {
        let current = self
            .current
            .and_then(|i| self.databases.get(i))
            .map(|d| d.path.clone());
        let name_key = |d: &DbEntry| d.name.to_lowercase();
        match by {
            DbSort::Name => self.databases.sort_by_key(name_key),
            DbSort::Language => self
                .databases
                .sort_by_key(|d| (d.language.is_none(), d.language.clone(), name_key(d))),
            DbSort::Added => self.databases.sort_by_key(|d| (d.added, name_key(d))),
        }
        self.sort_by = Some(by);
        self.current = current.and_then(|p| self.databases.iter().position(|d| d.path == p));
    }

    /// The index of the database at `path`, if it is listed.
    pub fn position(&self, path: &Path) -> Option<usize> {
        self.databases.iter().position(|d| d.path == path)
    }

    /// Forget database `index` (its files are the caller's business).
    /// `current` keeps pointing at the same entry, or clears when that
    /// entry is the one removed.
    pub fn remove(&mut self, index: usize) {
        if index >= self.databases.len() {
            return;
        }
        self.databases.remove(index);
        self.current = match self.current {
            Some(c) if c == index => None,
            Some(c) if c > index => Some(c - 1),
            other => other,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db(root: &Path, name: &str, lang: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join("db-python")).unwrap();
        std::fs::write(
            dir.join(DB_MARKER),
            format!("---\nsourceLocationPrefix: /src\nprimaryLanguage: \"{lang}\"\ncreationMetadata: {{}}\n"),
        )
        .unwrap();
        dir
    }

    #[test]
    fn a_database_is_recognised_with_its_language() {
        let tmp = tempfile::tempdir().unwrap();
        let db = make_db(tmp.path(), "flask-db", "python");
        assert!(is_database_dir(&db));
        assert!(!is_database_dir(tmp.path()));
        assert_eq!(database_language(&db).as_deref(), Some("python"));
        assert_eq!(database_language(tmp.path()), None);
    }

    #[test]
    fn an_extracted_archive_holds_the_database_one_level_down() {
        let tmp = tempfile::tempdir().unwrap();
        let db = make_db(tmp.path(), "inner", "javascript");
        assert_eq!(find_database_in(tmp.path()), Some(db.clone()));
        assert_eq!(find_database_in(&db), Some(db), "the dir itself");
        make_db(tmp.path(), "second", "go");
        assert_eq!(
            find_database_in(tmp.path()),
            None,
            "two candidates is a guess"
        );
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(find_database_in(empty.path()), None);
    }

    #[test]
    fn the_store_persists_dedupes_and_tracks_the_current_database() {
        let tmp = tempfile::tempdir().unwrap();
        let a = make_db(tmp.path(), "a-db", "python");
        let b = make_db(tmp.path(), "b-db", "go");
        let path = tmp.path().join("store.json");
        let mut s = DatabaseStore::load(&path);
        assert_eq!(s.add(&a), Ok(0));
        assert_eq!(s.add(&b), Ok(1));
        assert_eq!(s.current, Some(1), "the newest is current");
        assert_eq!(s.add(&a), Ok(0), "re-adding selects, never duplicates");
        assert_eq!(s.databases.len(), 2);
        assert_eq!(s.current, Some(0));
        assert_eq!(s.databases[1].language.as_deref(), Some("go"));
        assert_eq!(s.databases[0].name, "a-db");
        assert!(
            s.add(tmp.path()).unwrap_err().contains(DB_MARKER),
            "not a database"
        );
        s.save(&path).unwrap();
        let back = DatabaseStore::load(&path);
        assert_eq!(back, s);
        let mut back = back;
        back.remove(0);
        assert_eq!(back.databases.len(), 1);
        assert_eq!(back.current, None, "removing the current one clears it");
    }

    fn entry(name: &str, lang: Option<&str>, added: u64) -> DbEntry {
        DbEntry {
            name: name.into(),
            path: PathBuf::from("/dbs").join(name),
            language: lang.map(Into::into),
            added,
            former_names: Vec::new(),
        }
    }

    #[test]
    fn a_database_is_renamed_but_never_to_blank() {
        let mut s = DatabaseStore {
            databases: vec![entry("a", None, 0)],
            ..DatabaseStore::default()
        };
        assert_eq!(s.rename(0, "  flask main  "), Ok(()));
        assert_eq!(s.databases[0].name, "flask main");
        assert!(s.rename(0, "   ").is_err());
        assert_eq!(s.databases[0].name, "flask main", "unchanged");
        assert!(s.rename(3, "x").is_err());
        // It remembers what it was called, once each, for older history.
        s.rename(0, "flask").unwrap();
        s.rename(0, "flask").unwrap();
        assert_eq!(s.databases[0].former_names, ["a", "flask main"]);
        assert!(s.databases[0].has_had_name("a") && s.databases[0].has_had_name("flask"));
        assert!(!s.databases[0].has_had_name("b"));
    }

    #[test]
    fn sorting_keeps_the_current_database_current() {
        let mut s = DatabaseStore {
            databases: vec![
                entry("kafka", Some("java"), 30),
                entry("Flask", Some("python"), 10),
                entry("gin", Some("go"), 20),
            ],
            current: Some(0),
            sort_by: None,
        };
        let names = |s: &DatabaseStore| {
            s.databases
                .iter()
                .map(|d| d.name.clone())
                .collect::<Vec<_>>()
        };
        s.sort(DbSort::Name);
        assert_eq!(names(&s), ["Flask", "gin", "kafka"], "case-insensitive");
        assert_eq!(s.current, Some(2), "kafka is still current");
        s.sort(DbSort::Language);
        assert_eq!(names(&s), ["gin", "kafka", "Flask"]);
        assert_eq!(s.current, Some(1));
        s.sort(DbSort::Added);
        assert_eq!(names(&s), ["Flask", "gin", "kafka"]);
        assert_eq!(s.current, Some(2));
        assert_eq!(s.sort_by, Some(DbSort::Added), "the order is remembered");
        assert_eq!(DbSort::Added.next(), DbSort::Name);
    }

    #[test]
    fn a_new_database_takes_its_place_in_the_chosen_order() {
        let tmp = tempfile::tempdir().unwrap();
        let mut s = DatabaseStore::default();
        s.add(&make_db(tmp.path(), "zeta", "go")).unwrap();
        s.sort(DbSort::Name);
        let i = s.add(&make_db(tmp.path(), "alpha", "python")).unwrap();
        assert_eq!(i, 0, "sorted in by name");
        assert_eq!(s.current, Some(0));
        assert_eq!(s.databases[0].name, "alpha");
        assert!(s.databases[0].added > 0, "stamped when added");
    }

    #[test]
    fn removing_keeps_current_on_its_entry() {
        let mut s = DatabaseStore {
            databases: vec![
                entry("a", None, 0),
                entry("b", None, 0),
                entry("c", None, 0),
            ],
            current: Some(2),
            sort_by: None,
        };
        s.remove(0);
        assert_eq!(s.current, Some(1), "c moved up one");
        assert_eq!(s.databases[1].name, "c");
        s.remove(0);
        assert_eq!(s.current, Some(0));
        s.remove(5);
        assert_eq!(s.databases.len(), 1, "out of range is a no-op");
        s.remove(0);
        assert_eq!(s.current, None);
    }

    #[test]
    fn a_list_saved_before_dates_and_sorting_still_loads() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("store.json");
        std::fs::write(
            &path,
            r#"{"databases":[{"name":"a","path":"/dbs/a","language":"go"}],"current":0}"#,
        )
        .unwrap();
        let s = DatabaseStore::load(&path);
        assert_eq!(s.databases, vec![entry("a", Some("go"), 0)]);
        assert_eq!(s.current, Some(0));
        assert_eq!(s.sort_by, None);
    }

    #[test]
    fn database_languages_map_to_the_language_sections_labels() {
        assert_eq!(language_label("python"), Some("Python"));
        assert_eq!(
            language_label("typescript"),
            Some("JavaScript / TypeScript")
        );
        assert_eq!(language_label("kotlin"), Some("Java / Kotlin"));
        assert_eq!(language_label("c"), Some("C / C++"));
        assert_eq!(language_label("cobol"), None);
        for id in crate::widgets::codeql::LANGUAGE_IDS {
            assert!(language_label(id).is_some(), "{id}");
        }
    }

    #[test]
    fn zip_extraction_refuses_to_escape_its_folder() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let good = tmp.path().join("good.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&good).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("mydb/codeql-database.yml", opts).unwrap();
        z.write_all(b"primaryLanguage: \"python\"\n").unwrap();
        z.finish().unwrap();
        let dest = tmp.path().join("out");
        let got = extract_zip(&good, &dest, None).unwrap();
        assert_eq!(find_database_in(&got), Some(dest.join("mydb")));

        let evil = tmp.path().join("evil.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&evil).unwrap());
        z.start_file("../escaped.txt", opts).unwrap();
        z.write_all(b"x").unwrap();
        z.finish().unwrap();
        assert!(extract_zip(&evil, &tmp.path().join("out2"), None).is_err());
        assert!(
            !tmp.path().join("escaped.txt").exists(),
            "nothing written outside"
        );
    }

    /// #578: a limit caps what an archive may expand to, both by the sizes
    /// it declares and by what the copy actually writes.
    #[test]
    fn zip_extraction_stops_at_its_size_limit() {
        use std::io::Write as _;
        let tmp = tempfile::tempdir().unwrap();
        let zip = tmp.path().join("src.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        z.start_file("a.txt", opts).unwrap();
        z.write_all(&[b'a'; 600]).unwrap();
        z.start_file("b.txt", opts).unwrap();
        z.write_all(&[b'b'; 600]).unwrap();
        z.finish().unwrap();
        let err = extract_zip(&zip, &tmp.path().join("small"), Some(1000)).unwrap_err();
        assert!(err.contains("more than 1000 bytes"), "{err}");
        let got = extract_zip(&zip, &tmp.path().join("roomy"), Some(1200)).unwrap();
        assert_eq!(std::fs::read(got.join("b.txt")).unwrap().len(), 600);
    }

    #[test]
    fn a_databases_source_is_its_src_folder_else_its_src_zip() {
        let tmp = tempfile::tempdir().unwrap();
        let db = make_db(tmp.path(), "app", "go");
        assert_eq!(database_source(&db), None);
        std::fs::write(db.join("src.zip"), b"").unwrap();
        assert_eq!(
            database_source(&db),
            Some(DbSource::Zip(db.join("src.zip")))
        );
        std::fs::create_dir(db.join("src")).unwrap();
        assert_eq!(
            database_source(&db),
            Some(DbSource::Folder(db.join("src"))),
            "the folder wins"
        );
        let other = make_db(&tmp.path().join("elsewhere"), "app", "go");
        let zip = db.join("src.zip");
        let name = source_cache_name(&db, &zip);
        assert!(name.starts_with("app-") && name.len() == 29, "{name}");
        assert!(name.starts_with(&source_cache_prefix(&db)));
        let other_zip = other.join("src.zip");
        assert_ne!(
            name,
            source_cache_name(&other, &other_zip),
            "same name, other path"
        );
        assert_eq!(name, source_cache_name(&db, &zip), "stable");
        std::fs::write(&zip, b"archive one").unwrap();
        let one = source_cache_name(&db, &zip);
        let mtime = std::fs::metadata(&zip).unwrap().modified().unwrap();
        // Same size and timestamp, other contents: still a new folder.
        std::fs::write(&zip, b"archive two").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&zip)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let two = source_cache_name(&db, &zip);
        assert_ne!(one, two, "a changed archive gets a new folder");
        assert!(two.starts_with(&source_cache_prefix(&db)));
    }

    #[test]
    fn github_databases_come_from_the_code_scanning_api() {
        assert_eq!(
            github_database_args("apache/kafka", "java"),
            vec![
                "api",
                "-H",
                "Accept: application/zip",
                "/repos/apache/kafka/code-scanning/codeql/databases/java"
            ]
        );
    }
}
