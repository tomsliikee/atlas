//! PostgreSQL: structure from pg_catalog, for the session's current schema
//! (usually `public`).

use std::collections::BTreeMap;

use atlas_core::schema::{Column, Relation, RelationKind, Schema, Table, TableKind};
use sqlx::Row;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};

use super::{CONNECT_TIMEOUT, Pool, Session, text};

pub async fn connect(host: &str, port: u16, socket: Option<&str>, user: &str, password: &str, database: &str) -> Result<Pool, String> {
    let mut options = PgConnectOptions::new()
        .host(host)
        .port(port)
        .username(user)
        .database(database)
        .application_name("Atlas")
        // Read-only for the whole session, and strings without backslash
        // escapes, which the filter quoting relies on.
        .options([("default_transaction_read_only", "on"), ("standard_conforming_strings", "on")]);
    if !password.is_empty() {
        options = options.password(password);
    }
    if let Some(socket) = socket {
        options = options.socket(socket);
    }
    let pool = PgPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(CONNECT_TIMEOUT)
        .connect_with(options)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Pool::Postgres(pool))
}

pub fn cell(row: &PgRow, i: usize) -> Option<String> {
    // Simple query protocol: every value is text.
    row.try_get_unchecked::<Option<String>, _>(i).ok().flatten()
}

pub async fn schema(session: &Session) -> Result<Schema, String> {
    let tables = session
        .rows(
            "SELECT c.relname, c.relkind, c.reltuples::bigint, obj_description(c.oid, 'pg_class') \
             FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND NOT c.relispartition \
             ORDER BY c.relname",
        )
        .await?;
    let columns = session
        .rows(
            "SELECT c.relname, a.attname, format_type(a.atttypid, a.atttypmod), (NOT a.attnotnull)::text, \
             pg_get_expr(d.adbin, d.adrelid), col_description(c.oid, a.attnum) \
             FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY c.relname, a.attnum",
        )
        .await?;
    let constraints = session
        .rows(
            "SELECT con.conname, con.contype::text, c.relname, a.attname, fc.relname, fa.attname \
             FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL unnest(con.conkey) WITH ORDINALITY AS k(attnum, ord) \
             JOIN pg_attribute a ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
             LEFT JOIN pg_class fc ON fc.oid = con.confrelid \
             LEFT JOIN pg_attribute fa ON fa.attrelid = con.confrelid AND fa.attnum = con.confkey[k.ord] \
             WHERE n.nspname = current_schema() AND con.contype IN ('p', 'u', 'f') \
             ORDER BY c.relname, con.conname, k.ord",
        )
        .await?;

    let mut schema = Schema::default();
    for row in tables {
        let kind = row[1].as_deref().unwrap_or("r");
        // reltuples is -1 for a table never analysed.
        let rows = row[2].as_deref().and_then(|v| v.parse::<i64>().ok()).filter(|v| *v >= 0).map(|v| v as u64);
        schema.tables.push(Table {
            name: text(row[0].clone()),
            kind: if matches!(kind, "v" | "m") { TableKind::View } else { TableKind::Table },
            rows,
            rows_estimated: true,
            comment: row[3].clone(),
            ..Default::default()
        });
    }
    for row in columns {
        if let Some(t) = schema.tables.iter_mut().find(|t| Some(&t.name) == row[0].as_ref()) {
            t.columns.push(Column {
                name: text(row[1].clone()),
                data_type: text(row[2].clone()),
                nullable: row[3].as_deref() == Some("true"),
                primary: false,
                default: row[4].clone(),
                comment: row[5].clone(),
            });
        }
    }
    // (kind, table, name) → (columns, foreign table, foreign columns)
    type Key = (String, String, String);
    type Group = (Vec<String>, String, Vec<String>);
    let mut grouped: BTreeMap<Key, Group> = BTreeMap::new();
    for row in constraints {
        let key = (text(row[1].clone()), text(row[2].clone()), text(row[0].clone()));
        let entry = grouped.entry(key).or_insert_with(|| (Vec::new(), text(row[4].clone()), Vec::new()));
        entry.0.push(text(row[3].clone()));
        if let Some(f) = &row[5] {
            entry.2.push(f.clone());
        }
    }
    for ((kind, table, name), (cols, ftable, fcols)) in grouped {
        let Some(t) = schema.tables.iter_mut().find(|t| t.name == table) else { continue };
        match kind.as_str() {
            "p" => {
                for c in t.columns.iter_mut().filter(|c| cols.contains(&c.name)) {
                    c.primary = true;
                }
            }
            "u" => t.unique.push(cols),
            "f" => schema.relations.push(Relation {
                name: Some(name),
                from_table: table,
                from_columns: cols,
                to_table: ftable,
                to_columns: fcols,
                kind: RelationKind::Declared,
            }),
            _ => {}
        }
    }
    let names: Vec<String> = schema.tables.iter().map(|t| t.name.clone()).collect();
    schema.relations.retain(|r| names.contains(&r.to_table));
    Ok(schema)
}
