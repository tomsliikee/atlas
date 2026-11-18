//! From one shape of a table to another: the ALTER statements per dialect.
//!
//! Renames cannot be seen by comparing two tables, so the table editor
//! passes them along. SQLite can rename, add and drop columns but change
//! little else, so any other change rebuilds the table the way the SQLite
//! documentation describes: create the new shape under a temporary name,
//! copy the rows, drop the old table, rename. That needs foreign key
//! enforcement off for the duration, which `Migration::foreign_keys_off`
//! tells the session.

use crate::ddl;
use crate::schema::{Column, Relation, RelationKind, Table};
use crate::sql::{Dialect, ident};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Migration {
    pub statements: Vec<String>,
    /// SQLite only: run with `PRAGMA foreign_keys = OFF` around the
    /// transaction, and check the keys before committing.
    pub foreign_keys_off: bool,
}

impl Migration {
    pub fn is_empty(&self) -> bool {
        self.statements.is_empty()
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        for s in &self.statements {
            out.push_str(s.trim_end());
            if !s.trim_end().ends_with(';') {
                out.push(';');
            }
            out.push('\n');
        }
        out
    }

    fn push(&mut self, s: String) {
        self.statements.push(s.trim().trim_end_matches(';').to_owned());
    }
}

/// A table before and after an edit, with the columns renamed on the way
/// (old name, new name).
#[derive(Clone, Debug)]
pub struct TableEdit<'a> {
    pub old: &'a Table,
    pub new: &'a Table,
    pub renamed_columns: &'a [(String, String)],
    /// Declared foreign keys from this table, before and after.
    pub old_fks: &'a [Relation],
    pub new_fks: &'a [Relation],
}

fn same_column(a: &Column, b: &Column) -> bool {
    a.data_type.trim().eq_ignore_ascii_case(b.data_type.trim())
        && a.accepts_null() == b.accepts_null()
        && a.default_key() == b.default_key()
        && a.auto_increment == b.auto_increment
        && a.generated == b.generated
        && a.comment.as_deref().unwrap_or("") == b.comment.as_deref().unwrap_or("")
}

fn same_fk(a: &Relation, b: &Relation) -> bool {
    a.from_columns == b.from_columns
        && a.to_table == b.to_table
        && a.to_columns == b.to_columns
        && a.on_delete == b.on_delete
        && a.on_update == b.on_update
}

fn fk_name(r: &Relation) -> String {
    r.name.clone().unwrap_or_else(|| format!("{}_{}_fkey", r.from_table, r.from_columns.join("_")))
}

/// The name of `new_name` before the edit.
fn old_name_of<'a>(edit: &'a TableEdit, new_name: &'a str) -> &'a str {
    edit.renamed_columns.iter().find(|(_, n)| n == new_name).map_or(new_name, |(o, _)| o.as_str())
}

