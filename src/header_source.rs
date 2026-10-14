//! C/C++: Switch Header/Source (#1360): the file that pairs with a C,
//! C++ or Objective-C source or header, found the way projects lay them
//! out: beside it, across an `include/` ↔ `src/` split, then anywhere in
//! the workspace with the same stem.

use std::path::{Component, Path, PathBuf};

/// Header extensions, most usual first.
pub const HEADER_EXTS: &[&str] = &["h", "hpp", "hh", "hxx", "inl"];
/// Source extensions, most usual first.
pub const SOURCE_EXTS: &[&str] = &["c", "cpp", "cc", "cxx", "m", "mm"];

/// The folder names a header tree and a source tree go by, paired.
const TREE_PAIRS: &[(&str, &str)] = &[("include", "src"), ("inc", "src"), ("include", "source")];

fn ext_of(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
}

/// Whether `path` is a C-family header or source this command works on.
pub fn is_c_family(path: &Path) -> bool {
    ext_of(path)
        .is_some_and(|e| HEADER_EXTS.contains(&e.as_str()) || SOURCE_EXTS.contains(&e.as_str()))
}

/// The extensions a counterpart of `path` may have, the likeliest first:
/// `.cpp` looks for `.hpp` before `.h`, `.c` for `.h`, and a header for
/// the source of its own flavour first.
fn counterpart_exts(path: &Path) -> Vec<&'static str> {
    let Some(ext) = ext_of(path) else {
        return Vec::new();
    };
    let prefer: &[&str] = match ext.as_str() {
        "c" => &["h"],
        "cpp" | "cc" | "cxx" => &["hpp", "hh", "hxx", "h"],
        "m" | "mm" => &["h"],
        "h" => &["c", "cpp", "cc", "m", "mm"],
        "hpp" => &["cpp"],
        "hh" => &["cc"],
        "hxx" => &["cxx"],
        _ => &[],
    };
    let rest = if HEADER_EXTS.contains(&ext.as_str()) {
        SOURCE_EXTS
    } else if SOURCE_EXTS.contains(&ext.as_str()) {
        HEADER_EXTS
    } else {
        return Vec::new();
    };
    let mut out: Vec<&'static str> = prefer.to_vec();
    out.extend(rest.iter().filter(|e| !prefer.contains(e)));
    out
}

/// `stem` with each of `exts` in `dir`, the first that exists.
fn first_in(
    dir: &Path,
    stem: &str,
    exts: &[&str],
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    exts.iter()
        .map(|e| dir.join(format!("{stem}.{e}")))
        .find(|p| exists(p))
}

