//! The SQL console's first line of defence: only statements that read.
//!
//! The second line is the server: every session Atlas opens is read-only
//! (MariaDB `SET SESSION TRANSACTION READ ONLY`, PostgreSQL
//! `default_transaction_read_only`, SQLite `mode=ro`). This check exists so
//! that a write is refused with a clear sentence before it is sent, and so
//! that a statement which would switch the session back to read-write
//! (`SET SESSION TRANSACTION READ WRITE`) never gets through.

use sqlparser::ast::Statement;
use sqlparser::dialect::{MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

use crate::sql::Dialect;

#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    Empty,
    /// More than one statement; Atlas runs one at a time.
    Several,
    /// The parser did not understand it, so Atlas cannot tell it only reads.
    Unparsed(String),
    /// It would change something.
    Writes(String),
}

/// Ok when `sql` is a single statement that only reads.
pub fn check(dialect: Dialect, sql: &str) -> Result<(), Refusal> {
    // sqlparser knows `PRAGMA name = value` but not `PRAGMA name(arg)`.
    // Without `=` a pragma only reads; the file is open read-only anyway.
    let upper = sql.trim().to_ascii_uppercase();
    if dialect == Dialect::Sqlite && upper.starts_with("PRAGMA ") && !upper.contains('=') && !upper.trim_end_matches(';').contains(';') {
        return Ok(());
    }
    let parsed = match dialect {
        Dialect::MySql => Parser::parse_sql(&MySqlDialect {}, sql),
        Dialect::Postgres => Parser::parse_sql(&PostgreSqlDialect {}, sql),
        Dialect::Sqlite => Parser::parse_sql(&SQLiteDialect {}, sql),
    };
    let statements = match parsed {
        Ok(statements) => statements,
        Err(e) => return fallback(sql).map_err(|()| Refusal::Unparsed(e.to_string())),
    };
    match statements.as_slice() {
        [] => Err(Refusal::Empty),
        [one] => reads(one),
        _ => Err(Refusal::Several),
    }
}

/// For SQL the parser does not know (vendor syntax): a single statement that
/// starts with a reading keyword and says nothing about INTO is let through.
/// The read-only session on the server still stands behind it.
fn fallback(sql: &str) -> Result<(), ()> {
    let upper = sql.trim().trim_end_matches(';').to_ascii_uppercase();
    let first = upper.split_whitespace().next().unwrap_or("");
    let reading = matches!(first, "SELECT" | "SHOW" | "DESCRIBE" | "DESC" | "EXPLAIN" | "WITH" | "VALUES");
    let words: Vec<&str> = upper.split(|c: char| !c.is_alphanumeric() && c != '_').collect();
    let suspicious = ["INTO", "OUTFILE", "DUMPFILE", "ANALYZE", "UPDATE", "DELETE", "INSERT"];
    if reading && !upper.contains(';') && !words.iter().any(|w| suspicious.contains(w)) { Ok(()) } else { Err(()) }
}

fn reads(statement: &Statement) -> Result<(), Refusal> {
    let kind = format!("{statement:?}");
    let name = kind.split(['(', ' ', '{']).next().unwrap_or("").to_owned();
    match statement {
        Statement::Query(_) => {
            // SELECT … INTO OUTFILE writes a file on the server; INTO a table
            // creates one.
            if kind.contains("into: Some(") {
                Err(Refusal::Writes("SELECT … INTO".into()))
            } else {
                Ok(())
            }
        }
        Statement::Explain { statement, analyze, .. } => {
            // EXPLAIN ANALYZE runs the statement.
            if *analyze { reads(statement) } else { Ok(()) }
        }
        Statement::ExplainTable { .. } => Ok(()),
        Statement::Pragma { value: None, .. } => Ok(()),
        _ if name.starts_with("Show") => Ok(()),
        _ => Err(Refusal::Writes(name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_passes() {
        for sql in [
            "SELECT * FROM t WHERE a = 1",
            "WITH x AS (SELECT 1) SELECT * FROM x",
            "SHOW TABLES",
            "EXPLAIN SELECT 1",
            "DESCRIBE t",
        ] {
            assert_eq!(check(Dialect::MySql, sql), Ok(()), "{sql}");
        }
        assert_eq!(check(Dialect::Sqlite, "PRAGMA table_info(t)"), Ok(()));
    }

    #[test]
    fn writing_is_refused() {
        for sql in [
            "DELETE FROM t",
            "UPDATE t SET a = 1",
            "DROP TABLE t",
            "SET SESSION TRANSACTION READ WRITE",
            "SELECT * INTO OUTFILE '/tmp/x' FROM t",
            "EXPLAIN ANALYZE DELETE FROM t",
        ] {
            assert!(check(Dialect::MySql, sql).is_err(), "{sql}");
        }
        assert!(check(Dialect::Sqlite, "PRAGMA foreign_keys = OFF").is_err());
    }

    #[test]
    fn one_statement_at_a_time() {
        assert_eq!(check(Dialect::Postgres, "SELECT 1; SELECT 2"), Err(Refusal::Several));
        assert_eq!(check(Dialect::Postgres, "  "), Err(Refusal::Empty));
    }
}
