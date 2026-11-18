//! Users and privileges: the statements behind the users dialog, and the
//! catalog queries for server information. SQLite has neither users nor
//! a process list.

use crate::sql::{Dialect, ident, quoted};

/// A MariaDB account is a name and a host; a PostgreSQL role only a name.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub name: String,
    pub host: String,
}

impl Account {
    pub fn sql(&self, dialect: Dialect) -> String {
        match dialect {
            Dialect::MySql => format!("{}@{}", quoted(dialect, &self.name), quoted(dialect, if self.host.is_empty() { "%" } else { &self.host })),
            _ => ident(dialect, &self.name),
        }
    }
}

/// What a grant applies to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum On {
    /// Every table of the open database (schema in PostgreSQL).
    Database(String),
    Table(String),
}

pub const PRIVILEGES: [&str; 5] = ["SELECT", "INSERT", "UPDATE", "DELETE", "ALL PRIVILEGES"];

/// A statement and the same with the password hidden, for the review.
pub struct Secretive {
    pub sql: String,
    pub shown: String,
}

pub fn create_user(dialect: Dialect, account: &Account, password: &str) -> Secretive {
    let make = |p: &str| match dialect {
        Dialect::MySql => format!("CREATE USER {} IDENTIFIED BY {p}", account.sql(dialect)),
        _ => format!("CREATE ROLE {} WITH LOGIN PASSWORD {p}", account.sql(dialect)),
    };
    Secretive { sql: make(&quoted(dialect, password)), shown: make("'••••••'") }
}

pub fn set_password(dialect: Dialect, account: &Account, password: &str) -> Secretive {
    let make = |p: &str| match dialect {
        Dialect::MySql => format!("ALTER USER {} IDENTIFIED BY {p}", account.sql(dialect)),
        _ => format!("ALTER ROLE {} WITH PASSWORD {p}", account.sql(dialect)),
    };
    Secretive { sql: make(&quoted(dialect, password)), shown: make("'••••••'") }
}

pub fn drop_user(dialect: Dialect, account: &Account) -> String {
    match dialect {
        Dialect::MySql => format!("DROP USER {}", account.sql(dialect)),
        _ => format!("DROP ROLE {}", account.sql(dialect)),
    }
}

fn target(dialect: Dialect, on: &On) -> String {
    match (dialect, on) {
        (Dialect::MySql, On::Database(db)) => format!("{}.*", ident(dialect, db)),
        (_, On::Database(schema)) => format!("ALL TABLES IN SCHEMA {}", ident(dialect, schema)),
        (_, On::Table(t)) => ident(dialect, t),
    }
}

pub fn grant(dialect: Dialect, privileges: &[&str], on: &On, account: &Account) -> String {
    format!("GRANT {} ON {} TO {}", privileges.join(", "), target(dialect, on), account.sql(dialect))
}

pub fn revoke(dialect: Dialect, privileges: &[&str], on: &On, account: &Account) -> String {
    format!("REVOKE {} ON {} FROM {}", privileges.join(", "), target(dialect, on), account.sql(dialect))
}

/// Accounts: (name, host or attributes).
pub fn users_query(dialect: Dialect) -> Option<&'static str> {
    match dialect {
        Dialect::MySql => Some("SELECT User, Host FROM mysql.user ORDER BY User, Host"),
        Dialect::Postgres => Some(
            "SELECT rolname, CASE WHEN rolsuper THEN 'superuser' WHEN rolcanlogin THEN 'login' ELSE 'role' END \
             FROM pg_roles WHERE rolname NOT LIKE 'pg\\_%' ORDER BY rolname",
        ),
        Dialect::Sqlite => None,
    }
}

/// What one account may do, as one text line per row.
pub fn grants_query(dialect: Dialect, account: &Account) -> Option<String> {
    match dialect {
        Dialect::MySql => Some(format!("SHOW GRANTS FOR {}", account.sql(dialect))),
        Dialect::Postgres => Some(format!(
            "SELECT privilege_type || ' ON ' || table_name FROM information_schema.role_table_grants \
             WHERE grantee = {} AND table_schema = current_schema() ORDER BY table_name, privilege_type",
            quoted(dialect, &account.name)
        )),
        Dialect::Sqlite => None,
    }
}

/// Per table: name, data bytes, index bytes, rows.
pub fn sizes_query(dialect: Dialect) -> &'static str {
    match dialect {
        Dialect::MySql => {
            "SELECT TABLE_NAME, DATA_LENGTH, INDEX_LENGTH, TABLE_ROWS FROM information_schema.TABLES \
             WHERE TABLE_SCHEMA = DATABASE() ORDER BY DATA_LENGTH + INDEX_LENGTH DESC"
        }
        Dialect::Postgres => {
            "SELECT c.relname, pg_table_size(c.oid), pg_indexes_size(c.oid), c.reltuples::bigint FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = current_schema() AND c.relkind IN ('r', 'p', 'm') \
             ORDER BY pg_total_relation_size(c.oid) DESC"
        }
        Dialect::Sqlite => {
            "SELECT name, SUM(pgsize), 0, NULL FROM dbstat GROUP BY name ORDER BY SUM(pgsize) DESC"
        }
    }
}

pub fn processes_query(dialect: Dialect) -> Option<&'static str> {
    match dialect {
        Dialect::MySql => Some(
            "SELECT ID, USER, HOST, DB, COMMAND, TIME, STATE, LEFT(INFO, 200) FROM information_schema.PROCESSLIST ORDER BY TIME DESC",
        ),
        Dialect::Postgres => Some(
            "SELECT pid, usename, client_addr, datname, state, date_trunc('second', now() - query_start), left(query, 200) \
             FROM pg_stat_activity WHERE datname IS NOT NULL ORDER BY query_start",
        ),
        Dialect::Sqlite => None,
    }
}

/// Bytes as "12.3 MB".
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mariadb_accounts_and_grants() {
        let a = Account { name: "ann".into(), host: "localhost".into() };
        let c = create_user(Dialect::MySql, &a, "s'cret");
        assert_eq!(c.sql, "CREATE USER 'ann'@'localhost' IDENTIFIED BY 's''cret'");
        assert!(!c.shown.contains("cret"));
        assert_eq!(
            grant(Dialect::MySql, &["SELECT", "INSERT"], &On::Database("shop".into()), &a),
            "GRANT SELECT, INSERT ON `shop`.* TO 'ann'@'localhost'"
        );
        assert_eq!(revoke(Dialect::MySql, &["DELETE"], &On::Table("orders".into()), &a), "REVOKE DELETE ON `orders` FROM 'ann'@'localhost'");
    }

    #[test]
    fn postgres_roles() {
        let a = Account { name: "ann".into(), host: String::new() };
        assert_eq!(create_user(Dialect::Postgres, &a, "x").sql, "CREATE ROLE \"ann\" WITH LOGIN PASSWORD 'x'");
        assert_eq!(
            grant(Dialect::Postgres, &["SELECT"], &On::Database("public".into()), &a),
            "GRANT SELECT ON ALL TABLES IN SCHEMA \"public\" TO \"ann\""
        );
        assert_eq!(drop_user(Dialect::Postgres, &a), "DROP ROLE \"ann\"");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(12_345_678), "12.3 MB");
    }
}
