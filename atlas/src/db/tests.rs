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

use atlas_core::edit::Changes;
use atlas_core::migrate::{self, TableEdit};
use atlas_core::schema::{FkAction, Index};

use super::{Access, Progress, Session, Target, connect, connect_with};

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap()
}

async fn demo_sqlite() -> std::path::PathBuf {
    // Tests run in parallel; each gets its own file.
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("atlas-test-{}-{n}.sqlite", std::process::id()));
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
            writable: false,
            keyring: false,
        },
        std::env::var(password_var).unwrap_or_default(),
    ))
}

/// The write path, against a throwaway database only: a scratch table is
/// created, edited through the same code the grid and the table editor
/// use, read back, and dropped. The demo tables are not touched.
async fn writes(session: &Session) {
    let d = session.dialect;
    let q = |n: &str| sql::ident(d, n);
    let _ = session.execute(format!("DROP TABLE IF EXISTS {}", q("atlas_scratch"))).await;
    let id_type = if d == Dialect::Sqlite { "INTEGER" } else { "int" };
    session
        .execute_all(
            vec![format!(
                "CREATE TABLE {} ({} {id_type} NOT NULL PRIMARY KEY, {} varchar(40), {} int)",
                q("atlas_scratch"),
                q("id"),
                q("name"),
                q("customer_id")
            )],
            false,
            Progress::default(),
        )
        .await
        .unwrap();

    // Grid edits: insert two rows, change one, all in one transaction.
    let mut c = Changes::new(vec!["id".into()]);
    for (id, name) in [("1", "Anna"), ("2", "it's")] {
        let mut row = atlas_core::edit::Values::new();
        row.insert("id".into(), Some(id.into()));
        row.insert("name".into(), Some(name.into()));
        row.insert("customer_id".into(), Some("1".into()));
        c.insert(row);
    }
    let n = session.execute_all(c.to_sql(d, "atlas_scratch"), false, Progress::default()).await.unwrap();
    assert_eq!(n, 2);
    let mut c = Changes::new(vec!["id".into()]);
    c.set(vec![Some("2".into())], "name", Some("Ben".into()), Some("it's"));
    session.execute_all(c.to_sql(d, "atlas_scratch"), false, Progress::default()).await.unwrap();
    let name = session.scalar(format!("SELECT {} FROM {} WHERE {} = 2", q("name"), q("atlas_scratch"), q("id"))).await.unwrap();
    assert_eq!(name.as_deref(), Some("Ben"));

    // A failing batch leaves nothing behind.
    let bad = vec![
        format!("UPDATE {} SET {} = 'gone' WHERE {} = 1", q("atlas_scratch"), q("name"), q("id")),
        format!("INSERT INTO {} ({}) VALUES (1)", q("atlas_scratch"), q("id")),
    ];
    let err = session.execute_all(bad, false, Progress::default()).await.unwrap_err();
    assert!(err.starts_with("Statement 2:"), "{err}");
    let name = session.scalar(format!("SELECT {} FROM {} WHERE {} = 1", q("name"), q("atlas_scratch"), q("id"))).await.unwrap();
    assert_eq!(name.as_deref(), Some("Anna"), "rolled back");

    // The table editor: a type change, an index and a foreign key with an
    // action, then read back from the catalog.
    let schema = session.schema().await.unwrap();
    let old = schema.table("atlas_scratch").unwrap().clone();
    let mut new = old.clone();
    let name_col = new.columns.iter_mut().find(|c| c.name == "name").unwrap();
    name_col.data_type = if d == Dialect::Postgres { "character varying(80)".into() } else { "varchar(80)".into() };
    new.indexes.push(Index { name: "atlas_scratch_name".into(), columns: vec!["name".into()], unique: false, kind: String::new() });
    let fk = atlas_core::schema::Relation {
        name: Some("atlas_scratch_customer".into()),
        from_table: "atlas_scratch".into(),
        from_columns: vec!["customer_id".into()],
        to_table: "customers".into(),
        to_columns: vec!["id".into()],
        on_delete: Some(FkAction::Cascade),
        ..Default::default()
    };
    let m = migrate::alter_table(
        d,
        &TableEdit { old: &old, new: &new, renamed_columns: &[], old_fks: &[], new_fks: std::slice::from_ref(&fk) },
    );
    session.execute_all(m.statements.clone(), m.foreign_keys_off, Progress::default()).await.unwrap_or_else(|e| panic!("{e}\n{}", m.text()));
    let schema = session.schema().await.unwrap();
    let t = schema.table("atlas_scratch").unwrap();
    assert!(t.indexes.iter().any(|i| i.name == "atlas_scratch_name"), "{:?}", t.indexes);
    assert!(t.column("name").unwrap().data_type.contains("80"), "{:?}", t.column("name"));
    let r = schema.relations.iter().find(|r| r.from_table == "atlas_scratch" && r.kind == RelationKind::Declared).unwrap();
    assert_eq!(r.on_delete, Some(FkAction::Cascade));
    assert_eq!(session.scalar(format!("SELECT COUNT(*) FROM {}", q("atlas_scratch"))).await.unwrap().as_deref(), Some("2"));

    // Backup of the scratch table, dropped, restored.
    let mut only = schema.clone();
    only.tables.retain(|t| t.name == "atlas_scratch");
    only.relations.clear();
    let mut text = atlas_core::backup::header(d, &only, "test");
    text.push_str(&atlas_core::backup::structure(d, &only));
    let rows = session.query(format!("SELECT * FROM {}", q("atlas_scratch")), 100).await.unwrap();
    text.push_str(&atlas_core::backup::rows(d, only.table("atlas_scratch").unwrap(), &rows.columns, &rows.rows));
    text.push_str(&atlas_core::backup::footer(d, &only));
    session.execute(format!("DROP TABLE {}", q("atlas_scratch"))).await.unwrap();
    let split = atlas_core::dump::split(&text);
    session.execute_all(split.statements, false, Progress::default()).await.unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert_eq!(session.scalar(format!("SELECT COUNT(*) FROM {}", q("atlas_scratch"))).await.unwrap().as_deref(), Some("2"));
    let back = session.schema().await.unwrap();
    assert!(back.table("atlas_scratch").unwrap().indexes.iter().any(|i| i.name == "atlas_scratch_name"));

    session.execute(format!("DROP TABLE {}", q("atlas_scratch"))).await.unwrap();
}