/// The statements that turn `edit.old` into `edit.new`.
pub fn alter_table(dialect: Dialect, edit: &TableEdit) -> Migration {
    if dialect == Dialect::Sqlite && needs_rebuild(edit) {
        return sqlite_rebuild(edit);
    }
    let mut m = Migration::default();
    let q = |n: &str| ident(dialect, n);
    let old = edit.old;
    let new = edit.new;
    let table = q(&new.name);

    if old.name != new.name {
        m.push(format!("ALTER TABLE {} RENAME TO {table}", q(&old.name)));
    }

    // Foreign keys and indexes that go or change come off first, so the
    // columns under them can change.
    let fk_gone: Vec<&Relation> =
        edit.old_fks.iter().filter(|o| !edit.new_fks.iter().any(|n| same_fk(o, n) && fk_name(o) == fk_name(n))).collect();
    let fk_new: Vec<&Relation> =
        edit.new_fks.iter().filter(|n| !edit.old_fks.iter().any(|o| same_fk(o, n) && fk_name(o) == fk_name(n))).collect();
    for r in &fk_gone {
        match dialect {
            Dialect::MySql => m.push(format!("ALTER TABLE {table} DROP FOREIGN KEY {}", q(&fk_name(r)))),
            _ => m.push(format!("ALTER TABLE {table} DROP CONSTRAINT {}", q(&fk_name(r)))),
        }
    }
    let idx_gone: Vec<_> = old.indexes.iter().filter(|i| !new.indexes.contains(i)).collect();
    let idx_new: Vec<_> = new.indexes.iter().filter(|i| !old.indexes.contains(i)).collect();
    for i in &idx_gone {
        match dialect {
            Dialect::MySql => m.push(format!("ALTER TABLE {table} DROP INDEX {}", q(&i.name))),
            _ => m.push(format!("DROP INDEX {}", q(&i.name))),
        }
    }
    let check_gone: Vec<_> = old.checks.iter().filter(|c| !new.checks.contains(c)).collect();
    let check_new: Vec<_> = new.checks.iter().filter(|c| !old.checks.contains(c)).collect();
    for c in &check_gone {
        // MariaDB and MySQL 8.0.19+ take DROP CONSTRAINT like the others.
        if let Some(name) = &c.name {
            m.push(format!("ALTER TABLE {table} DROP CONSTRAINT {}", q(name)));
        }
    }

    let old_pk = old.primary_key();
    let new_pk: Vec<&str> = new.primary_key();
    let new_pk_as_old: Vec<&str> = new_pk.iter().map(|c| old_name_of(edit, c)).collect();
    let pk_changed = old_pk != new_pk_as_old;
    if pk_changed && !old_pk.is_empty() {
        match dialect {
            Dialect::MySql => m.push(format!("ALTER TABLE {table} DROP PRIMARY KEY")),
            _ => m.push(format!("ALTER TABLE {table} DROP CONSTRAINT {}", q(&format!("{}_pkey", old.name)))),
        }
    }

    for (from, to) in edit.renamed_columns {
        if from != to {
            m.push(format!("ALTER TABLE {table} RENAME COLUMN {} TO {}", q(from), q(to)));
        }
    }
    for c in &old.columns {
        let now = edit.renamed_columns.iter().find(|(o, _)| *o == c.name).map_or(c.name.as_str(), |(_, n)| n.as_str());
        if new.column(now).is_none() {
            m.push(format!("ALTER TABLE {table} DROP COLUMN {}", q(&c.name)));
        }
    }
    for c in &new.columns {
        let before = old.column(old_name_of(edit, &c.name));
        match before {
            None => m.push(format!("ALTER TABLE {table} ADD COLUMN {}", ddl::column_def(dialect, &unprimary(c), false))),
            Some(b) if !same_column(b, c) => alter_column(dialect, &mut m, &table, b, c),
            Some(_) => {}
        }
    }

    if pk_changed && !new_pk.is_empty() {
        let cols: Vec<String> = new_pk.iter().map(|c| q(c)).collect();
        m.push(format!("ALTER TABLE {table} ADD PRIMARY KEY ({})", cols.join(", ")));
    }
    for i in &idx_new {
        m.push(ddl::create_index(dialect, &new.name, i));
    }
    for c in &check_new {
        match &c.name {
            Some(n) => m.push(format!("ALTER TABLE {table} ADD CONSTRAINT {} CHECK ({})", q(n), c.expr)),
            None => m.push(format!("ALTER TABLE {table} ADD CHECK ({})", c.expr)),
        }
    }
    for r in &fk_new {
        m.push(format!("ALTER TABLE {table} ADD {}", ddl::foreign_key(dialect, r)));
    }
    m
}

/// A column without its primary flag: the key is added as a constraint.
fn unprimary(c: &Column) -> Column {
    let mut c = c.clone();
    if c.primary {
        c.nullable = false;
    }
    c.primary = false;
    c
}

fn alter_column(dialect: Dialect, m: &mut Migration, table: &str, before: &Column, after: &Column) {
    let col = ident(dialect, &after.name);
    match dialect {
        Dialect::MySql => {
            m.push(format!("ALTER TABLE {table} MODIFY COLUMN {}", ddl::column_def(dialect, &unprimary(after), false)));
        }
        Dialect::Postgres => {
            if !before.data_type.trim().eq_ignore_ascii_case(after.data_type.trim()) {
                m.push(format!(
                    "ALTER TABLE {table} ALTER COLUMN {col} TYPE {} USING {col}::{}",
                    after.data_type, after.data_type
                ));
            }
            if before.nullable != after.nullable && !after.primary {
                let what = if after.nullable { "DROP NOT NULL" } else { "SET NOT NULL" };
                m.push(format!("ALTER TABLE {table} ALTER COLUMN {col} {what}"));
            }
            if before.default.as_deref().map(str::trim) != after.default.as_deref().map(str::trim) {
                match after.default.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
                    Some(d) => m.push(format!("ALTER TABLE {table} ALTER COLUMN {col} SET DEFAULT {d}")),
                    None => m.push(format!("ALTER TABLE {table} ALTER COLUMN {col} DROP DEFAULT")),
                }
            }
            if before.comment.as_deref().unwrap_or("") != after.comment.as_deref().unwrap_or("") {
                let text = after.comment.as_deref().filter(|c| !c.is_empty());
                let value = text.map_or("NULL".to_owned(), |c| crate::sql::quoted(dialect, c));
                m.push(format!("COMMENT ON COLUMN {table}.{col} IS {value}"));
            }
            if before.auto_increment != after.auto_increment {
                let what = if after.auto_increment { "ADD GENERATED BY DEFAULT AS IDENTITY" } else { "DROP IDENTITY IF EXISTS" };
                m.push(format!("ALTER TABLE {table} ALTER COLUMN {col} {what}"));
            }
        }
        Dialect::Sqlite => unreachable!("SQLite column changes rebuild the table"),
    }
}

