//! The CodeQL AST Viewer's pure half (#578): which query prints a file's
//! AST, what the file is called inside a database's source archive, and
//! the tree read back from the query's `nodes` / `edges` graph.
//!
//! VS Code's CodeQL extension finds the query by tag: every language's
//! library pack (`codeql/<lang>-all`) ships a `@kind graph` query tagged
//! `ide-contextual-queries/print-ast`, taking the file through the
//! external predicate `selectedSourceFile`. croft resolves it the same
//! way, with a query suite, and runs it through the CLI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// A query suite selecting `lang`'s library-pack query tagged
/// `ide-contextual-queries/<tag>`, the way VS Code finds its contextual
/// queries.
pub fn contextual_suite(lang: &str, tag: &str) -> String {
    format!(
        "- from: codeql/{lang}-all\n  queries: .\n- include:\n    kind: graph\n    tags contain: ide-contextual-queries/{tag}\n"
    )
}

/// `codeql` arguments listing the queries `suite` selects, as JSON.
pub fn resolve_args(suite: &Path) -> Vec<String> {
    vec![
        String::from("resolve"),
        String::from("queries"),
        String::from("--format=json"),
        suite.display().to_string(),
    ]
}

/// The first query path in `codeql resolve queries --format=json` output.
pub fn first_query(json: &str) -> Option<PathBuf> {
    let list: Vec<String> = serde_json::from_str(json.trim()).ok()?;
    list.into_iter().next().map(PathBuf::from)
}

/// The folder of the pack `query` belongs to: the nearest one above it
/// holding a `qlpack.yml` or `codeql-pack.yml`.
pub fn pack_dir(query: &Path) -> Option<PathBuf> {
    query
        .ancestors()
        .skip(1)
        .find(|d| d.join("qlpack.yml").is_file() || d.join("codeql-pack.yml").is_file())
        .map(Path::to_path_buf)
}

/// The value file for `--external=selectedSourceFile=<file>`: one CSV row
/// holding the archive path.
pub fn selected_file_csv(archive: &str) -> String {
    format!("\"{}\"\n", archive.replace('"', "\"\""))
}

/// `codeql` arguments running the print-AST `query` on `db` for the file
/// named in `selected` (the CSV of [`selected_file_csv`]).
pub fn run_args(query: &Path, db: &Path, selected: &Path, bqrs: &Path) -> Vec<String> {
    vec![
        String::from("query"),
        String::from("run"),
        format!("--database={}", db.display()),
        format!("--output={}", bqrs.display()),
        format!("--external=selectedSourceFile={}", selected.display()),
        query.display().to_string(),
    ]
}

/// `codeql` arguments decoding the AST graph as JSON, with each entity's
/// id and location.
pub fn decode_args(bqrs: &Path, out: &Path) -> Vec<String> {
    vec![
        String::from("bqrs"),
        String::from("decode"),
        String::from("--format=json"),
        String::from("--entities=id,url,string"),
        format!("--output={}", out.display()),
        bqrs.display().to_string(),
    ]
}

/// What `file` is called in a database's source archive: its path below
/// one of `source_roots` (an extracted `src.zip` or a database's `src`
/// folder, which both mirror the original absolute paths), else its own
/// absolute path, for a database built from this very tree.
pub fn archive_path(file: &Path, source_roots: &[PathBuf]) -> String {
    for root in source_roots {
        if let Ok(rel) = file.strip_prefix(root) {
            return format!("/{}", rel.display());
        }
    }
    file.display().to_string()
}

/// Where an AST node is: the file's URI and its 1-based start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Loc {
    pub uri: String,
    pub line: u32,
    pub column: u32,
}

