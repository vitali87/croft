//! Building the program F5 debugs from a C or C++ file (#1359).
//!
//! A file is rarely a program on its own: it needs its project's include
//! paths, flags and other translation units. So the build comes from the
//! project, the way clangd reads it:
//!
//! 1. A configured CMake build folder: `cmake --build <dir>`, then the
//!    executable target whose sources include the file, read from CMake's
//!    File API (`.cmake/api/v1/reply`).
//! 2. A `compile_commands.json` entry for the file: its compiler and flags
//!    (`-I`, `-D`, `-std`, …), for a one-file program in a configured tree.
//! 3. Otherwise the file alone with `cc` / `c++`.
//!
//! What croft builds itself goes to its cache, never next to the source.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A program built for debugging, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    pub binary: PathBuf,
    /// The build as the status line names it, e.g. `cmake --build build`.
    pub how: String,
}

/// A failed build: a one-line summary and the build's whole output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub summary: String,
    pub output: String,
}

impl Failed {
    fn new(summary: impl Into<String>, output: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            output: output.into(),
        }
    }
}

/// The configured CMake build folder of `workspace`: the one its
/// `compile_commands.json` points into, else `build/`, when it holds a
/// `CMakeCache.txt`.
pub fn cmake_build_dir(workspace: &Path) -> Option<PathBuf> {
    if !workspace.join("CMakeLists.txt").is_file() {
        return None;
    }
    let linked = std::fs::read_link(workspace.join("compile_commands.json"))
        .ok()
        .map(|t| {
            if t.is_absolute() {
                t
            } else {
                workspace.join(t)
            }
        })
        .and_then(|t| t.parent().map(Path::to_path_buf));
    linked
        .into_iter()
        .chain(std::iter::once(workspace.join("build")))
        .find(|d| d.join("CMakeCache.txt").is_file())
}

/// The newest File API reply index in `build`, if CMake wrote one.
fn reply_index(build: &Path) -> Option<PathBuf> {
    let dir = build.join(".cmake/api/v1/reply");
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("index-") && n.ends_with(".json"))
        })
        .max()
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// An executable target of a CMake build: its name, its program and the
/// sources it compiles (absolute).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExeTarget {
    pub name: String,
    pub program: PathBuf,
    pub sources: Vec<PathBuf>,
}

/// The executable targets CMake's File API reply in `build` describes.
pub fn executable_targets(build: &Path) -> Vec<ExeTarget> {
    let reply = build.join(".cmake/api/v1/reply");
    let Some(index) = reply_index(build).and_then(|p| read_json(&p)) else {
        return Vec::new();
    };
    let Some(model) = index["reply"]["codemodel-v2"]["jsonFile"]
        .as_str()
        .and_then(|f| read_json(&reply.join(f)))
    else {
        return Vec::new();
    };
    let source_root = model["paths"]["source"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_default();
    let build_root = model["paths"]["build"]
        .as_str()
        .map(PathBuf::from)
        .unwrap_or_else(|| build.to_path_buf());
    let mut out = Vec::new();
    let Some(targets) = model["configurations"][0]["targets"].as_array() else {
        return out;
    };
    for t in targets {
        let Some(target) = t["jsonFile"]
            .as_str()
            .and_then(|f| read_json(&reply.join(f)))
        else {
            continue;
        };
        if target["type"] != "EXECUTABLE" {
            continue;
        }
        let Some(program) = target["artifacts"][0]["path"].as_str() else {
            continue;
        };
        let sources = target["sources"]
            .as_array()
            .map(|s| {
                s.iter()
                    .filter_map(|s| s["path"].as_str())
                    .map(|p| source_root.join(p))
                    .collect()
            })
            .unwrap_or_default();
        out.push(ExeTarget {
            name: target["name"].as_str().unwrap_or_default().to_string(),
            program: build_root.join(program),
            sources,
        });
    }
    out
}

fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || lexical(a) == lexical(b)
        || a.canonicalize()
            .ok()
            .is_some_and(|a| b.canonicalize().is_ok_and(|b| a == b))
}

/// `path` with its `.` and `..` components folded away, without touching
/// the disk: `build/../src/a.c` is `src/a.c` even before it exists.
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out
}

