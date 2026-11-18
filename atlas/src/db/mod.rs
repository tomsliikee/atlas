//! Live sources: MariaDB/MySQL, PostgreSQL and SQLite through sqlx.
//!
//! Every query Atlas sends goes as plain text (no prepared statements): the
//! MySQL text protocol and PostgreSQL's simple query protocol return every
//! value as text, which is exactly what a viewer shows. SQLite values are
//! typed per cell and are turned into text here.
//!
//! Sessions are read-only on the server side; see `connect`.

mod mysql;
mod postgres;
mod sqlite;
pub mod ssh;

use std::path::PathBuf;
use std::time::Duration;

use atlas_core::schema::Schema;
use atlas_core::sql::Dialect;
use atlas_core::store::Connection;
use futures_util::StreamExt;
use sqlx::{AssertSqlSafe, Column, Row};

pub use ssh::Tunnel;

/// Rows as text, as a viewer shows them.
#[derive(Clone, Debug, Default)]
pub struct ResultSet {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// More rows were there than asked for.
    pub truncated: bool,
}

#[derive(Clone)]
enum Pool {
    MySql(sqlx::MySqlPool),
    Postgres(sqlx::PgPool),
    Sqlite(sqlx::SqlitePool),
}

/// An open database. Cloning shares the pool; the tunnel (if any) closes
/// when the last clone is dropped.
#[derive(Clone)]
pub struct Session {
    pool: Pool,
    pub dialect: Dialect,
    pub name: String,
    _tunnel: Option<std::sync::Arc<Tunnel>>,
}

/// Where to connect.
pub enum Target {
    Server(Connection),
    File(PathBuf),
}

const CONNECT_TIMEOUT: Duration = Duration::from_secs(12);

pub async fn connect(target: Target, password: String) -> Result<Session, String> {
    match target {
        Target::File(path) => sqlite::connect(&path).await,
        Target::Server(conn) => {
            let tunnel = match &conn.ssh {
                Some(ssh) => {
                    let remote_host = if conn.host.is_empty() { "127.0.0.1".to_owned() } else { conn.host.clone() };
                    Some(std::sync::Arc::new(Tunnel::open(ssh, &remote_host, conn.port).await?))
                }
                None => None,
            };
            let (host, port, socket) = match &tunnel {
                Some(t) => ("127.0.0.1".to_owned(), t.local_port, None),
                None => (conn.host.clone(), conn.port, conn.socket.clone()),
            };
            let pool = match conn.dialect {
                Dialect::MySql => mysql::connect(&host, port, socket.as_deref(), &conn.user, &password, &conn.database).await?,
                Dialect::Postgres => postgres::connect(&host, port, socket.as_deref(), &conn.user, &password, &conn.database).await?,
                Dialect::Sqlite => return Err("SQLite is opened as a file".into()),
            };
            Ok(Session { pool, dialect: conn.dialect, name: conn.database.clone(), _tunnel: tunnel })
        }
    }
}

/// Databases `conn`'s user may open, for picking one. MariaDB connects
/// without a database; PostgreSQL needs one to connect to, so it tries the
/// typed one, then `postgres`.
pub async fn list_databases(conn: Connection, password: String) -> Result<Vec<String>, String> {
    let (session, sql) = match conn.dialect {
        Dialect::MySql => {
            let c = Connection { database: String::new(), ..conn };
            (connect(Target::Server(c), password).await?, "SHOW DATABASES")
        }
        Dialect::Postgres => {
            let first = if conn.database.is_empty() { "postgres".to_owned() } else { conn.database.clone() };
            let session = match connect(Target::Server(Connection { database: first.clone(), ..conn.clone() }), password.clone()).await {
                Ok(s) => s,
                Err(e) if first != "postgres" && atlas_core::problem::classify(&e).is_about_database() => {
                    connect(Target::Server(Connection { database: "postgres".into(), ..conn }), password).await?
                }
                Err(e) => return Err(e),
            };
            (
                session,
                "SELECT datname FROM pg_database WHERE datallowconn AND NOT datistemplate \
                 AND has_database_privilege(datname, 'CONNECT') ORDER BY datname",
            )
        }
        Dialect::Sqlite => return Err("SQLite is opened as a file".into()),
    };
    let rows = session.rows(sql).await;
    session.close().await;
    // The server's own bookkeeping is never what someone wants to browse.
    const SYSTEM: [&str; 4] = ["information_schema", "performance_schema", "mysql", "sys"];
    Ok(rows?
        .into_iter()
        .filter_map(|r| r.into_iter().next().flatten())
        .filter(|name| !SYSTEM.contains(&name.as_str()))
        .collect())
}