impl Loc {
    /// The local path a `file://` URI names.
    pub fn path(&self) -> Option<PathBuf> {
        let rest = self.uri.strip_prefix("file://")?;
        Some(PathBuf::from(crate::sarif::resolve::percent_decode(rest)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AstNode {
    /// What the node is, with the edge that leads to it when that says
    /// more than a position ("body: [Block] { … }").
    pub label: String,
    pub location: Option<Loc>,
    pub children: Vec<usize>,
}

/// A file's AST: its nodes and the top-level ones, in source order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AstTree {
    pub nodes: Vec<AstNode>,
    pub roots: Vec<usize>,
}

/// Read the print-AST query's decoded result: `nodes` rows of (entity,
/// key, value) and `edges` rows of (source, target, key, value). The keys
/// that matter are `semmle.label` (what to show) and `semmle.order` (where
/// among its siblings).
pub fn parse_graph(json: &str) -> Result<AstTree, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("unreadable AST: {e}"))?;
    let tuples = |set: &str| -> Vec<Vec<serde_json::Value>> {
        v.get(set)
            .and_then(|s| s.get("tuples"))
            .and_then(|t| t.as_array())
            .map(|rows| rows.iter().filter_map(|r| r.as_array().cloned()).collect())
            .unwrap_or_default()
    };
    if v.get("nodes").is_none() {
        return Err(String::from("the query returned no AST graph"));
    }
    let mut index: HashMap<i64, usize> = HashMap::new();
    let mut nodes: Vec<AstNode> = Vec::new();
    let mut order: Vec<i64> = Vec::new();
    let mut intern =
        |e: &serde_json::Value, nodes: &mut Vec<AstNode>, order: &mut Vec<i64>| -> Option<usize> {
            let id = e.get("id")?.as_i64()?;
            if let Some(&i) = index.get(&id) {
                return Some(i);
            }
            let location = e.get("url").and_then(|u| {
                Some(Loc {
                    uri: u.get("uri")?.as_str()?.to_string(),
                    line: u.get("startLine")?.as_u64()? as u32,
                    column: u.get("startColumn")?.as_u64()? as u32,
                })
            });
            nodes.push(AstNode {
                label: e
                    .get("label")
                    .and_then(|l| l.as_str())
                    .unwrap_or_default()
                    .to_string(),
                location,
                children: Vec::new(),
            });
            order.push(i64::MAX);
            index.insert(id, nodes.len() - 1);
            Some(nodes.len() - 1)
        };
    let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
    for row in tuples("nodes") {
        let [entity, key, value] = row.as_slice() else {
            continue;
        };
        let Some(i) = intern(entity, &mut nodes, &mut order) else {
            continue;
        };
        match key.as_str() {
            Some("semmle.label") => nodes[i].label = text(value),
            Some("semmle.order") => order[i] = text(value).parse().unwrap_or(i64::MAX),
            _ => {}
        }
    }
    // (parent, child) -> (order, edge label)
    let mut edges: HashMap<(usize, usize), (i64, String)> = HashMap::new();
    for row in tuples("edges") {
        let [source, target, key, value] = row.as_slice() else {
            continue;
        };
        let (Some(s), Some(t)) = (
            intern(source, &mut nodes, &mut order),
            intern(target, &mut nodes, &mut order),
        ) else {
            continue;
        };
        let edge = edges.entry((s, t)).or_insert((i64::MAX, String::new()));
        match key.as_str() {
            Some("semmle.order") => edge.0 = text(value).parse().unwrap_or(i64::MAX),
            Some("semmle.label") => edge.1 = text(value),
            _ => {}
        }
    }
    let mut is_child = vec![false; nodes.len()];
    let mut kids: Vec<Vec<(i64, usize)>> = vec![Vec::new(); nodes.len()];
    for (&(s, t), (ord, label)) in &edges {
        is_child[t] = true;
        kids[s].push((*ord, t));
        // An edge label that is only a position says nothing a reader needs.
        if !label.is_empty() && label.parse::<i64>().is_err() {
            nodes[t].label = format!("{label}: {}", nodes[t].label);
        }
    }
    for (i, mut k) in kids.into_iter().enumerate() {
        k.sort();
        nodes[i].children = k.into_iter().map(|(_, c)| c).collect();
    }
    let mut roots: Vec<usize> = (0..nodes.len()).filter(|&i| !is_child[i]).collect();
    roots.sort_by_key(|&i| {
        let at = nodes[i].location.as_ref().map(|l| (l.line, l.column));
        (order[i], at, i)
    });
    Ok(AstTree { nodes, roots })
}

/// The AST the side bar shows: whose it is, the tree, and which nodes are
/// unfolded. Top-level nodes start folded, as VS Code's viewer does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AstView {
    /// The file the AST is of, as opened in the editor.
    pub file: PathBuf,
    /// Its name in the source archive, which the nodes' locations use.
    pub archive: String,
    /// The database it came from, for the heading.
    pub database: String,
    pub tree: AstTree,
    pub open: std::collections::HashSet<usize>,
}

