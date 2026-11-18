//! What differs between two schemas, for "Compare with…" and for applying a
//! design to a live database. Tables and columns are matched by name; a
//! rename shows up as one removed and one added.

use crate::migrate::{self, Migration, TableEdit};
use crate::schema::{Relation, RelationKind, Schema, Table, TableKind};
use crate::sql::Dialect;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TableDiff {
    pub name: String,
    pub added_columns: Vec<String>,
    pub removed_columns: Vec<String>,
    /// Column name and what changed about it (`type`, `null`, `default`,
    /// `key`).
    pub changed_columns: Vec<(String, Vec<&'static str>)>,
    pub added_indexes: Vec<String>,
    pub removed_indexes: Vec<String>,
    pub added_fks: Vec<String>,
    pub removed_fks: Vec<String>,
    pub checks_changed: bool,
}

impl TableDiff {
    pub fn is_empty(&self) -> bool {
        self.added_columns.is_empty()
            && self.removed_columns.is_empty()
            && self.changed_columns.is_empty()
            && self.added_indexes.is_empty()
            && self.removed_indexes.is_empty()
            && self.added_fks.is_empty()
            && self.removed_fks.is_empty()
            && !self.checks_changed
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SchemaDiff {
    /// In `to` but not in `from`.
    pub added_tables: Vec<String>,
    /// In `from` but not in `to`.
    pub removed_tables: Vec<String>,
    pub changed_tables: Vec<TableDiff>,
}

impl SchemaDiff {
    pub fn is_empty(&self) -> bool {
        self.added_tables.is_empty() && self.removed_tables.is_empty() && self.changed_tables.is_empty()
    }
}

fn fk_label(r: &Relation) -> String {
    format!("({}) → {}({})", r.from_columns.join(", "), r.to_table, r.to_columns.join(", "))
}

fn declared<'a>(s: &'a Schema, table: &str) -> Vec<&'a Relation> {
    s.relations.iter().filter(|r| r.kind == RelationKind::Declared && r.from_table == table).collect()
}

fn fk_eq(a: &Relation, b: &Relation) -> bool {
    a.from_columns == b.from_columns
        && a.to_table == b.to_table
        && a.to_columns == b.to_columns
        && a.on_delete == b.on_delete
        && a.on_update == b.on_update
}

pub fn table_diff(from: &Schema, a: &Table, to: &Schema, b: &Table) -> TableDiff {
    let mut d = TableDiff { name: b.name.clone(), ..Default::default() };
    for c in &b.columns {
        match a.column(&c.name) {
            None => d.added_columns.push(c.name.clone()),
            Some(old) => {
                let mut what = Vec::new();
                if !old.data_type.trim().eq_ignore_ascii_case(c.data_type.trim()) {
                    what.push("type");
                }
                if old.accepts_null() != c.accepts_null() {
                    what.push("null");
                }
                if old.default_key() != c.default_key() {
                    what.push("default");
                }
                if old.primary != c.primary {
                    what.push("key");
                }
                if !what.is_empty() {
                    d.changed_columns.push((c.name.clone(), what));
                }
            }
        }
    }
    d.removed_columns = a.columns.iter().filter(|c| b.column(&c.name).is_none()).map(|c| c.name.clone()).collect();
    d.added_indexes = b.indexes.iter().filter(|i| !a.indexes.contains(i)).map(|i| i.name.clone()).collect();
    d.removed_indexes = a.indexes.iter().filter(|i| !b.indexes.contains(i)).map(|i| i.name.clone()).collect();
    let (fa, fb) = (declared(from, &a.name), declared(to, &b.name));
    d.added_fks = fb.iter().filter(|r| !fa.iter().any(|o| fk_eq(o, r))).map(|r| fk_label(r)).collect();
    d.removed_fks = fa.iter().filter(|r| !fb.iter().any(|n| fk_eq(r, n))).map(|r| fk_label(r)).collect();
    d.checks_changed = a.checks != b.checks;
    d
}

pub fn diff(from: &Schema, to: &Schema) -> SchemaDiff {
    let mut out = SchemaDiff::default();
    for b in &to.tables {
        match from.table(&b.name) {
            None => out.added_tables.push(b.name.clone()),
            Some(a) => {
                let d = table_diff(from, a, to, b);
                if !d.is_empty() || a.kind != b.kind || a.definition != b.definition {
                    out.changed_tables.push(d);
                }
            }
        }
    }
    out.removed_tables = from.tables.iter().filter(|t| to.table(&t.name).is_none()).map(|t| t.name.clone()).collect();
    out
}

/// The statements that turn `from` into `to` in `dialect`. New tables come
/// first, without their foreign keys, so keys between new tables can be
/// added once both exist; removed tables go last.
pub fn migration(dialect: Dialect, from: &Schema, to: &Schema) -> Migration {
    let d = diff(from, to);
    let mut out = Migration::default();
    let mut later_fks: Vec<Relation> = Vec::new();
    for name in &d.added_tables {
        let t = to.table(name).expect("added table exists in target");
        let fks = migrate::fks_of(&to.relations, name);
        let m = if dialect == Dialect::Sqlite || t.kind == TableKind::View {
            migrate::create(dialect, t, &fks)
        } else {
            later_fks.extend(fks);
            migrate::create(dialect, t, &[])
        };
        out.statements.extend(m.statements);
    }
    for td in &d.changed_tables {
        let (a, b) = (from.table(&td.name).unwrap(), to.table(&td.name).unwrap());
        if a.kind == TableKind::View || b.kind == TableKind::View {
            out.statements.push(crate::ddl::drop_table(dialect, a).trim().trim_end_matches(';').to_owned());
            out.statements.push(crate::ddl::create_view(dialect, b).trim().trim_end_matches(';').to_owned());
            continue;
        }
        let (old_fks, new_fks) = (migrate::fks_of(&from.relations, &a.name), migrate::fks_of(&to.relations, &b.name));
        let m = migrate::alter_table(
            dialect,
            &TableEdit { old: a, new: b, renamed_columns: &[], old_fks: &old_fks, new_fks: &new_fks },
        );
        out.foreign_keys_off |= m.foreign_keys_off;
        out.statements.extend(m.statements);
    }
    for r in &later_fks {
        out.statements.push(format!(
            "ALTER TABLE {} ADD {}",
            crate::sql::ident(dialect, &r.from_table),
            crate::ddl::foreign_key(dialect, r)
        ));
    }
    for name in &d.removed_tables {
        let t = from.table(name).unwrap();
        out.statements.push(crate::ddl::drop_table(dialect, t).trim().trim_end_matches(';').to_owned());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::fixture::{col, fk, shop, table};

    #[test]
    fn same_schema_no_difference() {
        assert!(diff(&shop(), &shop()).is_empty());
        assert!(migration(Dialect::MySql, &shop(), &shop()).is_empty());
    }

    #[test]
    fn catalog_quirks_are_not_differences() {
        let from = shop();
        let mut to = shop();
        to.tables[0].columns[0].nullable = true; // a primary key, as SQLite reports it
        to.tables[1].columns[2].default = Some("NULL".into());
        let mut a = shop();
        a.tables[1].columns[1].default = Some("TRUE".into());
        to.tables[1].columns[1].default = Some("true".into());
        assert!(diff(&from, &to).changed_tables.iter().all(|t| t.name != "customers"));
        assert!(diff(&a, &to).changed_tables.iter().all(|t| t.name != "customers" && t.name != "orders"), "{:?}", diff(&a, &to));
    }

    #[test]
    fn finds_tables_columns_and_keys() {
        let from = shop();
        let mut to = shop();
        to.tables.retain(|t| t.name != "profiles");
        to.relations.retain(|r| r.from_table != "profiles");
        to.tables.push(table("tags", vec![col("id", "int", false, true), col("order_id", "int", false, false)]));
        to.relations.push(fk("tags", "order_id", "orders", "id"));
        let orders = to.tables.iter_mut().find(|t| t.name == "orders").unwrap();
        orders.columns[2].data_type = "timestamp".into();
        orders.columns.push(col("note", "text", true, false));
        let d = diff(&from, &to);
        assert_eq!(d.added_tables, ["tags"]);
        assert_eq!(d.removed_tables, ["profiles"]);
        assert_eq!(d.changed_tables.len(), 1);
        assert_eq!(d.changed_tables[0].added_columns, ["note"]);
        assert_eq!(d.changed_tables[0].changed_columns, [("placed_at".to_owned(), vec!["type"])]);

        let m = migration(Dialect::Postgres, &from, &to);
        let text = m.text();
        assert!(text.starts_with("CREATE TABLE \"tags\""), "{text}");
        assert!(text.contains("ALTER TABLE \"orders\" ALTER COLUMN \"placed_at\" TYPE timestamp"), "{text}");
        assert!(text.contains("ALTER TABLE \"tags\" ADD FOREIGN KEY (\"order_id\") REFERENCES \"orders\" (\"id\")"), "{text}");
        assert!(text.trim_end().ends_with("DROP TABLE \"profiles\";"), "{text}");
    }
}