/// The target F5 on `source` debugs: the executable that compiles it, or
/// the project's only executable (a library source, such as `geom.cpp`,
/// runs inside it).
pub fn target_for<'a>(targets: &'a [ExeTarget], source: &Path) -> Result<&'a ExeTarget, String> {
    let building: Vec<&ExeTarget> = targets
        .iter()
        .filter(|t| t.sources.iter().any(|s| same_file(s, source)))
        .collect();
    match (building.as_slice(), targets) {
        ([one], _) => Ok(one),
        ([], [only]) => Ok(only),
        ([], []) => Err(String::from("the CMake project has no executable target")),
        (several, _) => {
            let names: Vec<&str> = if several.is_empty() {
                targets.iter().map(|t| t.name.as_str()).collect()
            } else {
                several.iter().map(|t| t.name.as_str()).collect()
            };
            Err(format!(
                "several executables could run this file ({}): add a launch.json configuration naming one",
                names.join(", ")
            ))
        }
    }
}

fn run(cmd: &mut Command, what: &str) -> Result<String, Failed> {
    // `.output()`, never `.status()`: a compiler must not inherit croft's
    // TTY, or its diagnostics spray over the UI.
    let out = cmd
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| Failed::new(format!("{what}: {e}"), ""))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    if out.status.success() {
        Ok(text)
    } else {
        let first_error = text
            .lines()
            .find(|l| l.contains("error"))
            .or_else(|| text.lines().find(|l| !l.trim().is_empty()))
            .unwrap_or("")
            .trim()
            .to_string();
        Err(Failed::new(
            format!("{what} failed ({}): {first_error}", out.status),
            text,
        ))
    }
}

/// Build the CMake project in `dir` and return the program that runs
/// `source`. The File API query is written first, so a build folder
/// configured without it is re-configured once to answer it. Only the
/// target that runs `source` is built when the reply already names it, so
/// an unrelated broken target does not stop the debug.
fn cmake_build(workspace: &Path, dir: &Path, source: &Path) -> Result<Built, Failed> {
    let query = dir.join(".cmake/api/v1/query/codemodel-v2");
    if reply_index(dir).is_none() {
        let _ = std::fs::create_dir_all(query.parent().unwrap_or(dir));
        let _ = std::fs::write(&query, "");
        run(
            Command::new("cmake").arg(dir).current_dir(workspace),
            "cmake (configure)",
        )?;
    }
    let shown = dir
        .strip_prefix(workspace)
        .unwrap_or(dir)
        .display()
        .to_string();
    let build = |target: Option<&str>| {
        let mut cmd = Command::new("cmake");
        cmd.arg("--build").arg(dir).current_dir(workspace);
        if let Some(name) = target {
            cmd.arg("--target").arg(name);
        }
        run(&mut cmd, &format!("cmake --build {shown}"))
    };
    let chosen = target_for(&executable_targets(dir), source)
        .ok()
        .map(|t| t.name.clone());
    build(chosen.as_deref())?;
    // The build may re-configure and change the reply: resolve again, and
    // build the target it now names if that is another one.
    let targets = executable_targets(dir);
    let target = target_for(&targets, source).map_err(|e| Failed::new(e, ""))?;
    if chosen.as_deref() != Some(target.name.as_str()) {
        build(Some(&target.name))?;
    }
    Ok(Built {
        binary: target.program.clone(),
        how: format!("cmake --build {shown} (target {})", target.name),
    })
}

/// The compiler and flags `compile_commands.json` gives `source`, and the
/// folder they run in. Looked for at the workspace root and in a configured
/// CMake build folder.
pub fn compile_entry(workspace: &Path, source: &Path) -> Option<(Vec<String>, PathBuf)> {
    let db = [workspace.join("compile_commands.json")]
        .into_iter()
        .chain(cmake_build_dir(workspace).map(|d| d.join("compile_commands.json")))
        .find_map(|p| read_json(&p))?;
    db.as_array()?.iter().find_map(|e| {
        let dir = PathBuf::from(e["directory"].as_str()?);
        let file = PathBuf::from(e["file"].as_str()?);
        let file = if file.is_absolute() {
            file
        } else {
            dir.join(file)
        };
        if !same_file(&file, source) {
            return None;
        }
        let args = match e["arguments"].as_array() {
            Some(a) => a
                .iter()
                .filter_map(|a| a.as_str().map(str::to_string))
                .collect(),
            None => shlex::split(e["command"].as_str()?)?,
        };
        (!args.is_empty()).then_some((args, dir))
    })
}