impl AstView {
    /// The rows the side bar shows, as (depth, node), in tree order: each
    /// node, then its children when it is unfolded.
    pub fn visible(&self) -> Vec<(usize, usize)> {
        fn walk(v: &AstView, i: usize, depth: usize, out: &mut Vec<(usize, usize)>) {
            out.push((depth, i));
            if v.open.contains(&i) {
                for &c in &v.tree.nodes[i].children {
                    walk(v, c, depth + 1, out);
                }
            }
        }
        let mut out = Vec::new();
        for &r in &self.tree.roots {
            walk(self, r, 0, &mut out);
        }
        out
    }

    /// Fold or unfold node `i`; a leaf has nothing to fold.
    pub fn toggle(&mut self, i: usize) {
        if self
            .tree
            .nodes
            .get(i)
            .is_some_and(|n| !n.children.is_empty())
            && !self.open.remove(&i)
        {
            self.open.insert(i);
        }
    }

    /// Where node `i` is on this machine: the opened file when the node is
    /// in it, else the path its location names.
    pub fn target(&self, i: usize) -> Option<(PathBuf, u32, u32)> {
        let loc = self.tree.nodes.get(i)?.location.as_ref()?;
        let path = loc.path()?;
        let path = if path == Path::new(&self.archive) {
            self.file.clone()
        } else {
            path
        };
        Some((path, loc.line, loc.column))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape `codeql bqrs decode --format=json --entities=id,url,string`
    /// gives a print-AST query's result (trimmed from a real Python run).
    const GRAPH: &str = r#"{
      "nodes": {"columns": [], "tuples": [
        [{"id": 0, "label": "[Import] import os", "url": {"uri": "file:///w/app.py", "startLine": 1, "startColumn": 1, "endLine": 1, "endColumn": 9}}, "semmle.order", "1"],
        [{"id": 0, "label": "[Import] import os", "url": {"uri": "file:///w/app.py", "startLine": 1, "startColumn": 1, "endLine": 1, "endColumn": 9}}, "semmle.label", "[Import] import os"],
        [{"id": 1, "label": "[ImportExpr] ", "url": {"uri": "file:///w/app.py", "startLine": 1, "startColumn": 8, "endLine": 1, "endColumn": 9}}, "semmle.label", "[ImportExpr] "],
        [{"id": 2, "label": "[Name] os", "url": {"uri": "file:///w/app.py", "startLine": 1, "startColumn": 8, "endLine": 1, "endColumn": 9}}, "semmle.label", "[Name] os"],
        [{"id": 3, "label": "[FunctionDef] run", "url": {"uri": "file:///w/my%20app.py", "startLine": 3, "startColumn": 1, "endLine": 4, "endColumn": 18}}, "semmle.order", "2"],
        [{"id": 4, "label": "x", "url": {"uri": "file:///w/app.py", "startLine": 4, "startColumn": 5, "endLine": 4, "endColumn": 18}}, "semmle.label", "[ExprStmt] os.system(cmd)"]
      ]},
      "edges": {"columns": [], "tuples": [
        [{"id": 0, "label": "[Import] import os"}, {"id": 2, "label": "[Name] os"}, "semmle.order", "2"],
        [{"id": 0, "label": "[Import] import os"}, {"id": 2, "label": "[Name] os"}, "semmle.label", "2"],
        [{"id": 0, "label": "[Import] import os"}, {"id": 1, "label": "[ImportExpr] "}, "semmle.order", "1"],
        [{"id": 3, "label": "[FunctionDef] run"}, {"id": 4, "label": "x"}, "semmle.order", "0"],
        [{"id": 3, "label": "[FunctionDef] run"}, {"id": 4, "label": "x"}, "semmle.label", "body"]
      ]},
      "graphProperties": {"columns": [], "tuples": []}
    }"#;