impl Session {
    /// Tables, columns, keys and declared relations, plus inferred ones.
    pub async fn schema(&self) -> Result<Schema, String> {
        let mut schema = match &self.pool {
            Pool::MySql(_) => mysql::schema(self).await,
            Pool::Postgres(_) => postgres::schema(self).await,
            Pool::Sqlite(_) => sqlite::schema(self).await,
        }?;
        schema.name = self.name.clone();
        atlas_core::infer::infer(&mut schema);
        Ok(schema)
    }

    /// Runs `sql` and returns at most `limit` rows.
    pub async fn query(&self, sql: String, limit: usize) -> Result<ResultSet, String> {
        match &self.pool {
            Pool::MySql(p) => collect(sqlx::raw_sql(AssertSqlSafe(sql)).fetch(p), limit, mysql::cell).await,
            Pool::Postgres(p) => collect(sqlx::raw_sql(AssertSqlSafe(sql)).fetch(p), limit, postgres::cell).await,
            Pool::Sqlite(p) => collect(sqlx::raw_sql(AssertSqlSafe(sql)).fetch(p), limit, sqlite::cell).await,
        }
        .map_err(|e| e.to_string())
    }

    /// The first column of the first row, as text.
    pub async fn scalar(&self, sql: String) -> Result<Option<String>, String> {
        let set = self.query(sql, 1).await?;
        Ok(set.rows.into_iter().next().and_then(|r| r.into_iter().next().flatten()))
    }

    pub async fn close(&self) {
        match &self.pool {
            Pool::MySql(p) => p.close().await,
            Pool::Postgres(p) => p.close().await,
            Pool::Sqlite(p) => p.close().await,
        }
    }

    /// Text rows of an internal query (catalog reads).
    async fn rows(&self, sql: &str) -> Result<Vec<Vec<Option<String>>>, String> {
        Ok(self.query(sql.to_owned(), usize::MAX).await?.rows)
    }
}

async fn collect<R: Row>(
    mut stream: futures_util::stream::BoxStream<'_, Result<R, sqlx::Error>>,
    limit: usize,
    cell: fn(&R, usize) -> Option<String>,
) -> Result<ResultSet, sqlx::Error> {
    let mut set = ResultSet::default();
    while let Some(row) = stream.next().await {
        let row = row?;
        if set.columns.is_empty() {
            set.columns = row.columns().iter().map(|c| c.name().to_owned()).collect();
        }
        if set.rows.len() == limit {
            set.truncated = true;
            break;
        }
        set.rows.push((0..row.len()).map(|i| cell(&row, i)).collect());
    }
    Ok(set)
}

/// Bytes that are not text, shown the way MariaDB's client shows them.
pub(crate) fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(2 + bytes.len() * 2);
    s.push_str("0x");
    for b in bytes {
        let _ = write!(s, "{b:02X}");
    }
    s
}

pub(crate) fn text(value: Option<String>) -> String {
    value.unwrap_or_default()
}

pub(crate) fn number(value: &Option<String>) -> Option<u64> {
    value.as_deref().and_then(|v| v.trim().parse::<f64>().ok()).map(|v| v.max(0.0) as u64)
}

#[cfg(test)]
mod tests;