#[test]
fn sqlite_writes() {
    rt().block_on(async {
        let path = demo_sqlite().await;
        let ro = connect(Target::File(path.clone()), String::new()).await.unwrap();
        assert!(ro.execute("DELETE FROM customers WHERE id = 1".into()).await.is_err(), "read-only refuses");
        ro.close().await;
        let session = connect_with(Target::File(path.clone()), String::new(), Access::ReadWrite).await.unwrap();
        writes(&session).await;
        // A rebuild with foreign keys off keeps the children of a parent.
        let schema = session.schema().await.unwrap();
        let old = schema.table("customers").unwrap().clone();
        let mut new = old.clone();
        new.columns.iter_mut().find(|c| c.name == "phone").unwrap().nullable = false;
        session.execute("UPDATE customers SET phone = '-' WHERE phone IS NULL".into()).await.unwrap();
        let m = migrate::alter_table(
            Dialect::Sqlite,
            &TableEdit { old: &old, new: &new, renamed_columns: &[], old_fks: &[], new_fks: &[] },
        );
        assert!(m.foreign_keys_off);
        session.execute_all(m.statements, true, Progress::default()).await.unwrap();
        assert_eq!(session.scalar("SELECT COUNT(*) FROM orders".into()).await.unwrap().as_deref(), Some("180"));
        assert!(!session.schema().await.unwrap().table("customers").unwrap().column("phone").unwrap().nullable);
        session.close().await;
        let _ = std::fs::remove_file(path);
    });
}

#[test]
fn sqlite_catalog_has_checks_indexes_and_triggers() {
    rt().block_on(async {
        let path = demo_sqlite().await;
        let session = connect_with(Target::File(path.clone()), String::new(), Access::ReadWrite).await.unwrap();
        session
            .execute_all(
                vec![
                    "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT, n int CHECK (n > 0), p int REFERENCES customers(id) ON DELETE SET NULL)".into(),
                    "CREATE INDEX t_n ON t (n)".into(),
                    "CREATE TRIGGER t_ins AFTER INSERT ON t BEGIN SELECT 1; END".into(),
                    "CREATE VIEW v AS SELECT id FROM t".into(),
                ],
                false,
                Progress::default(),
            )
            .await
            .unwrap();
        let s = session.schema().await.unwrap();
        let t = s.table("t").unwrap();
        assert_eq!(t.checks.len(), 1);
        assert_eq!(t.indexes[0].name, "t_n");
        assert!(t.column("id").unwrap().auto_increment);
        assert_eq!(t.triggers[0].timing, "AFTER");
        assert_eq!(t.triggers[0].event, "INSERT");
        assert_eq!(s.relations.iter().find(|r| r.from_table == "t").unwrap().on_delete, Some(FkAction::SetNull));
        assert_eq!(s.table("v").unwrap().definition.as_deref(), Some("SELECT id FROM t"));
        session.close().await;
        let _ = std::fs::remove_file(path);
    });
}

#[test]
fn mariadb_writes() {
    let Some((conn, password)) = server("ATLAS_TEST_MARIADB", "ATLAS_TEST_PASSWORD", Dialect::MySql) else {
        eprintln!("ATLAS_TEST_MARIADB not set; skipped");
        return;
    };
    rt().block_on(async {
        let session = connect_with(Target::Server(conn), password, Access::ReadWrite).await.unwrap();
        writes(&session).await;
        session.close().await;
    });
}

#[test]
fn postgres_writes() {
    let Some((conn, password)) = server("ATLAS_TEST_POSTGRES", "ATLAS_TEST_PG_PASSWORD", Dialect::Postgres) else {
        eprintln!("ATLAS_TEST_POSTGRES not set; skipped");
        return;
    };
    rt().block_on(async {
        let session = connect_with(Target::Server(conn), password, Access::ReadWrite).await.unwrap();
        writes(&session).await;
        session.close().await;
    });
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