    #[test]
    fn the_graph_becomes_a_tree_in_source_order() {
        let t = parse_graph(GRAPH).unwrap();
        let label = |i: usize| t.nodes[i].label.as_str();
        let roots: Vec<&str> = t.roots.iter().map(|&i| label(i)).collect();
        assert_eq!(roots, ["[Import] import os", "[FunctionDef] run"]);
        let import = t.roots[0];
        let kids: Vec<&str> = t.nodes[import].children.iter().map(|&i| label(i)).collect();
        assert_eq!(kids, ["[ImportExpr] ", "[Name] os"], "by edge order");
        let def = t.roots[1];
        let body = t.nodes[def].children[0];
        assert_eq!(
            label(body),
            "body: [ExprStmt] os.system(cmd)",
            "a named edge prefixes its node; a numbered one does not"
        );
        let loc = t.nodes[def].location.clone().unwrap();
        assert_eq!((loc.line, loc.column), (3, 1));
        assert_eq!(loc.path(), Some(PathBuf::from("/w/my app.py")));
        assert!(parse_graph("{}").is_err());
    }

    #[test]
    fn the_view_folds_nodes_and_leads_back_to_the_opened_file() {
        let mut v = AstView {
            file: PathBuf::from("/cache/src/w/app.py"),
            archive: String::from("/w/app.py"),
            tree: parse_graph(GRAPH).unwrap(),
            ..AstView::default()
        };
        let (import, def) = (v.tree.roots[0], v.tree.roots[1]);
        assert_eq!(v.visible(), [(0, import), (0, def)], "folded at first");
        v.toggle(import);
        let kids = v.tree.nodes[import].children.clone();
        assert_eq!(
            v.visible(),
            [(0, import), (1, kids[0]), (1, kids[1]), (0, def)]
        );
        v.toggle(kids[0]);
        assert_eq!(v.visible().len(), 4, "a leaf does not fold");
        v.toggle(import);
        assert_eq!(v.visible().len(), 2);
        assert_eq!(
            v.target(import),
            Some((PathBuf::from("/cache/src/w/app.py"), 1, 1)),
            "a node of the archived file opens the file the user opened"
        );
        assert_eq!(
            v.target(def),
            Some((PathBuf::from("/w/my app.py"), 3, 1)),
            "another file by its own path"
        );
    }

    #[test]
    fn an_open_file_is_named_as_the_source_archive_names_it() {
        let roots = [PathBuf::from("/cache/sources/db-1a2b")];
        assert_eq!(
            archive_path(
                Path::new("/cache/sources/db-1a2b/home/me/src/app.py"),
                &roots
            ),
            "/home/me/src/app.py"
        );
        assert_eq!(
            archive_path(Path::new("/home/me/src/app.py"), &roots),
            "/home/me/src/app.py",
            "a database built from this tree"
        );
        assert_eq!(selected_file_csv("/a \"b\".py"), "\"/a \"\"b\"\".py\"\n");
    }

    #[test]
    fn the_print_ast_query_is_found_by_its_tag_in_the_language_pack() {
        assert_eq!(
            contextual_suite("python", "print-ast"),
            "- from: codeql/python-all\n  queries: .\n- include:\n    kind: graph\n    tags contain: ide-contextual-queries/print-ast\n"
        );
        assert_eq!(
            first_query("[\n  \"/p/python-all/7.2.6/printAst.ql\"\n]\n"),
            Some(PathBuf::from("/p/python-all/7.2.6/printAst.ql"))
        );
        assert_eq!(first_query("[]"), None);
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("pack/sub")).unwrap();
        std::fs::write(dir.path().join("pack/qlpack.yml"), "name: x\n").unwrap();
        assert_eq!(
            pack_dir(&dir.path().join("pack/sub/printAst.ql")),
            Some(dir.path().join("pack"))
        );
    }
}
