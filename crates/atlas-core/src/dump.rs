//! Reads the structure out of an SQL dump (mariadb-dump, mysqldump,
//! pg_dump, sqlite3 .dump, or a hand-written schema.sql), without a server.
//!
//! The file is cut into statements by a small scanner that knows strings,
//! comments, `DELIMITER` and `COPY … FROM stdin` blocks. Only `CREATE TABLE`
//! and `ALTER TABLE` statements are handed to sqlparser, so the INSERTs that
//! make up most of a dump are never parsed.

use sqlparser::ast::{
    AlterTableOperation, ColumnOption, CreateIndex, CreateTable, DataType, Expr, Ident, IndexColumn, ObjectName,
    ReferentialAction, Statement, TableConstraint,
};
use sqlparser::dialect::{GenericDialect, MySqlDialect, PostgreSqlDialect, SQLiteDialect};
use sqlparser::parser::Parser;

use crate::schema::{Check, Column, FkAction, Index, Relation, RelationKind, Schema, Table, TableKind, enum_values};

#[derive(Debug, Default, PartialEq)]
pub struct Report {
    /// Structure statements that no dialect could parse, with the first
    /// line of each, so the user learns what is missing.
    pub skipped: Vec<String>,
}

/// Cuts `text` into statements and returns the ones that shape tables.
pub fn statements(text: &str) -> Vec<String> {
    split(text).statements.into_iter().filter(|s| is_structure(&s.to_ascii_uppercase())).collect()
}

/// Every statement of a script, for running it. Comments go; pg_dump's
/// `COPY … FROM stdin` blocks cannot be sent as SQL and are counted
/// instead.
#[derive(Debug, Default, PartialEq)]
pub struct Split {
    pub statements: Vec<String>,
    pub copy_blocks: usize,
}

pub fn split(text: &str) -> Split {
    let mut copy_blocks = 0;
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
                copy_blocks += 1;
                continue;
            }
            if !statement.is_empty() {
                out.push(statement);
            }
            continue;
        }
        let ch = rest.chars().next().unwrap_or(' ');
        current.push(ch);
        i += ch.len_utf8();
    }
    let last = current.trim();
    if !last.is_empty() {
        out.push(last.to_owned());
    }
    Split { statements: out, copy_blocks }
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
        || starts(&["CREATE", "INDEX"])
        || starts(&["CREATE", "UNIQUE", "INDEX"])
        || starts(&["CREATE", "VIEW"])
        || starts(&["CREATE", "OR", "REPLACE", "VIEW"])
}

