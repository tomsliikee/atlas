//! SQLite files, opened read-only unless the user opens one for editing.

use std::path::Path;

use atlas_core::guard::Access;
use atlas_core::schema::{Column, FkAction, Index, Relation, RelationKind, Schema, Table, TableKind, Trigger};
use atlas_core::sql::{Dialect, ident, literal};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, TypeInfo, ValueRef};

use super::{CONNECT_TIMEOUT, Pool, Session, number, text};

pub async fn connect(path: &Path, access: Access) -> Result<Session, String> {
    // sqlx turns foreign key enforcement on for every connection.
    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(access == Access::ReadOnly)
        .create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .acquire_timeout(CONNECT_TIMEOUT)
        .connect_with(options)
        .await
        .map_err(|e| e.to_string())?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "SQLite".into());
    Ok(Session { pool: Pool::Sqlite(pool), dialect: Dialect::Sqlite, name, access, _tunnel: None })
}

pub fn cell(row: &SqliteRow, i: usize) -> Option<String> {
    let raw = row.try_get_raw(i).ok()?;
    if raw.is_null() {
        return None;
    }
    // The storage class of this very value, not the column's declaration.
    match raw.type_info().name() {
        "INTEGER" => row.try_get_unchecked::<i64, _>(i).ok().map(|v| v.to_string()),
        "REAL" => row.try_get_unchecked::<f64, _>(i).ok().map(|v| v.to_string()),
        "BLOB" => row.try_get_unchecked::<Vec<u8>, _>(i).ok().map(|b| match String::from_utf8(b) {
            Ok(s) => s,
            Err(e) => super::hex(e.as_bytes()),
        }),
        _ => row.try_get_unchecked::<String, _>(i).ok(),
    }
}

pub async fn schema(session: &Session) -> Result<Schema, String> {
    let q = |s: &str| ident(Dialect::Sqlite, s);
    let lit = |s: &str| literal(Dialect::Sqlite, s);
    let tables = session
        .rows(
            "SELECT name, type, sql FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name",
        )
        .await?;
    let triggers = session.rows_or_none("SELECT name, tbl_name, sql FROM sqlite_master WHERE type = 'trigger' ORDER BY name").await;
    let mut schema = Schema::default();
    for row in tables {
        let name = text(row[0].clone());
        let view = row[1].as_deref() == Some("view");
        // SQLite keeps no catalog of checks, generated columns or
        // AUTOINCREMENT; the CREATE statement it stores has them.
        let sql = text(row[2].clone());
        let (parsed, _) = atlas_core::dump::parse("x", &sql);
        let parsed = parsed.tables.into_iter().next();
        let mut table = Table {
            name: name.clone(),
            kind: if view { TableKind::View } else { TableKind::Table },
            checks: parsed.as_ref().map(|p| p.checks.clone()).unwrap_or_default(),
            definition: if view { parsed.as_ref().and_then(|p| p.definition.clone()) } else { None },
            ..Default::default()
        };
        let parsed_col = |n: &str| parsed.as_ref().and_then(|p| p.column(n)).cloned();
        for c in session.rows(&format!("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_xinfo({}) WHERE hidden IN (0, 2, 3)", lit(&name))).await? {
            let col_name = text(c[0].clone());
            let p = parsed_col(&col_name);
            table.columns.push(Column {
                data_type: text(c[1].clone()).to_lowercase(),
                nullable: c[2].as_deref() != Some("1"),
                primary: c[4].as_deref().is_some_and(|v| v != "0"),
                default: c[3].clone(),
                comment: None,
                enum_values: None,
                generated: p.as_ref().and_then(|p| p.generated.clone()),
                // Only the rowid alias can be AUTOINCREMENT, and the parser
                // may drop the keyword, so the stored text decides.
                auto_increment: p.is_some_and(|p| p.auto_increment)
                    || (c[4].as_deref() == Some("1") && sql.to_ascii_uppercase().contains("AUTOINCREMENT")),
                name: col_name,
            });
        }
        // Small files are the rule; an exact count is affordable.
        table.rows = number(&session.scalar(format!("SELECT COUNT(*) FROM {}", q(&name))).await.ok().flatten());
        if !view {
            // origin: 'pk' the primary key, 'u' a UNIQUE constraint, 'c' a
            // CREATE INDEX.
            for index in session
                .rows(&format!("SELECT name, \"unique\", origin FROM pragma_index_list({}) WHERE origin <> 'pk'", lit(&name)))
                .await?
            {
                let index_name = text(index[0].clone());
                let cols: Vec<String> = session
                    .rows(&format!("SELECT name FROM pragma_index_info({}) ORDER BY seqno", lit(&index_name)))
                    .await?
                    .into_iter()
                    .map(|r| r[0].clone().unwrap_or_else(|| "(expression)".into()))
                    .collect();
                if cols.is_empty() {
                    continue;
                }
                if index[2].as_deref() == Some("u") {
                    table.unique.push(cols);
                } else {
                    table.indexes.push(Index {
                        name: index_name,
                        columns: cols,
                        unique: index[1].as_deref() == Some("1"),
                        kind: String::new(),
                    });
                }
            }
            let mut relations: Vec<(String, Relation)> = Vec::new();
            for fk in session
                .rows(&format!(
                    "SELECT id, \"table\", \"from\", \"to\", on_delete, on_update FROM pragma_foreign_key_list({}) ORDER BY id, seq",
                    lit(&name)
                ))
                .await?
            {
                let id = text(fk[0].clone());
                let action = |v: &Option<String>| v.as_deref().and_then(FkAction::parse).filter(|a| *a != FkAction::NoAction);
                let (on_delete, on_update) = (action(&fk[4]), action(&fk[5]));
                let (to, from, to_col) = (text(fk[1].clone()), text(fk[2].clone()), fk[3].clone());
                match relations.iter_mut().find(|(i, _)| *i == id) {
                    Some((_, r)) => {
                        r.from_columns.push(from);
                        r.to_columns.extend(to_col);
                    }
                    None => relations.push((
                        id,
                        Relation {
                            name: None,
                            from_table: name.clone(),
                            from_columns: vec![from],
                            to_table: to,
                            to_columns: to_col.into_iter().collect(),
                            kind: RelationKind::Declared,
                            on_delete,
                            on_update,
                        },
                    )),
                }
            }
            schema.relations.extend(relations.into_iter().map(|(_, r)| r));
        }
        schema.tables.push(table);
    }
    for row in triggers {
        let body = text(row[2].clone());
        let upper = body.to_ascii_uppercase();
        let timing = ["INSTEAD OF", "BEFORE", "AFTER"].into_iter().find(|t| upper.contains(&format!(" {t} "))).unwrap_or("");
        let event = ["INSERT", "UPDATE", "DELETE"].into_iter().find(|e| upper.contains(&format!(" {e} "))).unwrap_or("");
        if let Some(t) = schema.tables.iter_mut().find(|t| Some(&t.name) == row[1].as_ref()) {
            t.triggers.push(Trigger { name: text(row[0].clone()), timing: timing.into(), event: event.into(), body });
        }
    }
    // `REFERENCES parent` without a column means the parent's primary key.
    let tables = schema.tables.clone();
    for r in &mut schema.relations {
        if r.to_columns.is_empty()
            && let Some(p) = tables.iter().find(|t| t.name == r.to_table)
        {
            r.to_columns = p.primary_key().into_iter().map(str::to_owned).collect();
        }
    }
    Ok(schema)
}
