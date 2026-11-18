//! Reads the structure out of an SQL dump (mariadb-dump, mysqldump,
//! pg_dump, sqlite3 .dump, or a hand-written schema.sql), without a server.
//!
//! The file is cut into statements by a small scanner that knows strings,
//! comments, `DELIMITER` and `COPY … FROM stdin` blocks. Only `CREATE TABLE`
//! and `ALTER TABLE` statements are handed to sqlparser, so the INSERTs that
//! make up most of a dump are never parsed.

use sqlparser::ast::{
    AlterTableOperation, ColumnOption, CreateTable, Expr, Ident, IndexColumn, ObjectName, Statement, TableConstraint,
};
use sqlparser::dialect::{GenericDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

use crate::schema::{Column, Relation, RelationKind, Schema, Table};

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    /// Structure statements that no dialect could parse, with the first
    /// line of each, so the user learns what is missing.
    pub skipped: Vec<String>,
}

/// Cuts `text` into statements and returns the ones that shape tables.
pub fn statements(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut current = String::new();
    let mut delimiter = ";".to_owned();
    let mut i = 0;
    let at_line_start = |cur: &str| cur.trim().is_empty();

    while i < bytes.len() {
        let rest = &text[i..];
        let c = bytes[i];
        // Comments.
        if rest.starts_with("--") || (c == b'#' && at_line_start(&current)) {
            i += rest.find('\n').map_or(rest.len(), |n| n + 1);
            continue;
        }
        if rest.starts_with("/*") {
            i += rest.find("*/").map_or(rest.len(), |n| n + 2);
            continue;
        }
        // DELIMITER $$ (mysql client command, own line).
        if at_line_start(&current) && rest.len() > 10 && rest[..10].eq_ignore_ascii_case("DELIMITER ") {
            let line_end = rest.find('\n').unwrap_or(rest.len());
            delimiter = rest[10..line_end].trim().to_owned();
            if delimiter.is_empty() {
                delimiter = ";".into();
            }
            current.clear();
            i += line_end;
            continue;
        }
        // Quoted text and identifiers, copied whole.
        if c == b'\'' || c == b'"' || c == b'`' {
            let mut j = i + 1;
            while j < bytes.len() {
                if bytes[j] == b'\\' && c == b'\'' {
                    j += 2;
                    continue;
                }
                if bytes[j] == c {
                    if j + 1 < bytes.len() && bytes[j + 1] == c {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            let end = (j + 1).min(bytes.len());
            current.push_str(&text[i..end]);
            i = end;
            continue;
        }
        // PostgreSQL dollar quoting: $$ … $$ or $tag$ … $tag$.
        if c == b'$'
            && let Some(close) = rest[1..].find('$')
            && rest[1..close + 1].chars().all(|ch| ch.is_alphanumeric() || ch == '_')
            && let Some(end) = rest[close + 2..].find(&rest[..close + 2])
        {
            let total = 2 * (close + 2) + end;
            current.push_str(&rest[..total]);
            i += total;
            continue;
        }
        if rest.starts_with(delimiter.as_str()) {
            let statement = current.trim().to_owned();
            current.clear();
            i += delimiter.len();
            // pg_dump data: COPY t (…) FROM stdin; then rows until "\.".
            let upper = statement.to_ascii_uppercase();
            if upper.starts_with("COPY ") && upper.ends_with("FROM STDIN") {
                let after = &text[i..];
                i += after.find("\n\\.").map_or(after.len(), |n| n + 3);
                continue;
            }
            if is_structure(&upper) {
                out.push(statement);
            }
            continue;
        }
        let ch = rest.chars().next().unwrap_or(' ');
        current.push(ch);
        i += ch.len_utf8();
    }
    let last = current.trim();
    if is_structure(&last.to_ascii_uppercase()) {
        out.push(last.to_owned());
    }
    out
}

fn is_structure(upper: &str) -> bool {
    let words: Vec<&str> = upper.split_whitespace().take(6).collect();
    let starts = |w: &[&str]| words.len() >= w.len() && words[..w.len()] == *w;
    (starts(&["CREATE", "TABLE"])
        || starts(&["CREATE", "TEMPORARY", "TABLE"])
        || starts(&["CREATE", "UNLOGGED", "TABLE"])
        || starts(&["CREATE", "OR", "REPLACE", "TABLE"])
        || starts(&["ALTER", "TABLE"]))
        && !upper.contains(" AS SELECT ")
}

fn parse_one(sql: &str) -> Option<Statement> {
    let attempts: [&dyn sqlparser::dialect::Dialect; 4] =
        [&MySqlDialect {}, &PostgreSqlDialect {}, &SQLiteDialect {}, &GenericDialect {}];
    for dialect in attempts {
        if let Ok(mut parsed) = Parser::parse_sql(dialect, sql)
            && parsed.len() == 1
        {
            return parsed.pop();
        }
    }
    None
}

fn last_name(name: &ObjectName) -> String {
    name.0
        .last()
        .and_then(|p| p.as_ident())
        .map(|i| i.value.clone())
        .unwrap_or_else(|| name.to_string())
}

fn index_column(c: &IndexColumn) -> String {
    match &c.column.expr {
        Expr::Identifier(Ident { value, .. }) => value.clone(),
        other => other.to_string(),
    }
}

fn idents(list: &[Ident]) -> Vec<String> {
    list.iter().map(|i| i.value.clone()).collect()
}

/// Reads the structure of a dump. `name` becomes the schema's name.
pub fn parse(name: &str, text: &str) -> (Schema, Report) {
    let mut schema = Schema { name: name.to_owned(), ..Default::default() };
    let mut report = Report::default();

    for sql in statements(text) {
        match parse_one(&sql) {
            Some(Statement::CreateTable(create)) => create_table(&mut schema, &create),
            Some(Statement::AlterTable(alter)) => {
                let table = last_name(&alter.name);
                for op in &alter.operations {
                    if let AlterTableOperation::AddConstraint { constraint, .. } = op {
                        apply_constraint(&mut schema, &table, constraint);
                    }
                }
            }
            Some(_) => {}
            None => report.skipped.push(sql.lines().next().unwrap_or("").chars().take(120).collect()),
        }
    }

    // `REFERENCES parent` without columns means the parent's primary key.
    let tables = schema.tables.clone();
    for r in &mut schema.relations {
        if r.to_columns.is_empty()
            && let Some(parent) = tables.iter().find(|t| t.name == r.to_table)
        {
            r.to_columns = parent.primary_key().into_iter().map(str::to_owned).collect();
        }
    }
    // Drop relations to tables the dump does not contain.
    let names: Vec<String> = schema.tables.iter().map(|t| t.name.clone()).collect();
    schema.relations.retain(|r| names.contains(&r.to_table) && names.contains(&r.from_table));
    (schema, report)
}

fn create_table(schema: &mut Schema, create: &CreateTable) {
    let name = last_name(&create.name);
    let mut table = Table { name: name.clone(), ..Default::default() };
    for def in &create.columns {
        let mut column = Column {
            name: def.name.value.clone(),
            data_type: def.data_type.to_string().to_lowercase(),
            nullable: true,
            ..Default::default()
        };
        for option in &def.options {
            match &option.option {
                ColumnOption::NotNull => column.nullable = false,
                ColumnOption::Null => column.nullable = true,
                ColumnOption::Default(e) => column.default = Some(e.to_string()),
                ColumnOption::Comment(c) => column.comment = Some(c.clone()),
                ColumnOption::PrimaryKey(_) => {
                    column.primary = true;
                    column.nullable = false;
                }
                ColumnOption::Unique(_) => table.unique.push(vec![column.name.clone()]),
                ColumnOption::ForeignKey(fk) => schema.relations.push(Relation {
                    name: fk.name.as_ref().map(|n| n.value.clone()),
                    from_table: name.clone(),
                    from_columns: vec![column.name.clone()],
                    to_table: last_name(&fk.foreign_table),
                    to_columns: idents(&fk.referred_columns),
                    kind: RelationKind::Declared,
                }),
                _ => {}
            }
        }
        table.columns.push(column);
    }
    schema.tables.retain(|t| t.name != name);
    schema.tables.push(table);
    for constraint in &create.constraints {
        apply_constraint(schema, &name, constraint);
    }
}

fn apply_constraint(schema: &mut Schema, table_name: &str, constraint: &TableConstraint) {
    let Some(table) = schema.tables.iter_mut().find(|t| t.name == table_name) else { return };
    match constraint {
        TableConstraint::PrimaryKey(pk) => {
            for c in pk.columns.iter().map(index_column) {
                if let Some(col) = table.columns.iter_mut().find(|x| x.name == c) {
                    col.primary = true;
                    col.nullable = false;
                }
            }
        }
        TableConstraint::Unique(u) => table.unique.push(u.columns.iter().map(index_column).collect()),
        TableConstraint::ForeignKey(fk) => schema.relations.push(Relation {
            name: fk.name.as_ref().map(|n| n.value.clone()),
            from_table: table_name.to_owned(),
            from_columns: idents(&fk.columns),
            to_table: last_name(&fk.foreign_table),
            to_columns: idents(&fk.referred_columns),
            kind: RelationKind::Declared,
        }),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MARIADB: &str = r#"
-- MariaDB dump 10.19
/*!40101 SET NAMES utf8mb4 */;
DROP TABLE IF EXISTS `kunden`;
CREATE TABLE `kunden` (
  `id` int(10) unsigned NOT NULL AUTO_INCREMENT,
  `name` varchar(80) NOT NULL DEFAULT '' COMMENT 'Voller Name; mit Semikolon',
  `email` varchar(120) DEFAULT NULL,
  PRIMARY KEY (`id`),
  UNIQUE KEY `email` (`email`)
) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4;
INSERT INTO `kunden` VALUES (1,'Anna; Bauer','a@b.at');
CREATE TABLE `bestellungen` (
  `id` int(10) unsigned NOT NULL AUTO_INCREMENT,
  `kunde_id` int(10) unsigned NOT NULL,
  PRIMARY KEY (`id`),
  KEY `kunde_id` (`kunde_id`),
  CONSTRAINT `fk_kunde` FOREIGN KEY (`kunde_id`) REFERENCES `kunden` (`id`) ON DELETE CASCADE
) ENGINE=InnoDB;
DELIMITER ;;
CREATE TRIGGER t BEFORE INSERT ON bestellungen FOR EACH ROW BEGIN SET NEW.id = 1; END ;;
DELIMITER ;
"#;

    const PG: &str = r#"
SET search_path = public;
CREATE TABLE public.authors (id integer NOT NULL, name text);
CREATE TABLE public.books (id integer NOT NULL, author_id integer);
COPY public.books (id, author_id) FROM stdin;
1	1
2	\N
\.
CREATE FUNCTION f() RETURNS trigger AS $$ BEGIN; RETURN NULL; END $$ LANGUAGE plpgsql;
ALTER TABLE ONLY public.authors ADD CONSTRAINT authors_pkey PRIMARY KEY (id);
ALTER TABLE ONLY public.books ADD CONSTRAINT books_pkey PRIMARY KEY (id);
ALTER TABLE ONLY public.books ADD CONSTRAINT books_author_fkey FOREIGN KEY (author_id) REFERENCES public.authors(id);
"#;

    #[test]
    fn mariadb_dump() {
        let (s, report) = parse("shop", MARIADB);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(s.tables.len(), 2);
        let k = s.table("kunden").unwrap();
        assert_eq!(k.primary_key(), ["id"]);
        assert!(k.is_unique(&["email".into()]));
        assert_eq!(k.column("name").unwrap().comment.as_deref(), Some("Voller Name; mit Semikolon"));
        assert!(!k.column("name").unwrap().nullable);
        assert_eq!(s.relations.len(), 1);
        assert_eq!(s.relations[0].from_columns, ["kunde_id"]);
        assert_eq!(s.relations[0].to_table, "kunden");
    }

    #[test]
    fn pg_dump_with_copy_and_alter() {
        let (s, report) = parse("lib", PG);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        assert_eq!(s.tables.len(), 2);
        assert_eq!(s.table("books").unwrap().primary_key(), ["id"]);
        assert_eq!(s.relations.len(), 1);
        assert_eq!(s.relations[0].to_table, "authors");
    }

    #[test]
    fn sqlite_inline_reference_takes_parent_key() {
        let (s, _) = parse(
            "x",
            "CREATE TABLE a (id INTEGER PRIMARY KEY);\nCREATE TABLE b (id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a);",
        );
        assert_eq!(s.relations.len(), 1);
        assert_eq!(s.relations[0].to_columns, ["id"]);
    }
}
