//! Query plans as a tree the console can indent: from PostgreSQL's and
//! MariaDB/MySQL's JSON EXPLAIN, and from SQLite's EXPLAIN QUERY PLAN rows.

use serde_json::Value;

use crate::sql::Dialect;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Node {
    pub label: String,
    /// Short facts: table, index, rows, cost.
    pub detail: String,
    pub children: Vec<Node>,
}

/// The EXPLAIN form that gives a machine-readable plan.
pub fn explain_sql(dialect: Dialect, sql: &str) -> String {
    let sql = sql.trim().trim_end_matches(';');
    match dialect {
        Dialect::MySql => format!("EXPLAIN FORMAT=JSON {sql}"),
        Dialect::Postgres => format!("EXPLAIN (FORMAT JSON) {sql}"),
        Dialect::Sqlite => format!("EXPLAIN QUERY PLAN {sql}"),
    }
}

fn scalar(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

/// PostgreSQL: `[{"Plan": {"Node Type": …, "Plans": […]}}]`.
pub fn postgres(json: &str) -> Result<Node, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let plan = v.get(0).and_then(|p| p.get("Plan")).ok_or("no plan in the reply")?;
    Ok(pg_node(plan))
}

fn pg_node(p: &Value) -> Node {
    let label = p.get("Node Type").and_then(Value::as_str).unwrap_or("?").to_owned();
    let mut facts = Vec::new();
    for (key, name) in [
        ("Relation Name", "on"),
        ("Index Name", "index"),
        ("Join Type", "join"),
        ("Plan Rows", "rows"),
        ("Total Cost", "cost"),
        ("Filter", "filter"),
        ("Index Cond", "cond"),
        ("Hash Cond", "cond"),
    ] {
        if let Some(v) = p.get(key).and_then(scalar) {
            facts.push(format!("{name} {v}"));
        }
    }
    let children = p.get("Plans").and_then(Value::as_array).map(|a| a.iter().map(pg_node).collect()).unwrap_or_default();
    Node { label, detail: facts.join(" · "), children }
}

/// MariaDB and MySQL: nested objects (`query_block`, `table`,
/// `nested_loop`…). Each object becomes a node named by its key; plain
/// values become its details.
pub fn mysql(json: &str) -> Result<Node, String> {
    let v: Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let mut root = Node { label: "query".into(), ..Default::default() };
    match &v {
        Value::Object(map) => {
            for (k, val) in map {
                walk(k, val, &mut root);
            }
        }
        other => walk("query", other, &mut root),
    }
    Ok(if root.children.len() == 1 && root.detail.is_empty() { root.children.remove(0) } else { root })
}

fn walk(key: &str, v: &Value, parent: &mut Node) {
    match v {
        Value::Object(map) => {
            let mut node = Node { label: key.replace('_', " "), ..Default::default() };
            let mut facts = Vec::new();
            for (k, val) in map {
                match scalar(val) {
                    Some(s) if facts.len() < 8 => facts.push(format!("{} {s}", k.replace('_', " "))),
                    Some(_) => {}
                    None => walk(k, val, &mut node),
                }
            }
            node.detail = facts.join(" · ");
            parent.children.push(node);
        }
        Value::Array(items) => {
            for item in items {
                walk(key, item, parent);
            }
        }
        other => {
            if let Some(s) = scalar(other) {
                parent.detail.push_str(&s);
            }
        }
    }
}

/// SQLite rows `(id, parent, detail)`, parents before children.
pub fn sqlite(rows: &[(i64, i64, String)]) -> Node {
    fn build(id: i64, rows: &[(i64, i64, String)]) -> Vec<Node> {
        rows.iter()
            .filter(|(_, parent, _)| *parent == id)
            .map(|(i, _, detail)| Node { label: detail.clone(), detail: String::new(), children: build(*i, rows) })
            .collect()
    }
    Node { label: "QUERY PLAN".into(), detail: String::new(), children: build(0, rows) }
}

/// The tree flattened for display: (depth, label, detail).
pub fn lines(root: &Node) -> Vec<(usize, String, String)> {
    fn go(n: &Node, depth: usize, out: &mut Vec<(usize, String, String)>) {
        out.push((depth, n.label.clone(), n.detail.clone()));
        for c in &n.children {
            go(c, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    go(root, 0, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn postgres_plan() {
        let json = r#"[{"Plan": {"Node Type": "Hash Join", "Join Type": "Inner", "Total Cost": 12.5, "Plan Rows": 10,
            "Plans": [{"Node Type": "Seq Scan", "Relation Name": "orders", "Plan Rows": 100},
                      {"Node Type": "Hash", "Plans": [{"Node Type": "Index Scan", "Relation Name": "customers", "Index Name": "customers_pkey"}]}]}}]"#;
        let n = postgres(json).unwrap();
        let l = lines(&n);
        assert_eq!(l[0], (0, "Hash Join".into(), "join Inner · rows 10 · cost 12.5".into()));
        assert_eq!(l[1], (1, "Seq Scan".into(), "on orders · rows 100".into()));
        assert_eq!(l[3], (2, "Index Scan".into(), "on customers · index customers_pkey".into()));
    }

    #[test]
    fn mariadb_plan() {
        let json = r#"{"query_block": {"select_id": 1, "table": {"table_name": "orders", "access_type": "ALL", "rows": 180}}}"#;
        let n = mysql(json).unwrap();
        assert_eq!(n.label, "query block");
        assert_eq!(n.detail, "select id 1");
        assert_eq!(n.children[0].label, "table");
        assert!(n.children[0].detail.contains("table name orders"));
    }

    #[test]
    fn sqlite_rows() {
        let n = sqlite(&[(2, 0, "SCAN orders".into()), (5, 0, "SEARCH customers USING INTEGER PRIMARY KEY".into()), (7, 5, "x".into())]);
        let l = lines(&n);
        assert_eq!(l.len(), 4);
        assert_eq!(l[3], (2, "x".into(), String::new()));
    }

    #[test]
    fn explain_forms() {
        assert_eq!(explain_sql(Dialect::Postgres, "SELECT 1;"), "EXPLAIN (FORMAT JSON) SELECT 1");
    }
}