/// The file that pairs with `path`, or `None` when there is none.
/// `workspace` bounds the `include/` ↔ `src/` walk, and `index` (the
/// workspace's files, as Quick Open lists them) is the last resort, where
/// the candidate sharing the most folders with `path` wins.
pub fn counterpart(
    path: &Path,
    workspace: &Path,
    index: &[PathBuf],
    exists: &dyn Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let exts = counterpart_exts(path);
    let stem = path.file_stem()?.to_str()?;
    let dir = path.parent()?;
    // 1. Beside it.
    if let Some(found) = first_in(dir, stem, &exts, exists) {
        return Some(found);
    }
    // 2. Across a header/source tree split: `src/geom/a.cpp` looks in
    // `include/geom/`, then `include/`, at every level up to the workspace.
    let rel = path.strip_prefix(workspace).ok()?;
    let parts: Vec<&str> = rel
        .parent()?
        .components()
        .filter_map(|c| match c {
            Component::Normal(s) => s.to_str(),
            _ => None,
        })
        .collect();
    for (i, part) in parts.iter().enumerate().rev() {
        let swaps = TREE_PAIRS.iter().filter_map(|(h, s)| {
            if part == h {
                Some(*s)
            } else if part == s {
                Some(*h)
            } else {
                None
            }
        });
        for other in swaps {
            let base = parts[..i]
                .iter()
                .fold(workspace.to_path_buf(), |p, c| p.join(c))
                .join(other);
            let below = parts[i + 1..].iter().fold(base.clone(), |p, c| p.join(c));
            if let Some(found) = first_in(&below, stem, &exts, exists)
                .or_else(|| first_in(&base, stem, &exts, exists))
            {
                return Some(found);
            }
        }
    }
    // 3. Anywhere with the same stem: the one sharing the most folders
    // with `path`, then one in a header (or source) tree, then the
    // likeliest extension, then the shallowest.
    let shared = |p: &Path| {
        p.components()
            .zip(path.components())
            .take_while(|(a, b)| a == b)
            .count()
    };
    let want_header = exts.first().is_some_and(|e| HEADER_EXTS.contains(e));
    let in_tree = |p: &Path| {
        p.strip_prefix(workspace).is_ok_and(|rel| {
            rel.components().any(|c| {
                let c = c.as_os_str();
                TREE_PAIRS
                    .iter()
                    .any(|(h, s)| c == if want_header { *h } else { *s })
            })
        })
    };
    index
        .iter()
        .filter(|p| p.file_stem().and_then(|s| s.to_str()) == Some(stem))
        .filter_map(|p| {
            let rank = exts.iter().position(|e| ext_of(p).as_deref() == Some(*e))?;
            Some((p, rank))
        })
        .min_by_key(|(p, rank)| {
            (
                std::cmp::Reverse(shared(p)),
                !in_tree(p),
                *rank,
                p.components().count(),
            )
        })
        .map(|(p, _)| p.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(files: &[&str]) -> (tempfile::TempDir, Vec<PathBuf>) {
        let tmp = tempfile::tempdir().unwrap();
        let mut all = Vec::new();
        for f in files {
            let p = tmp.path().join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, "").unwrap();
            all.push(p);
        }
        (tmp, all)
    }

    fn find(root: &Path, index: &[PathBuf], file: &str) -> Option<String> {
        counterpart(&root.join(file), root, index, &|p| p.is_file())
            .map(|p| p.strip_prefix(root).unwrap().to_string_lossy().into_owned())
    }

    #[test]
    fn a_source_and_its_header_side_by_side_switch_both_ways() {
        let (tmp, index) = tree(&["geom.cpp", "geom.hpp", "geom.h", "util.c", "util.h"]);
        let r = tmp.path();
        assert_eq!(find(r, &index, "geom.cpp").as_deref(), Some("geom.hpp"));
        assert_eq!(find(r, &index, "geom.hpp").as_deref(), Some("geom.cpp"));
        assert_eq!(find(r, &index, "util.c").as_deref(), Some("util.h"));
        assert_eq!(find(r, &index, "util.h").as_deref(), Some("util.c"));
    }

    #[test]
    fn src_and_include_trees_switch_both_ways() {
        let (tmp, index) = tree(&[
            "src/geom.cpp",
            "include/geom.hpp",
            "lib/src/shape/circle.cc",
            "lib/include/shape/circle.hh",
        ]);
        let r = tmp.path();
        assert_eq!(
            find(r, &index, "src/geom.cpp").as_deref(),
            Some("include/geom.hpp")
        );
        assert_eq!(
            find(r, &index, "include/geom.hpp").as_deref(),
            Some("src/geom.cpp")
        );
        assert_eq!(
            find(r, &index, "lib/src/shape/circle.cc").as_deref(),
            Some("lib/include/shape/circle.hh")
        );
        assert_eq!(
            find(r, &index, "lib/include/shape/circle.hh").as_deref(),
            Some("lib/src/shape/circle.cc")
        );
    }

    /// A header under `include/<project>/` is found through the index,
    /// the closest of several same-named candidates first.
    #[test]
    fn the_index_finds_a_counterpart_elsewhere_closest_first() {
        let (tmp, index) = tree(&[
            "src/geom.cpp",
            "include/mylib/geom.hpp",
            "third_party/other/geom.hpp",
            "tools/geom/geom.cpp",
        ]);
        let r = tmp.path();
        assert_eq!(
            find(r, &index, "src/geom.cpp").as_deref(),
            Some("include/mylib/geom.hpp")
        );
        assert_eq!(
            find(r, &index, "include/mylib/geom.hpp").as_deref(),
            Some("src/geom.cpp")
        );
    }

    /// Negative: no counterpart, a non-C file, and a same-stem file of the
    /// same kind (a header is not a header's counterpart).
    #[test]
    fn nothing_pairs_without_a_counterpart() {
        let (tmp, index) = tree(&[
            "main.cpp",
            "include/main.cpp",
            "notes.md",
            "notes.h",
            "a.h",
            "b/a.hpp",
        ]);
        let r = tmp.path();
        assert_eq!(find(r, &index, "main.cpp"), None);
        assert_eq!(find(r, &index, "notes.md"), None);
        assert_eq!(find(r, &index, "a.h"), None);
        assert!(!is_c_family(&r.join("notes.md")));
        assert!(is_c_family(&r.join("main.cpp")));
        assert!(is_c_family(&r.join("A.HPP")));
    }
}