/// True when SQLite's ALTER TABLE cannot do the edit by itself.
fn needs_rebuild(edit: &TableEdit) -> bool {
    let old = edit.old;
    let new = edit.new;
    let new_pk_as_old: Vec<&str> = new.primary_key().iter().map(|c| old_name_of(edit, c)).collect();
    if old.primary_key() != new_pk_as_old || old.checks != new.checks || old.unique != new.unique {
        return true;
    }
    let fks_same = edit.old_fks.len() == edit.new_fks.len()
        && edit.old_fks.iter().all(|o| edit.new_fks.iter().any(|n| same_fk(o, n)));
    if !fks_same {
        return true;
    }
    for c in &new.columns {
        match old.column(old_name_of(edit, &c.name)) {
            // ADD COLUMN cannot add a NOT NULL column without a default.
            None if (!c.nullable && c.default.is_none()) || c.primary => return true,
            None => {}
            Some(b) if !same_column(b, c) => return true,
            Some(_) => {}
        }
    }
    false
}

fn sqlite_rebuild(edit: &TableEdit) -> Migration {
    let d = Dialect::Sqlite;
    let q = |n: &str| ident(d, n);
    let new = edit.new;
    let temp = format!("_atlas_new_{}", new.name);
    let mut m = Migration { foreign_keys_off: true, ..Default::default() };
    let mut shape = new.clone();
    shape.name = temp.clone();
    let fks: Vec<Relation> = edit.new_fks.iter().map(|r| Relation { from_table: temp.clone(), ..r.clone() }).collect();
    m.push(ddl::create_table(d, &shape, &fks, true).trim().to_owned());
    // Copy the columns that existed before, under their new names.
    let (mut to, mut from) = (Vec::new(), Vec::new());
    for c in &new.columns {
        let before = old_name_of(edit, &c.name);
        if edit.old.column(before).is_some() {
            to.push(q(&c.name));
            from.push(q(before));
        }
    }
    if !to.is_empty() {
        m.push(format!(
            "INSERT INTO {} ({}) SELECT {} FROM {}",
            q(&temp),
            to.join(", "),
            from.join(", "),
            q(&edit.old.name)
        ));
    }
    m.push(format!("DROP TABLE {}", q(&edit.old.name)));
    m.push(format!("ALTER TABLE {} RENAME TO {}", q(&temp), q(&new.name)));
    for i in &new.indexes {
        m.push(ddl::create_index(d, &new.name, i));
    }
    for t in &new.triggers {
        if t.body.trim().to_ascii_uppercase().starts_with("CREATE") {
            m.push(t.body.clone());
        }
    }
    m
}

/// CREATE TABLE plus its indexes, as a migration.
pub fn create(dialect: Dialect, table: &Table, fks: &[Relation]) -> Migration {
    let mut m = Migration::default();
    m.push(ddl::create_table(dialect, table, fks, true));
    for i in &table.indexes {
        m.push(ddl::create_index(dialect, &table.name, i));
    }
    // The PostgreSQL COMMENT lines ride along inside create_table's text;
    // split them into their own statements.
    let mut out = Migration::default();
    for s in &m.statements {
        for part in s.split(";\n") {
            if !part.trim().is_empty() {
                out.push(part.to_owned());
            }
        }
    }
    out
}

pub fn drop(dialect: Dialect, table: &Table) -> Migration {
    let mut m = Migration::default();
    m.push(ddl::drop_table(dialect, table));
    m
}