/// `entry` (a compile command for `file`) turned into a debug build of
/// that one file into `out`: its object-file output dropped, debug info
/// and no optimisation added.
pub fn debug_compile_args(
    entry: &[String],
    file: &Path,
    entry_dir: &Path,
    out: &Path,
) -> Vec<String> {
    let mut args = Vec::new();
    let mut skip = false;
    for a in entry.iter().skip(1) {
        if skip {
            skip = false;
            continue;
        }
        let is_file = {
            let p = Path::new(a);
            let p = if p.is_absolute() {
                p.to_path_buf()
            } else {
                entry_dir.join(p)
            };
            same_file(&p, file)
        };
        match a.as_str() {
            "-c" => {}
            "-o" => skip = true,
            _ if a.starts_with("-o") && a.len() > 2 => {}
            _ if a.starts_with("-O") => {}
            _ if is_file => {}
            _ => args.push(a.clone()),
        }
    }
    args.extend([
        String::from("-g"),
        String::from("-O0"),
        file.to_string_lossy().into_owned(),
        String::from("-o"),
        out.to_string_lossy().into_owned(),
    ]);
    args
}

/// Where croft puts a program it compiles from `source` itself.
pub fn cached_binary(cache: &Path, source: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut h);
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("program"));
    cache.join(format!("{:016x}", h.finish())).join(stem)
}

