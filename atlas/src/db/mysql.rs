//! MariaDB and MySQL: structure from information_schema.

use std::collections::BTreeMap;

use atlas_core::schema::{Column, Relation, RelationKind, Schema, Table, TableKind};
use futures_util::FutureExt;
use sqlx::Row;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow};

use super::{CONNECT_TIMEOUT, Pool, Session, number, text};

pub async fn connect(host: &str, port: u16, socket: Option<&str>, user: &str, password: &str, database: &str) -> Result<Pool, String> {
    let mut options = MySqlConnectOptions::new()
        .host(host)
        .port(port)
        .username(user)
        .charset("utf8mb4");
    // Empty when only the list of databases is wanted.
    if !database.is_empty() {
        options = options.database(database);
    }
    if !password.is_empty() {
        options = options.password(password);
    }
    if let Some(socket) = socket {
        options = options.socket(socket);
    }
    let pool = MySqlPoolOptions::new()
        .max_connections(3)
        .acquire_timeout(CONNECT_TIMEOUT)
        // Every connection is read-only for its whole life. The SQL console
        // refuses `SET SESSION TRANSACTION READ WRITE` before it is sent.
        .after_connect(|conn, _| {
            async move {
                sqlx::raw_sql("SET SESSION TRANSACTION READ ONLY").execute(conn).await?;
                Ok(())
            }
            .boxed()
        })
        .connect_with(options)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Pool::MySql(pool))
}

pub fn cell(row: &MySqlRow, i: usize) -> Option<String> {
    // Text protocol: every value arrives as text, binary columns as bytes.
    match row.try_get_unchecked::<Option<String>, _>(i) {
        Ok(v) => v,
        Err(_) => row
            .try_get_unchecked::<Option<Vec<u8>>, _>(i)
            .ok()
            .flatten()
            .map(|b| super::hex(&b)),
    }
}

pub async fn schema(session: &Session) -> Result<Schema, String> {
    let tables = session
        .rows(
            "SELECT TABLE_NAME, TABLE_TYPE, TABLE_ROWS, ENGINE, TABLE_COMMENT FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME",
        )
        .await?;
    let columns = session
        .rows(
            "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, COLUMN_COMMENT \
             FROM information_schema.COLUMNS WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME, ORDINAL_POSITION",
        )
        .await?;
    let foreign = session
        .rows(
            "SELECT CONSTRAINT_NAME, TABLE_NAME, COLUMN_NAME, REFERENCED_TABLE_NAME, REFERENCED_COLUMN_NAME \
             FROM information_schema.KEY_COLUMN_USAGE \
             WHERE TABLE_SCHEMA = DATABASE() AND REFERENCED_TABLE_SCHEMA = DATABASE() AND REFERENCED_TABLE_NAME IS NOT NULL \
             ORDER BY TABLE_NAME, CONSTRAINT_NAME, ORDINAL_POSITION",
        )
        .await?;
    let unique = session
        .rows(
            "SELECT TABLE_NAME, INDEX_NAME, COLUMN_NAME FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA = DATABASE() AND NON_UNIQUE = 0 ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX",
        )
        .await?;

    let mut schema = Schema::default();
    for row in tables {
        let engine = row[3].clone().unwrap_or_default().to_lowercase();
        schema.tables.push(Table {
            name: text(row[0].clone()),
            kind: if row[1].as_deref() == Some("VIEW") { TableKind::View } else { TableKind::Table },
            rows: number(&row[2]),
            // Only MyISAM and Aria keep an exact count.
            rows_estimated: !matches!(engine.as_str(), "myisam" | "aria"),
            comment: row[4].clone().filter(|c| !c.is_empty()),
            ..Default::default()
        });
    }
    for row in columns {
        if let Some(t) = schema.tables.iter_mut().find(|t| Some(&t.name) == row[0].as_ref()) {
            t.columns.push(Column {
                name: text(row[1].clone()),
                data_type: text(row[2].clone()),
                nullable: row[3].as_deref() == Some("YES"),
                primary: row[4].as_deref() == Some("PRI"),
                default: row[5].clone(),
                comment: row[6].clone().filter(|c| !c.is_empty()),
            });
        }
    }
    let mut sets: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for row in unique {
        sets.entry((text(row[0].clone()), text(row[1].clone()))).or_default().push(text(row[2].clone()));
    }
    for ((table, index), cols) in sets {
        if index == "PRIMARY" {
            continue;
        }
        if let Some(t) = schema.tables.iter_mut().find(|t| t.name == table) {
            t.unique.push(cols);
        }
    }
    let mut relations: Vec<Relation> = Vec::new();
    for row in foreign {
        let (name, from, col, to, to_col) =
            (text(row[0].clone()), text(row[1].clone()), text(row[2].clone()), text(row[3].clone()), text(row[4].clone()));
        match relations.iter_mut().find(|r| r.name.as_deref() == Some(&name) && r.from_table == from) {
            Some(r) => {
                r.from_columns.push(col);
                r.to_columns.push(to_col);
            }
            None => relations.push(Relation {
                name: Some(name),
                from_table: from,
                from_columns: vec![col],
                to_table: to,
                to_columns: vec![to_col],
                kind: RelationKind::Declared,
            }),
        }
    }
    schema.relations = relations;
    Ok(schema)
}
