//! SQLite files, opened read-only.

use std::path::Path;

use atlas_core::schema::{Column, Relation, RelationKind, Schema, Table, TableKind};
use atlas_core::sql::{Dialect, ident, literal};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, TypeInfo, ValueRef};

use super::{CONNECT_TIMEOUT, Pool, Session, number, text};

pub async fn connect(path: &Path) -> Result<Session, String> {
    let options = SqliteConnectOptions::new().filename(path).read_only(true).create_if_missing(false);
    let pool = SqlitePoolOptions::new()
        .max_connections(2)
        .acquire_timeout(CONNECT_TIMEOUT)
        .connect_with(options)
        .await
        .map_err(|e| e.to_string())?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "SQLite".into());
    Ok(Session { pool: Pool::Sqlite(pool), dialect: Dialect::Sqlite, name, _tunnel: None })
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
        .rows("SELECT name, type FROM sqlite_master WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite_%' ORDER BY name")
        .await?;
    let mut schema = Schema::default();
    for row in tables {
        let name = text(row[0].clone());
        let view = row[1].as_deref() == Some("view");
        let mut table = Table {
            name: name.clone(),
            kind: if view { TableKind::View } else { TableKind::Table },
            ..Default::default()
        };
        for c in session.rows(&format!("SELECT name, type, \"notnull\", dflt_value, pk FROM pragma_table_info({})", lit(&name))).await? {
            table.columns.push(Column {
                name: text(c[0].clone()),
                data_type: text(c[1].clone()).to_lowercase(),
                nullable: c[2].as_deref() != Some("1"),
                primary: c[4].as_deref().is_some_and(|v| v != "0"),
                default: c[3].clone(),
                comment: None,
            });
        }
        // Small files are the rule; an exact count is affordable.
        table.rows = number(&session.scalar(format!("SELECT COUNT(*) FROM {}", q(&name))).await.ok().flatten());
        if !view {
            for index in session.rows(&format!("SELECT name FROM pragma_index_list({}) WHERE \"unique\" = 1", lit(&name))).await? {
                let index = text(index[0].clone());
                let cols: Vec<String> = session
                    .rows(&format!("SELECT name FROM pragma_index_info({}) ORDER BY seqno", lit(&index)))
                    .await?
                    .into_iter()
                    .map(|r| text(r[0].clone()))
                    .collect();
                if !cols.is_empty() {
                    table.unique.push(cols);
                }
            }
            let mut relations: Vec<(String, Relation)> = Vec::new();
            for fk in session
                .rows(&format!("SELECT id, \"table\", \"from\", \"to\" FROM pragma_foreign_key_list({}) ORDER BY id, seq", lit(&name)))
                .await?
            {
                let id = text(fk[0].clone());
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
                        },
                    )),
                }
            }
            schema.relations.extend(relations.into_iter().map(|(_, r)| r));
        }
        schema.tables.push(table);
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
