//! CodeQL's View CFG (#578): the control flow graph of the function at the
//! cursor, from the language pack's `ide-contextual-queries/print-cfg`
//! query, laid out as a document croft can show.
//!
//! The query takes the file, line and column through external predicates.
//! Current packs answer with a Mermaid `flowchart` (one string, result set
//! `mermaid`); older ones, and some languages, with a `nodes` / `edges`
//! graph like the print-AST query's. Both read into a [`Cfg`].

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

/// `codeql` arguments running the print-CFG `query` on `db` for the
/// position the three value files name.
pub fn run_args(
    query: &Path,
    db: &Path,
    file: &Path,
    line: &Path,
    column: &Path,
    bqrs: &Path,
) -> Vec<String> {
    vec![
        String::from("query"),
        String::from("run"),
        format!("--database={}", db.display()),
        format!("--output={}", bqrs.display()),
        format!("--external=selectedSourceFile={}", file.display()),
        format!("--external=selectedSourceLine={}", line.display()),
        format!("--external=selectedSourceColumn={}", column.display()),
        query.display().to_string(),
    ]
}

/// A control flow graph: node labels, and edges `(from, to, label)`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cfg {
    pub nodes: Vec<String>,
    pub edges: Vec<(usize, usize, String)>,
    /// The Mermaid source, when the query gave one.
    pub mermaid: Option<String>,
}

/// Read the decoded print-CFG result (`bqrs decode --format=json
/// --entities=id,url,string`), in either shape.
pub fn parse_cfg(json: &str) -> Result<Cfg, String> {
    let v: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("unreadable CFG: {e}"))?;
    let tuples = |set: &str| -> Vec<Vec<serde_json::Value>> {
        v.get(set)
            .and_then(|s| s.get("tuples"))
            .and_then(|t| t.as_array())
            .map(|rows| rows.iter().filter_map(|r| r.as_array().cloned()).collect())
            .unwrap_or_default()
    };
    if v.get("mermaid").is_some() {
        let text: String = tuples("mermaid")
            .iter()
            .filter_map(|r| r.first().and_then(|s| s.as_str()).map(str::to_string))
            .collect::<Vec<_>>()
            .join("\n");
        return Ok(parse_mermaid(&text));
    }
    if v.get("edges").is_none() {
        return Err(String::from("the query returned no control flow graph"));
    }
    let mut cfg = Cfg::default();
    let mut index: HashMap<i64, usize> = HashMap::new();
    let mut intern = |e: &serde_json::Value, cfg: &mut Cfg| -> Option<usize> {
        let id = e.get("id")?.as_i64()?;
        Some(*index.entry(id).or_insert_with(|| {
            let label = e.get("label").and_then(|l| l.as_str()).unwrap_or("?");
            cfg.nodes.push(label.to_string());
            cfg.nodes.len() - 1
        }))
    };
    for row in tuples("nodes") {
        if let [entity, key, value] = row.as_slice()
            && let Some(i) = intern(entity, &mut cfg)
            && key.as_str() == Some("semmle.label")
        {
            cfg.nodes[i] = value.as_str().unwrap_or_default().to_string();
        }
    }
    let mut labels: BTreeMap<(usize, usize), String> = BTreeMap::new();
    for row in tuples("edges") {
        let [pred, succ, key, value] = row.as_slice() else {
            continue;
        };
        let (Some(p), Some(s)) = (intern(pred, &mut cfg), intern(succ, &mut cfg)) else {
            continue;
        };
        let label = labels.entry((p, s)).or_default();
        if key.as_str() == Some("semmle.label") {
            *label = value.as_str().unwrap_or_default().to_string();
        }
    }
    cfg.edges = labels.into_iter().map(|((p, s), l)| (p, s, l)).collect();
    Ok(cfg)
}

/// Read a Mermaid `flowchart`: `id["label"]` nodes and `a --> b`,
/// `a -->|label| b` or `a -- label --> b` edges.
pub fn parse_mermaid(text: &str) -> Cfg {
    let mut cfg = Cfg {
        mermaid: Some(text.trim_end().to_string()),
        ..Cfg::default()
    };
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut node = |id: &str, label: Option<&str>, cfg: &mut Cfg| -> usize {
        let i = *index.entry(id.to_string()).or_insert_with(|| {
            cfg.nodes.push(id.to_string());
            cfg.nodes.len() - 1
        });
        if let Some(l) = label {
            cfg.nodes[i] = l.to_string();
        }
        i
    };
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with("flowchart") || line.starts_with("%%") {
            continue;
        }
        if let Some((from, rest)) = line.split_once("--") {
            let (label, to) = if let Some(r) = rest.strip_prefix(">|") {
                match r.split_once('|') {
                    Some((l, to)) => (l.trim().to_string(), to),
                    None => (String::new(), r),
                }
            } else if let Some(r) = rest.strip_prefix('>') {
                (String::new(), r)
            } else {
                match rest.split_once("-->") {
                    Some((l, to)) => (l.trim().to_string(), to),
                    None => continue,
                }
            };
            let (from, to) = (from.trim(), to.trim());
            if from.is_empty() || to.is_empty() {
                continue;
            }
            let a = node(from, None, &mut cfg);
            let b = node(to, None, &mut cfg);
            cfg.edges.push((a, b, label.trim_matches('"').to_string()));
        } else if let Some((id, rest)) = line.split_once('[') {
            let label = rest
                .trim_end_matches(']')
                .trim_matches('"')
                .replace("#quot;", "\"");
            node(id.trim(), Some(&label), &mut cfg);
        }
    }
    cfg
}