/// Build the program F5 on the C/C++ file `source` debugs, as the module
/// docs describe. `cache` holds what croft compiles itself.
pub fn build_for_debug(workspace: &Path, source: &Path, cache: &Path) -> Result<Built, Failed> {
    if let Some(dir) = cmake_build_dir(workspace) {
        return cmake_build(workspace, &dir, source);
    }
    let out = cached_binary(cache, source);
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| Failed::new(format!("{}: {e}", parent.display()), ""))?;
    }
    let name = source
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if let Some((entry, dir)) = compile_entry(workspace, source) {
        let compiler = entry[0].clone();
        let args = debug_compile_args(&entry, source, &dir, &out);
        run(
            Command::new(&compiler).args(&args).current_dir(&dir),
            &compiler,
        )?;
        return Ok(Built {
            binary: out,
            how: format!("{compiler} with {name}'s compile_commands.json flags"),
        });
    }
    let c = source
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "c" | "h" | "m"));
    let compiler = if c { "cc" } else { "c++" };
    run(
        Command::new(compiler)
            .args(["-g", "-O0"])
            .arg(source)
            .arg("-o")
            .arg(&out)
            .current_dir(source.parent().unwrap_or(workspace)),
        compiler,
    )?;
    Ok(Built {
        binary: out,
        how: format!("{compiler} {name}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, text: &str) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, text).unwrap();
        p
    }

    fn have(cmds: &[&str]) -> bool {
        let ok = cmds.iter().all(|c| crate::lsp::manager::is_on_path(c));
        if !ok {
            eprintln!("SKIPPED: needs {cmds:?} on PATH");
        }
        ok
    }

    /// The issue's project: `main.cpp` includes `include/geom.hpp` and
    /// calls into `src/geom.cpp`.
    fn cmake_project(root: &Path) {
        write(
            root,
            "include/geom.hpp",
            "#pragma once\nnamespace geom { double twice(double); }\n",
        );
        write(
            root,
            "src/geom.cpp",
            "#include \"geom.hpp\"\nnamespace geom { double twice(double x) { return 2*x; } }\n",
        );
        write(
            root,
            "src/main.cpp",
            "#include <cstdio>\n#include \"geom.hpp\"\nint main() { std::printf(\"%f\\n\", geom::twice(2)); }\n",
        );
        write(
            root,
            "CMakeLists.txt",
            "cmake_minimum_required(VERSION 3.10)\nproject(cp CXX)\nset(CMAKE_EXPORT_COMPILE_COMMANDS ON)\nadd_executable(app src/main.cpp src/geom.cpp)\ntarget_include_directories(app PRIVATE include)\n",
        );
    }

    /// #1359: F5 in a configured CMake project builds the project and
    /// debugs the executable that compiles the file, even from a source
    /// it shares with no other target, and leaves nothing in `src/`.
    #[test]
    fn a_cmake_project_builds_its_target_for_debugging() {
        if !have(&["cmake", "c++"]) {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        cmake_project(root);
        let st = Command::new("cmake")
            .args(["-S", ".", "-B", "build"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(st.status.success(), "{st:?}");
        std::os::unix::fs::symlink(
            "build/compile_commands.json",
            root.join("compile_commands.json"),
        )
        .unwrap();
        let cache = tmp.path().join("cache");
        for file in ["src/main.cpp", "src/geom.cpp"] {
            let built = build_for_debug(root, &root.join(file), &cache).unwrap();
            assert_eq!(built.binary, root.join("build/app"), "{file}");
            assert_eq!(built.how, "cmake --build build (target app)");
            let ran = Command::new(&built.binary).output().unwrap();
            assert_eq!(String::from_utf8_lossy(&ran.stdout), "4.000000\n");
        }
        let in_src: Vec<_> = std::fs::read_dir(root.join("src"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(
            in_src.len(),
            2,
            "nothing written beside the sources: {in_src:?}"
        );
    }

    /// #1359: F5 builds only the target that runs the file, so another
    /// executable that does not compile does not stop the debug.
    #[test]
    fn only_the_files_target_is_built() {
        if !have(&["cmake", "c++"]) {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        cmake_project(root);
        write(root, "tool/broken.cpp", "int main() { return nope; }\n");
        let lists = root.join("CMakeLists.txt");
        let mut text = std::fs::read_to_string(&lists).unwrap();
        text.push_str("add_executable(broken tool/broken.cpp)\n");
        std::fs::write(&lists, text).unwrap();
        let st = Command::new("cmake")
            .args(["-S", ".", "-B", "build"])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(st.status.success(), "{st:?}");
        let cache = tmp.path().join("cache");
        let main = root.join("src/main.cpp");
        // The first build writes the File API query and re-configures.
        for _ in 0..2 {
            let built = build_for_debug(root, &main, &cache)
                .unwrap_or_else(|f| panic!("{}\n{}", f.summary, f.output));
            assert_eq!(built.binary, root.join("build/app"));
        }
        // Negative: debugging the broken program still reports its error.
        let err = build_for_debug(root, &root.join("tool/broken.cpp"), &cache).unwrap_err();
        assert!(err.output.contains("nope"), "{}", err.output);
    }

    /// #1359: with only a `compile_commands.json`, a one-file program
    /// builds with that entry's flags (here, its include path).
    #[test]
    fn a_compile_commands_entry_gives_its_flags() {
        if !have(&["c++"]) {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        write(
            root,
            "include/two.hpp",
            "#pragma once\nconstexpr int two = 2;\n",
        );
        let main = write(
            root,
            "tool/main.cpp",
            "#include <cstdio>\n#include \"two.hpp\"\nint main() { std::printf(\"%d\\n\", two); }\n",
        );
        let db = serde_json::json!([{
            "directory": root.join("out").display().to_string(),
            "command": format!("/usr/bin/c++ -I{} -std=c++17 -O2 -o tool.o -c {}", root.join("include").display(), main.display()),
            "file": main.display().to_string(),
        }]);
        std::fs::create_dir_all(root.join("out")).unwrap();
        std::fs::write(root.join("compile_commands.json"), db.to_string()).unwrap();
        let cache = tmp.path().join("cache");
        let built = build_for_debug(root, &main, &cache).unwrap();
        assert!(built.binary.starts_with(&cache), "{:?}", built.binary);
        assert!(built.how.contains("compile_commands.json"), "{}", built.how);
        let ran = Command::new(&built.binary).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&ran.stdout), "2\n");
    }

    #[test]
    fn a_compile_command_becomes_a_debug_build_of_the_file() {
        let entry: Vec<String> = shlex::split(
            "/usr/bin/c++ -Iinc -DX=1 -std=c++17 -O2 -o CMakeFiles/app.dir/a.cpp.o -c /p/src/a.cpp",
        )
        .unwrap();
        let args = debug_compile_args(
            &entry,
            Path::new("/p/src/a.cpp"),
            Path::new("/p/build"),
            Path::new("/c/a"),
        );
        assert_eq!(
            args,
            [
                "-Iinc",
                "-DX=1",
                "-std=c++17",
                "-g",
                "-O0",
                "/p/src/a.cpp",
                "-o",
                "/c/a"
            ]
        );
        // A relative source in the entry is the same file.
        let entry: Vec<String> = shlex::split("cc -c ../src/a.c -o a.o").unwrap();
        let args = debug_compile_args(
            &entry,
            Path::new("/p/src/a.c"),
            Path::new("/p/build"),
            Path::new("/c/a"),
        );
        assert_eq!(args, ["-g", "-O0", "/p/src/a.c", "-o", "/c/a"]);
    }

    /// #1359, negative: a lone file, with no build files, is still built
    /// alone, into the cache.
    #[test]
    fn a_lone_file_still_builds_alone_into_the_cache() {
        if !have(&["c++"]) {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let hello = write(tmp.path(), "hello.cpp", "int main() { return 0; }\n");
        let cache = tmp.path().join("cache");
        let built = build_for_debug(tmp.path(), &hello, &cache).unwrap();
        assert!(built.binary.starts_with(&cache));
        assert_eq!(built.how, "c++ hello.cpp");
        assert!(built.binary.is_file());
        assert!(!tmp.path().join("hello.dbg").exists());
    }

    /// #1359, negative: a CMake project that is not configured is not
    /// guessed at, and a failed build keeps the compiler's whole output.
    #[test]
    fn an_unconfigured_project_and_a_failed_build() {
        let tmp = tempfile::tempdir().unwrap();
        cmake_project(tmp.path());
        assert_eq!(cmake_build_dir(tmp.path()), None);
        if !have(&["c++"]) {
            return;
        }
        let main = tmp.path().join("src/main.cpp");
        let err = build_for_debug(tmp.path(), &main, &tmp.path().join("cache")).unwrap_err();
        assert!(err.summary.starts_with("c++ failed"), "{}", err.summary);
        assert!(err.summary.contains("geom.hpp"), "{}", err.summary);
        assert!(err.output.contains("geom.hpp"), "{}", err.output);
    }

    fn exe(name: &str, sources: &[&str]) -> ExeTarget {
        ExeTarget {
            name: name.into(),
            program: PathBuf::from(format!("/b/{name}")),
            sources: sources.iter().map(PathBuf::from).collect(),
        }
    }

    #[test]
    fn the_target_is_the_one_that_compiles_the_file() {
        let targets = [
            exe("app", &["/s/main.cpp", "/s/geom.cpp"]),
            exe("tool", &["/s/tool.cpp"]),
        ];
        assert_eq!(
            target_for(&targets, Path::new("/s/tool.cpp")).unwrap().name,
            "tool"
        );
        assert_eq!(
            target_for(&targets, Path::new("/s/main.cpp")).unwrap().name,
            "app"
        );
        // A library source of a one-program project runs in that program.
        let one = [exe("app", &["/s/main.cpp"])];
        assert_eq!(
            target_for(&one, Path::new("/s/lib.cpp")).unwrap().name,
            "app"
        );
        // Negative: no way to tell which of several programs to run.
        let err = target_for(&targets, Path::new("/s/lib.cpp")).unwrap_err();
        assert!(err.contains("app, tool"), "{err}");
        let shared = [exe("a", &["/s/x.cpp"]), exe("b", &["/s/x.cpp"])];
        assert!(
            target_for(&shared, Path::new("/s/x.cpp"))
                .unwrap_err()
                .contains("a, b")
        );
        assert!(target_for(&[], Path::new("/s/x.cpp")).is_err());
    }
}
