//! MariaDB and MySQL: structure from information_schema.

use std::collections::BTreeMap;

use atlas_core::guard::Access;
use atlas_core::schema::{
    Check, Column, FkAction, Index, Relation, RelationKind, Routine, RoutineKind, Schema, Table, TableKind, Trigger,
    enum_values,
};
use atlas_core::sql::{Dialect, quoted};
use futures_util::FutureExt;
use sqlx::Row;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow};

use super::{CONNECT_TIMEOUT, Pool, Session, number, text};

pub async fn connect(
    host: &str,
    port: u16,
    socket: Option<&str>,
    user: &str,
    password: &str,
    database: &str,
    access: Access,
) -> Result<Pool, String> {
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
        // Unless opened for editing, every connection is read-only for its
        // whole life. The SQL console refuses `SET SESSION TRANSACTION READ
        // WRITE` before it is sent.
        .after_connect(move |conn, _| {
            async move {
                if access == Access::ReadOnly {
                    sqlx::raw_sql("SET SESSION TRANSACTION READ ONLY").execute(conn).await?;
                }
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
            "SELECT TABLE_NAME, COLUMN_NAME, COLUMN_TYPE, IS_NULLABLE, COLUMN_KEY, COLUMN_DEFAULT, COLUMN_COMMENT, \
             EXTRA, GENERATION_EXPRESSION \
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
    let indexes = session
        .rows(
            "SELECT TABLE_NAME, INDEX_NAME, COLUMN_NAME, NON_UNIQUE, INDEX_TYPE FROM information_schema.STATISTICS \
             WHERE TABLE_SCHEMA = DATABASE() ORDER BY TABLE_NAME, INDEX_NAME, SEQ_IN_INDEX",
        )
        .await?;
    let rules = session
        .rows_or_none(
            "SELECT CONSTRAINT_NAME, TABLE_NAME, DELETE_RULE, UPDATE_RULE FROM information_schema.REFERENTIAL_CONSTRAINTS \
             WHERE CONSTRAINT_SCHEMA = DATABASE()",
        )
        .await;
    let checks = session
        .rows_or_none(
            "SELECT tc.TABLE_NAME, cc.CONSTRAINT_NAME, cc.CHECK_CLAUSE FROM information_schema.TABLE_CONSTRAINTS tc \
             JOIN information_schema.CHECK_CONSTRAINTS cc \
               ON cc.CONSTRAINT_SCHEMA = tc.CONSTRAINT_SCHEMA AND cc.CONSTRAINT_NAME = tc.CONSTRAINT_NAME \
             WHERE tc.TABLE_SCHEMA = DATABASE() AND tc.CONSTRAINT_TYPE = 'CHECK' ORDER BY tc.TABLE_NAME, cc.CONSTRAINT_NAME",
        )
        .await;
    let triggers = session
        .rows_or_none(
            "SELECT TRIGGER_NAME, EVENT_OBJECT_TABLE, ACTION_TIMING, EVENT_MANIPULATION, ACTION_STATEMENT \
             FROM information_schema.TRIGGERS WHERE TRIGGER_SCHEMA = DATABASE() ORDER BY TRIGGER_NAME",
        )
        .await;
    let views = session
        .rows_or_none("SELECT TABLE_NAME, VIEW_DEFINITION FROM information_schema.VIEWS WHERE TABLE_SCHEMA = DATABASE()")
        .await;
    let routines = session
        .rows_or_none(
            "SELECT ROUTINE_NAME, ROUTINE_TYPE, ROUTINE_DEFINITION FROM information_schema.ROUTINES \
             WHERE ROUTINE_SCHEMA = DATABASE() ORDER BY ROUTINE_NAME",
        )
        .await;
    // MySQL 8 prints string defaults unquoted; MariaDB quotes them.
    let mariadb = session.version().await.unwrap_or_default().to_ascii_lowercase().contains("mariadb");

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
            let data_type = text(row[2].clone());
            let extra = row[7].clone().unwrap_or_default().to_ascii_lowercase();
            let generated = row[8].clone().filter(|g| !g.is_empty() && extra.contains("generated") && !extra.contains("default_generated"));
            let mut default = row[5].clone();
            if !mariadb
                && let Some(d) = &default
                && !extra.contains("default_generated")
            {
                default = Some(quoted(Dialect::MySql, d));
            }
            if generated.is_some() || default.as_deref().is_some_and(|d| d.eq_ignore_ascii_case("null")) {
                default = None;
            }
            t.columns.push(Column {
                name: text(row[1].clone()),
                enum_values: enum_values(&data_type),
                data_type,
                nullable: row[3].as_deref() == Some("YES"),
                primary: row[4].as_deref() == Some("PRI"),
                default,
                comment: row[6].clone().filter(|c| !c.is_empty()),
                generated,
                auto_increment: extra.contains("auto_increment"),
            });
        }
    }
    // (table, index) → (columns, unique, kind)
    let mut sets: BTreeMap<(String, String), (Vec<String>, bool, String)> = BTreeMap::new();
    for row in indexes {
        let entry = sets
            .entry((text(row[0].clone()), text(row[1].clone())))
            .or_insert_with(|| (Vec::new(), row[3].as_deref() == Some("0"), text(row[4].clone()).to_lowercase()));
        entry.0.push(text(row[2].clone()));
    }
    for ((table, index), (columns, unique, kind)) in sets {
        if index == "PRIMARY" {
            continue;
        }
        if let Some(t) = schema.tables.iter_mut().find(|t| t.name == table) {
            let kind = if kind == "btree" { String::new() } else { kind };
            t.indexes.push(Index { name: index, columns, unique, kind });
        }
    }
    for row in checks {
        let (table, name, clause) = (text(row[0].clone()), text(row[1].clone()), text(row[2].clone()));
        // MariaDB guards JSON columns with an implicit json_valid() check.
        if clause.to_ascii_lowercase().starts_with("json_valid(") {
            continue;
        }
        if let Some(t) = schema.tables.iter_mut().find(|t| t.name == table) {
            t.checks.push(Check { name: Some(name), expr: clause });
        }
    }
    for row in triggers {
        let (name, table, timing, event, body) =
            (text(row[0].clone()), text(row[1].clone()), text(row[2].clone()), text(row[3].clone()), text(row[4].clone()));
        if let Some(t) = schema.tables.iter_mut().find(|t| t.name == table) {
            let q = |n: &str| atlas_core::sql::ident(Dialect::MySql, n);
            t.triggers.push(Trigger {
                body: format!("CREATE TRIGGER {} {timing} {event} ON {} FOR EACH ROW {body}", q(&name), q(&table)),
                name,
                timing,
                event,
            });
        }
    }
    for row in views {
        if let Some(t) = schema.tables.iter_mut().find(|t| Some(&t.name) == row[0].as_ref()) {
            t.definition = row[1].clone();
        }
    }
    for row in routines {
        let name = text(row[0].clone());
        let function = row[1].as_deref() == Some("FUNCTION");
        let what = if function { "FUNCTION" } else { "PROCEDURE" };
        // SHOW CREATE gives the whole statement with its parameters; the
        // catalog only has the body.
        let full = session
            .query(format!("SHOW CREATE {what} {}", atlas_core::sql::ident(Dialect::MySql, &name)), 1)
            .await
            .ok()
            .and_then(|r| r.rows.into_iter().next())
            .and_then(|r| r.into_iter().nth(2).flatten());
        schema.routines.push(Routine {
            name,
            kind: if function { RoutineKind::Function } else { RoutineKind::Procedure },
            definition: full.or_else(|| row[2].clone()).unwrap_or_default(),
        });
    }
    let rule = |name: &str, table: &str, i: usize| {
        rules
            .iter()
            .find(|r| r[0].as_deref() == Some(name) && r[1].as_deref() == Some(table))
            .and_then(|r| r[i].as_deref().and_then(FkAction::parse))
            .filter(|a| *a != FkAction::NoAction)
    };
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
                on_delete: rule(&name, &from, 2),
                on_update: rule(&name, &from, 3),
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