/// The document View CFG opens: each node once, in the order control
/// reaches it from the entry, with where it goes next; then the Mermaid
/// source, for a renderer that draws it.
pub fn render_document(cfg: &Cfg, title: &str) -> String {
    let mut out = format!("# Control flow graph of {title}\n\n");
    if cfg.nodes.is_empty() {
        out.push_str("No control flow graph at that position: put the cursor on a function.\n");
        return out;
    }
    let mut succ: Vec<Vec<(usize, &str)>> = vec![Vec::new(); cfg.nodes.len()];
    let mut has_pred = vec![false; cfg.nodes.len()];
    for (a, b, l) in &cfg.edges {
        succ[*a].push((*b, l.as_str()));
        has_pred[*b] = true;
    }
    // Entry first, then whatever else starts a path, then anything left.
    let mut starts: Vec<usize> = (0..cfg.nodes.len()).filter(|&i| !has_pred[i]).collect();
    starts.sort_by_key(|&i| (cfg.nodes[i] != "Entry", i));
    starts.extend(0..cfg.nodes.len());
    let mut seen = vec![false; cfg.nodes.len()];
    let mut order = Vec::new();
    for s in starts {
        let mut stack = vec![s];
        while let Some(n) = stack.pop() {
            if std::mem::replace(&mut seen[n], true) {
                continue;
            }
            order.push(n);
            for (next, _) in succ[n].iter().rev() {
                if !seen[*next] {
                    stack.push(*next);
                }
            }
        }
    }
    for n in order {
        let next: Vec<String> = succ[n]
            .iter()
            .map(|(b, l)| {
                if l.is_empty() {
                    cfg.nodes[*b].clone()
                } else {
                    format!("{} ({l})", cfg.nodes[*b])
                }
            })
            .collect();
        match next.as_slice() {
            [] => out.push_str(&format!("- {}\n", cfg.nodes[n])),
            _ => out.push_str(&format!(
                "- {} \u{2192} {}\n",
                cfg.nodes[n],
                next.join(", ")
            )),
        }
    }
    if let Some(m) = &cfg.mermaid {
        out.push_str(&format!("\n```mermaid\n{m}\n```\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real print-CFG answer from python-all 7.2.6, trimmed.
    const MERMAID: &str = "flowchart TD\n1[\"Entry\"]\n2[\"Exit\"]\n3[\"Normal Exit\"]\n4[\"cmd\"]\n5[\"os\"]\n6[\"Call #quot;x#quot;\"]\n\n1 --> 4\n3 --> 2\n4 --> 5\n5 -->|true| 6\n5 -- false --> 3\n6 --> 3";

    #[test]
    fn a_mermaid_flowchart_reads_as_nodes_and_edges() {
        let json = serde_json::json!({"mermaid": {"tuples": [[MERMAID]]}, "edges": {"tuples": []}});
        let cfg = parse_cfg(&json.to_string()).unwrap();
        assert_eq!(
            cfg.nodes,
            ["Entry", "Exit", "Normal Exit", "cmd", "os", "Call \"x\""]
        );
        assert!(cfg.edges.contains(&(4, 5, String::from("true"))));
        assert!(cfg.edges.contains(&(4, 2, String::from("false"))));
        assert_eq!(cfg.edges.len(), 6);
        assert_eq!(cfg.mermaid.as_deref(), Some(MERMAID));
    }

    #[test]
    fn a_nodes_and_edges_graph_reads_the_same_way() {
        let json = serde_json::json!({
            "nodes": {"tuples": [[{"id": 7, "label": "Entry"}, "semmle.label", "Entry"]]},
            "edges": {"tuples": [
                [{"id": 7, "label": "Entry"}, {"id": 8, "label": "x = 1"}, "semmle.label", ""],
                [{"id": 8, "label": "x = 1"}, {"id": 9, "label": "Exit"}, "semmle.label", "normal"]
            ]}
        });
        let cfg = parse_cfg(&json.to_string()).unwrap();
        assert_eq!(cfg.nodes, ["Entry", "x = 1", "Exit"]);
        assert_eq!(
            cfg.edges,
            [(0, 1, String::new()), (1, 2, String::from("normal"))]
        );
        assert!(parse_cfg("{}").is_err());
    }

    #[test]
    fn the_document_follows_control_from_the_entry() {
        let json = serde_json::json!({"mermaid": {"tuples": [[MERMAID]]}});
        let cfg = parse_cfg(&json.to_string()).unwrap();
        let doc = render_document(&cfg, "run (app.py:3)");
        let list: Vec<&str> = doc.lines().filter(|l| l.starts_with("- ")).collect();
        assert_eq!(
            list,
            [
                "- Entry \u{2192} cmd",
                "- cmd \u{2192} os",
                "- os \u{2192} Call \"x\" (true), Normal Exit (false)",
                "- Call \"x\" \u{2192} Normal Exit",
                "- Normal Exit \u{2192} Exit",
                "- Exit",
            ]
        );
        assert!(doc.starts_with("# Control flow graph of run (app.py:3)\n"));
        assert!(doc.contains("```mermaid\nflowchart TD\n"));
        let empty = parse_mermaid("flowchart TD\n\n\n");
        assert!(render_document(&empty, "x").contains("put the cursor on a function"));
    }
}
