//! Changes to rows that the user has made in the grid but not yet sent.
//!
//! A row is found again by the values of its key columns as they were when
//! it was loaded (`Table::row_key`), so editing a key column still updates
//! the right row. Nothing is written until the user commits; then every
//! change goes in one transaction.

use std::collections::BTreeMap;

use crate::sql::{Dialect, ident, literal};

/// The key values of one row, in the order of `Changes::key_columns`.
pub type Key = Vec<Option<String>>;
/// Column name → new value (None is NULL).
pub type Values = BTreeMap<String, Option<String>>;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Changes {
    pub key_columns: Vec<String>,
    pub updates: BTreeMap<Key, Values>,
    pub inserts: Vec<Values>,
    pub deletes: Vec<Key>,
}

impl Changes {
    pub fn new(key_columns: Vec<String>) -> Self {
        Changes { key_columns, ..Default::default() }
    }

    pub fn is_empty(&self) -> bool {
        self.updates.is_empty() && self.inserts.is_empty() && self.deletes.is_empty()
    }

    /// Changed cells plus inserted and deleted rows.
    pub fn count(&self) -> usize {
        self.updates.values().map(BTreeMap::len).sum::<usize>() + self.inserts.len() + self.deletes.len()
    }

    /// Sets one cell. Setting it back to `original` forgets the change.
    pub fn set(&mut self, key: Key, column: &str, value: Option<String>, original: Option<&str>) {
        if self.deletes.contains(&key) {
            return;
        }
        let row = self.updates.entry(key.clone()).or_default();
        if value.as_deref() == original {
            row.remove(column);
        } else {
            row.insert(column.to_owned(), value);
        }
        if row.is_empty() {
            self.updates.remove(&key);
        }
    }

    /// The pending value of a cell, if it was changed.
    pub fn pending(&self, key: &Key, column: &str) -> Option<&Option<String>> {
        self.updates.get(key).and_then(|r| r.get(column))
    }

    pub fn delete(&mut self, key: Key) {
        self.updates.remove(&key);
        if !self.deletes.contains(&key) {
            self.deletes.push(key);
        }
    }

    pub fn undelete(&mut self, key: &Key) {
        self.deletes.retain(|k| k != key);
    }

    pub fn is_deleted(&self, key: &Key) -> bool {
        self.deletes.contains(key)
    }

    pub fn insert(&mut self, values: Values) {
        self.inserts.push(values);
    }

    pub fn clear(&mut self) {
        self.updates.clear();
        self.inserts.clear();
        self.deletes.clear();
    }

    fn where_key(&self, dialect: Dialect, key: &Key) -> String {
        let parts: Vec<String> = self
            .key_columns
            .iter()
            .zip(key)
            .map(|(c, v)| match v {
                Some(v) => format!("{} = {}", ident(dialect, c), literal(dialect, v)),
                None => format!("{} IS NULL", ident(dialect, c)),
            })
            .collect();
        parts.join(" AND ")
    }

    /// The statements, in the order they must run: deletes, updates,
    /// inserts.
    pub fn to_sql(&self, dialect: Dialect, table: &str) -> Vec<String> {
        let t = ident(dialect, table);
        let mut out = Vec::new();
        for key in &self.deletes {
            out.push(format!("DELETE FROM {t} WHERE {}", self.where_key(dialect, key)));
        }
        for (key, values) in &self.updates {
            let sets: Vec<String> = values
                .iter()
                .map(|(c, v)| {
                    let v = v.as_deref().map_or("NULL".to_owned(), |v| literal(dialect, v));
                    format!("{} = {v}", ident(dialect, c))
                })
                .collect();
            out.push(format!("UPDATE {t} SET {} WHERE {}", sets.join(", "), self.where_key(dialect, key)));
        }
        for values in &self.inserts {
            if values.is_empty() {
                out.push(match dialect {
                    Dialect::MySql => format!("INSERT INTO {t} () VALUES ()"),
                    _ => format!("INSERT INTO {t} DEFAULT VALUES"),
                });
                continue;
            }
            let cols: Vec<String> = values.keys().map(|c| ident(dialect, c)).collect();
            let vals: Vec<String> =
                values.values().map(|v| v.as_deref().map_or("NULL".to_owned(), |v| literal(dialect, v))).collect();
            out.push(format!("INSERT INTO {t} ({}) VALUES ({})", cols.join(", "), vals.join(", ")));
        }
        out
    }
}

/// The key of `row` (a result row in `columns` order).
pub fn key_of(key_columns: &[String], columns: &[String], row: &[Option<String>]) -> Option<Key> {
    key_columns
        .iter()
        .map(|k| columns.iter().position(|c| c == k).map(|i| row.get(i).cloned().flatten()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_in_order() {
        let mut c = Changes::new(vec!["id".into()]);
        c.set(vec![Some("1".into())], "name", Some("Anna".into()), Some("Ana"));
        c.set(vec![Some("1".into())], "note", None, Some("x"));
        c.delete(vec![Some("2".into())]);
        let mut row = Values::new();
        row.insert("name".into(), Some("Ben".into()));
        c.insert(row);
        assert_eq!(c.count(), 4);
        assert_eq!(
            c.to_sql(Dialect::Sqlite, "people"),
            [
                "DELETE FROM \"people\" WHERE \"id\" = '2'",
                "UPDATE \"people\" SET \"name\" = 'Anna', \"note\" = NULL WHERE \"id\" = '1'",
                "INSERT INTO \"people\" (\"name\") VALUES ('Ben')",
            ]
        );
    }

    #[test]
    fn setting_back_forgets() {
        let mut c = Changes::new(vec!["id".into()]);
        let k = vec![Some("1".into())];
        c.set(k.clone(), "a", Some("x".into()), Some("y"));
        c.set(k.clone(), "a", Some("y".into()), Some("y"));
        assert!(c.is_empty());
    }

    #[test]
    fn delete_wins_over_update_and_mariadb_uses_hex() {
        let mut c = Changes::new(vec!["a".into(), "b".into()]);
        let k = vec![Some("1".into()), None];
        c.set(k.clone(), "x", Some("v".into()), None);
        c.delete(k.clone());
        c.set(k.clone(), "x", Some("w".into()), None);
        assert_eq!(
            c.to_sql(Dialect::MySql, "t"),
            ["DELETE FROM `t` WHERE `a` = CONVERT(X'31' USING utf8mb4) AND `b` IS NULL"]
        );
    }

    #[test]
    fn empty_insert_uses_defaults() {
        let mut c = Changes::new(vec!["id".into()]);
        c.insert(Values::new());
        assert_eq!(c.to_sql(Dialect::Postgres, "t"), ["INSERT INTO \"t\" DEFAULT VALUES"]);
        assert_eq!(c.to_sql(Dialect::MySql, "t"), ["INSERT INTO `t` () VALUES ()"]);
    }

    #[test]
    fn key_of_picks_columns() {
        let cols = vec!["x".to_owned(), "id".to_owned()];
        assert_eq!(key_of(&["id".into()], &cols, &[Some("a".into()), Some("7".into())]), Some(vec![Some("7".into())]));
        assert_eq!(key_of(&["nope".into()], &cols, &[None, None]), None);
    }
}
