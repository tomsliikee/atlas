//! SQL that Atlas writes itself: quoting, the paged and filtered query
//! behind the data browser, and copying rows as CSV or INSERT statements.
//!
//! Values typed into a filter never reach the server as raw text. MariaDB
//! gets them as a hex literal converted to the connection's character set,
//! PostgreSQL and SQLite as a standard string with quotes doubled (Atlas
//! turns `standard_conforming_strings` on for every PostgreSQL session, so
//! a backslash is just a backslash).

use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Dialect {
    MySql,
    Postgres,
    Sqlite,
}

pub fn ident(dialect: Dialect, name: &str) -> String {
    match dialect {
        Dialect::MySql => format!("`{}`", name.replace('`', "``")),
        Dialect::Postgres | Dialect::Sqlite => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

pub fn literal(dialect: Dialect, value: &str) -> String {
    match dialect {
        Dialect::MySql => {
            if value.is_empty() {
                return "''".into();
            }
            let mut hex = String::with_capacity(value.len() * 2 + 40);
            hex.push_str("CONVERT(X'");
            for b in value.as_bytes() {
                let _ = write!(hex, "{b:02x}");
            }
            hex.push_str("' USING utf8mb4)");
            hex
        }
        Dialect::Postgres | Dialect::Sqlite => format!("'{}'", value.replace('\'', "''")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Contains,
    Equals,
    NotEquals,
    Less,
    Greater,
    IsNull,
    IsNotNull,
}

impl Op {
    pub const ALL: [Op; 7] = [Op::Contains, Op::Equals, Op::NotEquals, Op::Less, Op::Greater, Op::IsNull, Op::IsNotNull];

    pub fn takes_value(self) -> bool {
        !matches!(self, Op::IsNull | Op::IsNotNull)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Filter {
    pub column: String,
    pub op: Op,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sort {
    pub column: String,
    pub descending: bool,
}

/// The column as text, for `contains` on numbers and dates.
fn as_text(dialect: Dialect, column: &str) -> String {
    let c = ident(dialect, column);
    match dialect {
        Dialect::MySql => format!("CAST({c} AS CHAR)"),
        Dialect::Postgres => format!("CAST({c} AS TEXT)"),
        Dialect::Sqlite => format!("CAST({c} AS TEXT)"),
    }
}

fn condition(dialect: Dialect, f: &Filter) -> String {
    let c = ident(dialect, &f.column);
    match f.op {
        Op::Contains => {
            let escaped = f.value.replace('!', "!!").replace('%', "!%").replace('_', "!_");
            let like = if dialect == Dialect::Postgres { "ILIKE" } else { "LIKE" };
            format!(
                "{} {like} {} ESCAPE '!'",
                as_text(dialect, &f.column),
                literal(dialect, &format!("%{escaped}%"))
            )
        }
        Op::Equals => format!("{c} = {}", literal(dialect, &f.value)),
        Op::NotEquals => format!("{c} <> {}", literal(dialect, &f.value)),
        Op::Less => format!("{c} < {}", literal(dialect, &f.value)),
        Op::Greater => format!("{c} > {}", literal(dialect, &f.value)),
        Op::IsNull => format!("{c} IS NULL"),
        Op::IsNotNull => format!("{c} IS NOT NULL"),
    }
}

pub fn where_clause(dialect: Dialect, filters: &[Filter]) -> String {
    if filters.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = filters.iter().map(|f| condition(dialect, f)).collect();
    format!(" WHERE {}", parts.join(" AND "))
}

/// One page of `table`.
pub fn page(dialect: Dialect, table: &str, filters: &[Filter], sort: Option<&Sort>, limit: u64, offset: u64) -> String {
    let mut q = format!("SELECT * FROM {}{}", ident(dialect, table), where_clause(dialect, filters));
    if let Some(s) = sort {
        let _ = write!(q, " ORDER BY {} {}", ident(dialect, &s.column), if s.descending { "DESC" } else { "ASC" });
    }
    let _ = write!(q, " LIMIT {limit} OFFSET {offset}");
    q
}

pub fn count(dialect: Dialect, table: &str, filters: &[Filter]) -> String {
    format!("SELECT COUNT(*) FROM {}{}", ident(dialect, table), where_clause(dialect, filters))
}

/// RFC 4180 CSV with a header line.
pub fn csv(columns: &[String], rows: &[Vec<Option<String>>]) -> String {
    fn field(out: &mut String, v: &str) {
        if v.contains([',', '"', '\n', '\r']) {
            out.push('"');
            out.push_str(&v.replace('"', "\"\""));
            out.push('"');
        } else {
            out.push_str(v);
        }
    }
    let mut out = String::new();
    for (i, c) in columns.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        field(&mut out, c);
    }
    out.push_str("\r\n");
    for row in rows {
        for (i, v) in row.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            if let Some(v) = v {
                field(&mut out, v);
            }
        }
        out.push_str("\r\n");
    }
    out
}

/// One INSERT per row. Values are written as strings (or NULL); every
/// database Atlas reads converts them back on insert.
pub fn inserts(dialect: Dialect, table: &str, columns: &[String], rows: &[Vec<Option<String>>]) -> String {
    let cols: Vec<String> = columns.iter().map(|c| ident(dialect, c)).collect();
    let head = format!("INSERT INTO {} ({}) VALUES (", ident(dialect, table), cols.join(", "));
    let mut out = String::new();
    for row in rows {
        out.push_str(&head);
        for (i, v) in row.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            match v {
                // Plain quoting here, not the hex form: the text is meant to
                // be read and pasted by a person.
                Some(v) => {
                    let quoted = if dialect == Dialect::MySql {
                        format!("'{}'", v.replace('\\', "\\\\").replace('\'', "''"))
                    } else {
                        format!("'{}'", v.replace('\'', "''"))
                    };
                    out.push_str(&quoted);
                }
                None => out.push_str("NULL"),
            }
        }
        out.push_str(");\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_double_their_quote() {
        assert_eq!(ident(Dialect::MySql, "a`b"), "`a``b`");
        assert_eq!(ident(Dialect::Postgres, "a\"b"), "\"a\"\"b\"");
    }

    #[test]
    fn mariadb_values_go_as_hex() {
        assert_eq!(literal(Dialect::MySql, "a'"), "CONVERT(X'6127' USING utf8mb4)");
        assert_eq!(literal(Dialect::Postgres, "a'\\"), "'a''\\'");
    }

    #[test]
    fn contains_escapes_wildcards() {
        let f = Filter { column: "name".into(), op: Op::Contains, value: "50%_!".into() };
        assert_eq!(
            where_clause(Dialect::Sqlite, &[f]),
            " WHERE CAST(\"name\" AS TEXT) LIKE '%50!%!_!!%' ESCAPE '!'"
        );
    }

    #[test]
    fn page_query() {
        let f = Filter { column: "id".into(), op: Op::IsNotNull, value: String::new() };
        let s = Sort { column: "id".into(), descending: true };
        assert_eq!(
            page(Dialect::Postgres, "t", &[f], Some(&s), 200, 400),
            "SELECT * FROM \"t\" WHERE \"id\" IS NOT NULL ORDER BY \"id\" DESC LIMIT 200 OFFSET 400"
        );
    }

    #[test]
    fn csv_quotes_only_when_needed() {
        let out = csv(&["a".into(), "b".into()], &[vec![Some("x,y".into()), None], vec![Some("say \"hi\"".into()), Some("1".into())]]);
        assert_eq!(out, "a,b\r\n\"x,y\",\r\n\"say \"\"hi\"\"\",1\r\n");
    }

    #[test]
    fn inserts_keep_nulls() {
        let out = inserts(Dialect::MySql, "t", &["a".into(), "b".into()], &[vec![Some("it's".into()), None]]);
        assert_eq!(out, "INSERT INTO `t` (`a`, `b`) VALUES ('it''s', NULL);\n");
    }
}
