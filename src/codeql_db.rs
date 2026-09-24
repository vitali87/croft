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

/// Extract `zip` into `dest`, refusing any entry whose path would land
/// outside it (zip slip). Returns `dest`.
pub fn extract_zip(zip: &Path, dest: &Path) -> Result<PathBuf, String> {
    let file = std::fs::File::open(zip).map_err(|e| format!("{}: {e}", zip.display()))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("not a zip: {e}"))?;
    // Check every name before writing anything, so a hostile entry late in
    // the archive cannot leave the earlier ones half-extracted.
    for i in 0..archive.len() {
        let entry = archive.by_index(i).map_err(|e| e.to_string())?;
        if entry.enclosed_name().is_none() {
            return Err(format!(
                "refusing {:?}: it would extract outside the folder",
                entry.name()
            ));
        }
    }
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
        std::io::copy(&mut entry, &mut f).map_err(|e| e.to_string())?;
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DbEntry {
    pub name: String,
    pub path: PathBuf,
    pub language: Option<String>,
}

/// The databases the user added, and which one queries run against.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DatabaseStore {
    #[serde(default)]
    pub databases: Vec<DbEntry>,
    #[serde(default)]
    pub current: Option<usize>,
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
        });
        let i = self.databases.len() - 1;
        self.current = Some(i);
        Ok(i)
    }

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
        let got = extract_zip(&good, &dest).unwrap();
        assert_eq!(find_database_in(&got), Some(dest.join("mydb")));

        let evil = tmp.path().join("evil.zip");
        let mut z = zip::ZipWriter::new(std::fs::File::create(&evil).unwrap());
        z.start_file("../escaped.txt", opts).unwrap();
        z.write_all(b"x").unwrap();
        z.finish().unwrap();
        assert!(extract_zip(&evil, &tmp.path().join("out2")).is_err());
        assert!(
            !tmp.path().join("escaped.txt").exists(),
            "nothing written outside"
        );
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
