//! Against real databases. SQLite always runs (a temporary file made from
//! tests/fixtures/demo.sql). MariaDB and PostgreSQL run when
//! `ATLAS_TEST_MARIADB` / `ATLAS_TEST_POSTGRES` are set to
//! `host:port:user:database` and the password is in `ATLAS_TEST_PASSWORD`
//! (or `ATLAS_TEST_PG_PASSWORD`).

use atlas_core::schema::RelationKind;
use atlas_core::sql::{self, Dialect, Filter, Op, Sort};
use atlas_core::store::Connection;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{AssertSqlSafe, ConnectOptions};

use super::{Session, Target, connect};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

async fn demo_sqlite() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("atlas-test-{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let mut conn = SqliteConnectOptions::new().filename(&path).create_if_missing(true).connect().await.unwrap();
    for file in ["../tests/fixtures/demo.sql", "../tests/fixtures/demo-data.sql"] {
        let text = std::fs::read_to_string(file).unwrap();
        sqlx::raw_sql(AssertSqlSafe(text)).execute(&mut conn).await.unwrap();
    }
    path
}

/// The checks every source must pass.
async fn common(session: &Session) {
    let schema = session.schema().await.unwrap();
    let orders = schema.table("orders").expect("orders table");
    assert_eq!(orders.primary_key(), ["id"]);
    assert!(orders.rows.is_some());
    assert!(schema.relations.iter().any(|r| r.from_table == "orders" && r.to_table == "customers" && r.kind == RelationKind::Declared));
    assert!(
        schema.relations.iter().any(|r| r.from_table == "wishlist" && r.to_table == "customers" && r.kind == RelationKind::Inferred),
        "wishlist.customer_id should be inferred"
    );
    let items = schema.table("order_items").unwrap();
    assert_eq!(items.primary_key().len(), 2);
    let payment = schema.relations.iter().find(|r| r.from_table == "payments").unwrap();
    assert_eq!(schema.ends(payment).0, atlas_core::schema::End::ZeroOrOne, "payments.order_id is unique");

    // A page, filtered and sorted, with a quote in the value.
    let f = Filter { column: "last_name".into(), op: Op::Contains, value: "a'".into() };
    let q = sql::page(session.dialect, "customers", &[f], Some(&Sort { column: "id".into(), descending: true }), 5, 0);
    let page = session.query(q, 5).await.unwrap();
    assert!(page.rows.is_empty() || page.columns.contains(&"email".to_string()));
    let all = session.query(sql::page(session.dialect, "customers", &[], None, 10, 0), 10).await.unwrap();
    assert_eq!(all.rows.len(), 10);
    let n = session.scalar(sql::count(session.dialect, "orders", &[])).await.unwrap();
    assert_eq!(n.as_deref(), Some("180"));

    // The server refuses writes, whatever the console's guard says.
    let write = session.query("UPDATE customers SET first_name = 'x' WHERE id = 1".into(), 1).await;
    assert!(write.is_err(), "a write went through");
}

#[test]
fn sqlite_demo() {
    rt().block_on(async {
        let path = demo_sqlite().await;
        let session = connect(Target::File(path.clone()), String::new()).await.unwrap();
        assert_eq!(session.dialect, Dialect::Sqlite);
        common(&session).await;
        // JSON stays text, NULL stays NULL.
        let set = session.query("SELECT attributes, updated_at FROM products WHERE id = 1".into(), 1).await.unwrap();
        assert!(set.rows[0][0].as_deref().is_some_and(|v| v.starts_with('{')));
        assert_eq!(set.rows[0][1], None);
        session.close().await;
        let _ = std::fs::remove_file(path);
    });
}

fn server(var: &str, password_var: &str, dialect: Dialect) -> Option<(Connection, String)> {
    let spec = std::env::var(var).ok()?;
    let parts: Vec<&str> = spec.split(':').collect();
    let [host, port, user, database] = parts[..] else { panic!("{var} must be host:port:user:database") };
    Some((
        Connection {
            id: "test".into(),
            name: "test".into(),
            dialect,
            host: host.into(),
            port: port.parse().unwrap(),
            user: user.into(),
            database: database.into(),
            socket: None,
            ssh: None,
        },
        std::env::var(password_var).unwrap_or_default(),
    ))
}

#[test]
fn mariadb_demo() {
    let Some((conn, password)) = server("ATLAS_TEST_MARIADB", "ATLAS_TEST_PASSWORD", Dialect::MySql) else {
        eprintln!("ATLAS_TEST_MARIADB not set; skipped");
        return;
    };
    rt().block_on(async {
        let session = connect(Target::Server(conn), password).await.unwrap();
        common(&session).await;
        let list = super::list_databases(server("ATLAS_TEST_MARIADB", "ATLAS_TEST_PASSWORD", Dialect::MySql).unwrap().0, std::env::var("ATLAS_TEST_PASSWORD").unwrap()).await.unwrap();
        assert_eq!(list, ["shop"], "only the user's databases, no system ones");
        // Even a session switched back by hand stays read-only for the
        // console, because the guard refuses the statement first.
        assert!(atlas_core::guard::check(Dialect::MySql, "SET SESSION TRANSACTION READ WRITE").is_err());
        session.close().await;
    });
}

#[test]
fn postgres_demo() {
    let Some((conn, password)) = server("ATLAS_TEST_POSTGRES", "ATLAS_TEST_PG_PASSWORD", Dialect::Postgres) else {
        eprintln!("ATLAS_TEST_POSTGRES not set; skipped");
        return;
    };
    rt().block_on(async {
        // A wrong database name still lists the right ones.
        let wrong = Connection { database: "nope".into(), ..conn.clone() };
        let list = super::list_databases(wrong, password.clone()).await.unwrap();
        assert!(list.contains(&"shop".to_string()), "{list:?}");
        let session = connect(Target::Server(conn), password).await.unwrap();
        common(&session).await;
        session.close().await;
    });
}

/// Connects exactly as the app does and only reads the structure; safe for
/// a real database. `ATLAS_PROBE=host:port:user:database`, password in
/// `ATLAS_PROBE_PASSWORD`.
#[test]
fn probe() {
    let Some((conn, password)) = server("ATLAS_PROBE", "ATLAS_PROBE_PASSWORD", Dialect::MySql) else {
        return;
    };
    rt().block_on(async {
        match connect(Target::Server(conn.clone()), password.clone()).await {
            Ok(session) => {
                let schema = session.schema().await;
                eprintln!("probe: connected; schema: {:?}", schema.map(|s| s.tables.len()));
                session.close().await;
            }
            Err(e) => {
                let problem = atlas_core::problem::classify(&e);
                eprintln!("probe: connect failed: {problem:?} ({e})");
                if problem.is_about_database() {
                    eprintln!("probe: may open {:?}", super::list_databases(conn, password).await);
                }
            }
        }
    });
}