fn action(a: &Option<ReferentialAction>) -> Option<FkAction> {
    a.as_ref().map(|a| match a {
        ReferentialAction::Restrict => FkAction::Restrict,
        ReferentialAction::Cascade => FkAction::Cascade,
        ReferentialAction::SetNull => FkAction::SetNull,
        ReferentialAction::NoAction => FkAction::NoAction,
        ReferentialAction::SetDefault => FkAction::SetDefault,
    })
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
            Some(Statement::CreateIndex(index)) => create_index(&mut schema, &index),
            Some(Statement::CreateView(view)) => {
                let name = last_name(&view.name);
                schema.tables.retain(|t| t.name != name);
                schema.tables.push(Table {
                    name,
                    kind: TableKind::View,
                    definition: Some(view.query.to_string()),
                    ..Default::default()
                });
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
        let data_type = def.data_type.to_string();
        let mut column = Column {
            name: def.name.value.clone(),
            enum_values: enum_values(&data_type),
            auto_increment: matches!(data_type.to_ascii_lowercase().as_str(), "serial" | "bigserial" | "smallserial"),
            // Keep the spelling of enum values; lowercase the type keywords.
            data_type: if matches!(def.data_type, DataType::Enum(..) | DataType::Set(..)) {
                data_type
            } else {
                data_type.to_lowercase()
            },
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
                    on_delete: action(&fk.on_delete),
                    on_update: action(&fk.on_update),
                }),
                ColumnOption::Check(c) => table.checks.push(Check {
                    name: c.name.as_ref().map(|n| n.value.clone()),
                    expr: c.expr.to_string(),
                }),
                ColumnOption::Generated { generation_expr: Some(e), .. } => column.generated = Some(e.to_string()),
                ColumnOption::DialectSpecific(tokens)
                    if tokens.iter().any(|t| t.to_string().eq_ignore_ascii_case("AUTO_INCREMENT") || t.to_string().eq_ignore_ascii_case("AUTOINCREMENT")) =>
                {
                    column.auto_increment = true;
                }
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
        TableConstraint::Unique(u) => {
            let columns: Vec<String> = u.columns.iter().map(index_column).collect();
            let name = u.index_name.as_ref().or(u.name.as_ref()).map(|n| n.value.clone());
            match name {
                Some(name) => table.indexes.push(Index { name, columns, unique: true, kind: String::new() }),
                None => table.unique.push(columns),
            }
        }
        TableConstraint::Index(i) => table.indexes.push(Index {
            name: i.name.as_ref().map(|n| n.value.clone()).unwrap_or_default(),
            columns: i.columns.iter().map(index_column).collect(),
            unique: false,
            kind: i.index_type.as_ref().map(|t| t.to_string().to_lowercase()).unwrap_or_default(),
        }),
        TableConstraint::FulltextOrSpatial(i) => table.indexes.push(Index {
            name: i.opt_index_name.as_ref().map(|n| n.value.clone()).unwrap_or_default(),
            columns: i.columns.iter().map(index_column).collect(),
            unique: false,
            kind: if i.fulltext { "fulltext".into() } else { "spatial".into() },
        }),
        TableConstraint::Check(c) => {
            table.checks.push(Check { name: c.name.as_ref().map(|n| n.value.clone()), expr: c.expr.to_string() });
        }
        TableConstraint::ForeignKey(fk) => schema.relations.push(Relation {
            name: fk.name.as_ref().map(|n| n.value.clone()),
            from_table: table_name.to_owned(),
            from_columns: idents(&fk.columns),
            to_table: last_name(&fk.foreign_table),
            to_columns: idents(&fk.referred_columns),
            kind: RelationKind::Declared,
            on_delete: action(&fk.on_delete),
            on_update: action(&fk.on_update),
        }),
        _ => {}
    }
}

fn create_index(schema: &mut Schema, index: &CreateIndex) {
    let table_name = last_name(&index.table_name);
    let Some(table) = schema.tables.iter_mut().find(|t| t.name == table_name) else { return };
    table.indexes.push(Index {
        name: index.name.as_ref().map(last_name).unwrap_or_default(),
        columns: index.columns.iter().map(index_column).collect(),
        unique: index.unique,
        kind: index.using.as_ref().map(|t| t.to_string().to_lowercase()).unwrap_or_default(),
    });
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
        assert_eq!(s.relations[0].on_delete, Some(FkAction::Cascade));
        assert!(k.column("id").unwrap().auto_increment);
        let b = s.table("bestellungen").unwrap();
        assert_eq!(b.indexes.len(), 1);
        assert_eq!(b.indexes[0].name, "kunde_id");
        assert!(!b.indexes[0].unique);
    }

    #[test]
    fn indexes_views_checks_and_enums() {
        let (s, report) = parse(
            "x",
            "CREATE TABLE t (id int PRIMARY KEY, state enum('Neu','it''s') NOT NULL, n int CHECK (n > 0));\n\
             CREATE UNIQUE INDEX t_n ON t (n);\n\
             CREATE VIEW v AS SELECT id FROM t;",
        );
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let t = s.table("t").unwrap();
        assert_eq!(t.column("state").unwrap().enum_values.as_deref().unwrap(), ["Neu", "it's"]);
        assert_eq!(t.checks.len(), 1);
        assert_eq!(t.checks[0].expr, "n > 0");
        assert_eq!(t.indexes, [Index { name: "t_n".into(), columns: vec!["n".into()], unique: true, kind: String::new() }]);
        let v = s.table("v").unwrap();
        assert_eq!(v.kind, TableKind::View);
        assert_eq!(v.definition.as_deref(), Some("SELECT id FROM t"));
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
    fn split_keeps_data_and_trigger_bodies() {
        let s = split(MARIADB);
        assert_eq!(s.copy_blocks, 0);
        assert!(s.statements.iter().any(|x| x.starts_with("INSERT INTO `kunden`")));
        assert!(s.statements.iter().any(|x| x.starts_with("CREATE TRIGGER") && x.contains("SET NEW.id = 1;")));
        assert_eq!(split(PG).copy_blocks, 1);
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
