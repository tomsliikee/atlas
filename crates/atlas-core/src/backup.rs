//! Atlas' own backup format: a plain SQL script that Atlas' importer, the
//! mysql/psql/sqlite3 clients and Atlas' dump reader all understand. Tables
//! first, then the rows, then indexes, foreign keys, views and triggers, so
//! the rows go in without the keys getting in the way.
//!
//! The rows are fetched page by page by the caller and handed to
//! [`rows`]; this module only writes text.

use crate::ddl;
use crate::schema::{Schema, Table};
use crate::sql::{self, Dialect};

/// Rows per INSERT statement.
pub const BATCH: usize = 200;

pub fn header(dialect: Dialect, schema: &Schema, when: &str) -> String {
    let mut out = format!("-- Atlas backup of {}\n-- {when}\n\n", schema.name);
    match dialect {
        Dialect::MySql => out.push_str("SET NAMES utf8mb4;\nSET FOREIGN_KEY_CHECKS = 0;\n\n"),
        Dialect::Postgres => out.push_str("SET client_encoding = 'UTF8';\nSET standard_conforming_strings = on;\n\n"),
        Dialect::Sqlite => out.push_str("PRAGMA foreign_keys = OFF;\n\n"),
    }
    out
}

/// Types and tables: everything that must exist before the rows.
pub fn structure(dialect: Dialect, schema: &Schema) -> String {
    ddl::schema_script(dialect, schema).before
}

/// One batch of rows of `table` as a single INSERT.
pub fn rows(dialect: Dialect, table: &Table, columns: &[String], rows: &[Vec<Option<String>>]) -> String {
    if rows.is_empty() {
        return String::new();
    }
    let types: Vec<String> =
        columns.iter().map(|c| table.column(c).map(|c| c.data_type.clone()).unwrap_or_default()).collect();
    sql::insert_batch(dialect, &table.name, columns, &types, rows)
}

/// Indexes, foreign keys, views, triggers and routines, and the footer.
pub fn footer(dialect: Dialect, schema: &Schema) -> String {
    let mut out = String::from("\n");
    out.push_str(&ddl::schema_script(dialect, schema).after);
    // Identity columns would hand out ids that the restored rows already
    // use; move them past the highest one.
    if dialect == Dialect::Postgres {
        for t in &schema.tables {
            for c in t.columns.iter().filter(|c| c.auto_increment) {
                let (ti, ci) = (sql::ident(dialect, &t.name), sql::ident(dialect, &c.name));
                out.push_str(&format!(
                    "SELECT setval(pg_get_serial_sequence({}, {}), COALESCE((SELECT MAX({ci}) FROM {ti}), 0) + 1, false);\n",
                    sql::quoted(dialect, &ti),
                    sql::quoted(dialect, &c.name),
                ));
            }
        }
    }
    match dialect {
        Dialect::MySql => out.push_str("\nSET FOREIGN_KEY_CHECKS = 1;\n"),
        Dialect::Sqlite => out.push_str("\nPRAGMA foreign_keys = ON;\n"),
        Dialect::Postgres => {}
    }
    out
}

/// Tables in an order where parents come before children, for restoring
/// into a database that checks keys row by row. Cycles keep their order.
pub fn table_order(schema: &Schema) -> Vec<&Table> {
    let mut done: Vec<&str> = Vec::new();
    let tables: Vec<&Table> = schema.tables.iter().filter(|t| t.kind == crate::schema::TableKind::Table).collect();
    while done.len() < tables.len() {
        let before = done.len();
        for t in &tables {
            if done.contains(&t.name.as_str()) {
                continue;
            }
            let ready = schema
                .relations
                .iter()
                .filter(|r| r.kind == crate::schema::RelationKind::Declared && r.from_table == t.name && r.to_table != t.name)
                .all(|r| done.contains(&r.to_table.as_str()) || !tables.iter().any(|x| x.name == r.to_table));
            if ready {
                done.push(&t.name);
            }
        }
        if done.len() == before {
            // A cycle: take the rest as they are.
            for t in &tables {
                if !done.contains(&t.name.as_str()) {
                    done.push(&t.name);
                }
            }
        }
    }
    done.iter().filter_map(|n| tables.iter().find(|t| t.name == *n).copied()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dump;
    use crate::schema::fixture::shop;

    #[test]
    fn parents_first() {
        let s = shop();
        let order: Vec<&str> = table_order(&s).iter().map(|t| t.name.as_str()).collect();
        let at = |n: &str| order.iter().position(|x| *x == n).unwrap();
        assert!(at("customers") < at("orders"));
        assert!(at("orders") < at("order_lines"));
        assert!(at("products") < at("order_lines"));
        assert_eq!(order.len(), 5);
    }

    /// A whole backup splits into statements and reads back as the schema.
    #[test]
    fn backup_reads_back() {
        for d in [Dialect::MySql, Dialect::Postgres, Dialect::Sqlite] {
            let s = shop();
            let mut text = header(d, &s, "2026-11-18");
            text.push_str(&structure(d, &s));
            let t = s.table("customers").unwrap();
            text.push_str(&rows(d, t, &["id".into(), "name".into()], &[vec![Some("1".into()), Some("A; 'b'".into())]]));
            text.push_str(&footer(d, &s));
            let split = dump::split(&text);
            assert!(split.statements.iter().any(|x| x.starts_with("INSERT INTO") && x.contains("A; ''b''")), "{d:?}");
            let (back, report) = dump::parse("shop", &text);
            assert!(report.skipped.is_empty(), "{d:?} {:?}", report.skipped);
            assert_eq!(back.tables.len(), 5, "{d:?}");
            assert_eq!(back.relations.len(), 4, "{d:?}");
        }
    }
}