/// Declared foreign keys starting at `table`.
pub fn fks_of(relations: &[Relation], table: &str) -> Vec<Relation> {
    relations.iter().filter(|r| r.kind == RelationKind::Declared && r.from_table == table).cloned().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::fixture::{col, fk, table};
    use crate::schema::{FkAction, Index};

    fn base() -> Table {
        table("t", vec![col("id", "int", false, true), col("name", "varchar(20)", true, false)])
    }

    fn run(d: Dialect, old: &Table, new: &Table, renames: &[(String, String)]) -> Migration {
        alter_table(d, &TableEdit { old, new, renamed_columns: renames, old_fks: &[], new_fks: &[] })
    }

    #[test]
    fn nothing_changed_nothing_to_do() {
        for d in [Dialect::MySql, Dialect::Postgres, Dialect::Sqlite] {
            assert!(run(d, &base(), &base(), &[]).is_empty(), "{d:?}");
        }
    }

    #[test]
    fn add_rename_and_modify_in_mysql() {
        let old = base();
        let mut new = base();
        new.columns[1].name = "title".into();
        new.columns[1].data_type = "varchar(80)".into();
        new.columns.push(col("price", "decimal(8,2)", true, false));
        new.indexes.push(Index { name: "t_title".into(), columns: vec!["title".into()], unique: false, kind: String::new() });
        let m = run(Dialect::MySql, &old, &new, &[("name".into(), "title".into())]);
        assert_eq!(
            m.statements,
            [
                "ALTER TABLE `t` RENAME COLUMN `name` TO `title`",
                "ALTER TABLE `t` MODIFY COLUMN `title` varchar(80)",
                "ALTER TABLE `t` ADD COLUMN `price` decimal(8,2)",
                "CREATE INDEX `t_title` ON `t` (`title`)",
            ]
        );
    }

    #[test]
    fn postgres_alters_each_property() {
        let old = base();
        let mut new = base();
        new.name = "things".into();
        new.columns[1].nullable = false;
        new.columns[1].default = Some("''".into());
        let m = run(Dialect::Postgres, &old, &new, &[]);
        assert_eq!(
            m.statements,
            [
                "ALTER TABLE \"t\" RENAME TO \"things\"",
                "ALTER TABLE \"things\" ALTER COLUMN \"name\" SET NOT NULL",
                "ALTER TABLE \"things\" ALTER COLUMN \"name\" SET DEFAULT ''",
            ]
        );
    }

    #[test]
    fn primary_key_change_drops_and_adds() {
        let old = base();
        let mut new = base();
        new.columns[1].primary = true;
        new.columns[1].nullable = false;
        let m = run(Dialect::MySql, &old, &new, &[]);
        assert_eq!(m.statements[0], "ALTER TABLE `t` DROP PRIMARY KEY");
        assert_eq!(m.statements.last().unwrap(), "ALTER TABLE `t` ADD PRIMARY KEY (`id`, `name`)");
    }

    #[test]
    fn foreign_keys_change_in_postgres() {
        let old = base();
        let new = base();
        let before = vec![fk("t", "id", "p", "id")];
        let mut after = before.clone();
        after[0].on_delete = Some(FkAction::Cascade);
        let m = alter_table(
            Dialect::Postgres,
            &TableEdit { old: &old, new: &new, renamed_columns: &[], old_fks: &before, new_fks: &after },
        );
        assert_eq!(
            m.statements,
            [
                "ALTER TABLE \"t\" DROP CONSTRAINT \"t_id_fkey\"",
                "ALTER TABLE \"t\" ADD FOREIGN KEY (\"id\") REFERENCES \"p\" (\"id\") ON DELETE CASCADE",
            ]
        );
    }

    #[test]
    fn sqlite_adds_and_renames_without_rebuild() {
        let old = base();
        let mut new = base();
        new.columns[1].name = "title".into();
        new.columns.push(col("n", "int", true, false));
        let m = run(Dialect::Sqlite, &old, &new, &[("name".into(), "title".into())]);
        assert!(!m.foreign_keys_off);
        assert_eq!(
            m.statements,
            ["ALTER TABLE \"t\" RENAME COLUMN \"name\" TO \"title\"", "ALTER TABLE \"t\" ADD COLUMN \"n\" int"]
        );
    }

    #[test]
    fn sqlite_rebuilds_for_a_type_change() {
        let old = base();
        let mut new = base();
        new.columns[1].data_type = "text".into();
        new.columns[1].nullable = false;
        let m = run(Dialect::Sqlite, &old, &new, &[]);
        assert!(m.foreign_keys_off);
        assert_eq!(m.statements.len(), 4);
        assert!(m.statements[0].starts_with("CREATE TABLE \"_atlas_new_t\""));
        assert_eq!(m.statements[1], "INSERT INTO \"_atlas_new_t\" (\"id\", \"name\") SELECT \"id\", \"name\" FROM \"t\"");
        assert_eq!(m.statements[2], "DROP TABLE \"t\"");
        assert_eq!(m.statements[3], "ALTER TABLE \"_atlas_new_t\" RENAME TO \"t\"");
    }
}
