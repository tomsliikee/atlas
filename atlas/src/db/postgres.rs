//! PostgreSQL: structure from pg_catalog, for the session's current schema
//! (usually `public`).

use std::collections::BTreeMap;

use atlas_core::guard::Access;
use atlas_core::schema::{
    Check, Column, FkAction, Index, Relation, RelationKind, Routine, RoutineKind, Schema, Table, TableKind, Trigger,
};
use sqlx::Row;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};

use super::{CONNECT_TIMEOUT, Pool, Session, text};

pub async fn connect(
    host: &str,
    port: u16,
    socket: Option<&str>,
    user: &str,
    password: &str,
    database: &str,
    access: Access,
) -> Result<Pool, String> {
    // Read-only for the whole session unless opened for editing, and
    // strings without backslash escapes, which the filter quoting relies on.
    let read_only = if access == Access::ReadOnly { "on" } else { "off" };
    let mut options = PgConnectOptions::new()
        .host(host)
        .port(port)
        .username(user)
        .database(database)
        .application_name("Atlas")
        .options([("default_transaction_read_only", read_only), ("standard_conforming_strings", "on")]);
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
             pg_get_expr(d.adbin, d.adrelid), col_description(c.oid, a.attnum), a.attidentity::text, a.attgenerated::text, \
             (SELECT string_agg(e.enumlabel, chr(31) ORDER BY e.enumsortorder) FROM pg_enum e WHERE e.enumtypid = a.atttypid) \
             FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p', 'v', 'm', 'f') AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY c.relname, a.attnum",
        )
        .await?;
    let constraints = session
        .rows(
            "SELECT con.conname, con.contype::text, c.relname, a.attname, fc.relname, fa.attname, \
             con.confdeltype::text, con.confupdtype::text \
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
    let checks = session
        .rows_or_none(
            "SELECT c.relname, con.conname, pg_get_constraintdef(con.oid, true) FROM pg_constraint con \
             JOIN pg_class c ON c.oid = con.conrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = current_schema() AND con.contype = 'c' ORDER BY c.relname, con.conname",
        )
        .await;
    // Indexes that back a primary key or unique constraint are already
    // known as such.
    let indexes = session
        .rows_or_none(
            "SELECT t.relname, i.relname, ix.indisunique::text, am.amname, \
             (SELECT string_agg(pg_get_indexdef(ix.indexrelid, k, true), chr(31) ORDER BY k) \
              FROM generate_series(1, ix.indnkeyatts) k) \
             FROM pg_index ix JOIN pg_class i ON i.oid = ix.indexrelid JOIN pg_class t ON t.oid = ix.indrelid \
             JOIN pg_namespace n ON n.oid = t.relnamespace JOIN pg_am am ON am.oid = i.relam \
             WHERE n.nspname = current_schema() AND NOT ix.indisprimary \
             AND NOT EXISTS (SELECT 1 FROM pg_constraint con WHERE con.conindid = ix.indexrelid) \
             ORDER BY t.relname, i.relname",
        )
        .await;
    let triggers = session
        .rows_or_none(
            "SELECT c.relname, t.tgname, pg_get_triggerdef(t.oid, true) FROM pg_trigger t \
             JOIN pg_class c ON c.oid = t.tgrelid JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = current_schema() AND NOT t.tgisinternal ORDER BY c.relname, t.tgname",
        )
        .await;
    let views = session
        .rows_or_none(
            "SELECT c.relname, pg_get_viewdef(c.oid, true) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = current_schema() AND c.relkind IN ('v', 'm')",
        )
        .await;
    let routines = session
        .rows_or_none(
            "SELECT p.proname, p.prokind::text, pg_get_functiondef(p.oid) FROM pg_proc p \
             JOIN pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = current_schema() AND p.prokind IN ('f', 'p') \
             AND NOT EXISTS (SELECT 1 FROM pg_depend d WHERE d.objid = p.oid AND d.deptype = 'e') \
             ORDER BY p.proname",
        )
        .await;

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
            let identity = row[6].as_deref().is_some_and(|v| !v.is_empty());
            let stored = row[7].as_deref().is_some_and(|v| !v.is_empty());
            let default = row[4].clone();
            let serial = default.as_deref().is_some_and(|d| d.starts_with("nextval("));
            t.columns.push(Column {
                name: text(row[1].clone()),
                data_type: text(row[2].clone()),
                nullable: row[3].as_deref() == Some("true"),
                primary: false,
                generated: if stored { default.clone() } else { None },
                default: if stored { None } else { default },
                comment: row[5].clone(),
                enum_values: row[8].as_ref().map(|v| v.split('\u{1f}').map(str::to_owned).collect()),
                auto_increment: identity || serial,
            });
        }
    }
    // (kind, table, name) → (columns, foreign table, foreign columns)
    type Key = (String, String, String);
    type Group = (Vec<String>, String, Vec<String>);
    let mut grouped: BTreeMap<Key, Group> = BTreeMap::new();
    let mut actions: BTreeMap<Key, (Option<FkAction>, Option<FkAction>)> = BTreeMap::new();
    let action = |v: &Option<String>| v.as_deref().and_then(FkAction::parse).filter(|a| *a != FkAction::NoAction);
    for row in constraints {
        let key = (text(row[1].clone()), text(row[2].clone()), text(row[0].clone()));
        actions.entry(key.clone()).or_insert_with(|| (action(&row[6]), action(&row[7])));
        let entry = grouped.entry(key).or_insert_with(|| (Vec::new(), text(row[4].clone()), Vec::new()));
        entry.0.push(text(row[3].clone()));
        if let Some(f) = &row[5] {
            entry.2.push(f.clone());
        }
    }
    for ((kind, table, name), (cols, ftable, fcols)) in grouped {
        let (on_delete, on_update) =
            actions.get(&(kind.clone(), table.clone(), name.clone())).copied().unwrap_or_default();
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
                on_delete,
                on_update,
            }),
            _ => {}
        }
    }
    for row in checks {
        let def = text(row[2].clone());
        let expr = def.strip_prefix("CHECK ").unwrap_or(&def).trim();
        let expr = expr.strip_prefix('(').and_then(|e| e.strip_suffix(')')).unwrap_or(expr).to_owned();
        if let Some(t) = schema.tables.iter_mut().find(|t| Some(&t.name) == row[0].as_ref()) {
            t.checks.push(Check { name: row[1].clone(), expr });
        }
    }
    for row in indexes {
        if let Some(t) = schema.tables.iter_mut().find(|t| Some(&t.name) == row[0].as_ref()) {
            let kind = text(row[3].clone());
            t.indexes.push(Index {
                name: text(row[1].clone()),
                unique: row[2].as_deref() == Some("true"),
                kind: if kind == "btree" { String::new() } else { kind },
                columns: row[4].as_deref().unwrap_or("").split('\u{1f}').map(|c| c.trim_matches('"').to_owned()).collect(),
            });
        }
    }
    for row in triggers {
        let body = text(row[2].clone());
        // "CREATE TRIGGER name BEFORE INSERT OR UPDATE ON …"
        let words: Vec<&str> = body.split_whitespace().collect();
        let on = words.iter().position(|w| w.eq_ignore_ascii_case("ON")).unwrap_or(words.len());
        let timing_at = 3.min(on);
        let (timing, event) = if words.get(3).is_some_and(|w| w.eq_ignore_ascii_case("INSTEAD")) {
            ("INSTEAD OF".to_owned(), words[5.min(on)..on].join(" "))
        } else {
            (words.get(timing_at).map(|w| w.to_string()).unwrap_or_default(), words[(timing_at + 1).min(on)..on].join(" "))
        };
        if let Some(t) = schema.tables.iter_mut().find(|t| Some(&t.name) == row[0].as_ref()) {
            t.triggers.push(Trigger { name: text(row[1].clone()), timing, event, body });
        }
    }
    for row in views {
        if let Some(t) = schema.tables.iter_mut().find(|t| Some(&t.name) == row[0].as_ref()) {
            t.definition = row[1].clone();
        }
    }
    for row in routines {
        schema.routines.push(Routine {
            name: text(row[0].clone()),
            kind: if row[1].as_deref() == Some("p") { RoutineKind::Procedure } else { RoutineKind::Function },
            definition: text(row[2].clone()),
        });
    }
    let names: Vec<String> = schema.tables.iter().map(|t| t.name.clone()).collect();
    schema.relations.retain(|r| names.contains(&r.to_table));
    Ok(schema)
}
