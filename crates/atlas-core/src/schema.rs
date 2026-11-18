//! What Atlas knows about a database: tables, their columns and keys, and
//! the relations between them. Every source (MariaDB, PostgreSQL, SQLite, a
//! dump) fills the same model.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    /// Database name as shown in the header bar.
    pub name: String,
    pub tables: Vec<Table>,
    pub relations: Vec<Relation>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    pub name: String,
    pub kind: TableKind,
    pub columns: Vec<Column>,
    /// Sets of columns that are unique together, the primary key included.
    pub unique: Vec<Vec<String>>,
    /// Row count; `estimated` when the server only guesses (InnoDB, pg).
    pub rows: Option<u64>,
    pub rows_estimated: bool,
    pub comment: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum TableKind {
    #[default]
    Table,
    View,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Column {
    pub name: String,
    /// The type as the database spells it (`varchar(255)`, `int unsigned`).
    pub data_type: String,
    pub nullable: bool,
    pub primary: bool,
    pub default: Option<String>,
    pub comment: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationKind {
    /// A declared foreign key.
    Declared,
    /// Guessed from names (`kunde_id` → `kunden.id`), drawn dashed.
    Inferred,
}

/// `from` (the child, holding the foreign key) refers to `to` (the parent).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    pub name: Option<String>,
    pub from_table: String,
    pub from_columns: Vec<String>,
    pub to_table: String,
    pub to_columns: Vec<String>,
    pub kind: RelationKind,
}

/// How many rows sit at one end of a relation, in crow's-foot terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    ExactlyOne,
    ZeroOrOne,
    ZeroOrMany,
}

impl Table {
    pub fn column(&self, name: &str) -> Option<&Column> {
        self.columns.iter().find(|c| c.name == name)
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    pub fn primary_key(&self) -> Vec<&str> {
        self.columns
            .iter()
            .filter(|c| c.primary)
            .map(|c| c.name.as_str())
            .collect()
    }

    /// True when exactly these columns (in any order) are unique together.
    pub fn is_unique(&self, columns: &[String]) -> bool {
        let mut wanted: Vec<&str> = columns.iter().map(String::as_str).collect();
        wanted.sort_unstable();
        let pk = {
            let mut pk = self.primary_key();
            pk.sort_unstable();
            pk
        };
        if !pk.is_empty() && pk == wanted {
            return true;
        }
        self.unique.iter().any(|set| {
            let mut set: Vec<&str> = set.iter().map(String::as_str).collect();
            set.sort_unstable();
            set == wanted
        })
    }
}

impl Schema {
    pub fn table(&self, name: &str) -> Option<&Table> {
        self.tables.iter().find(|t| t.name == name)
    }

    pub fn table_index(&self, name: &str) -> Option<usize> {
        self.tables.iter().position(|t| t.name == name)
    }

    /// The ends of `relation`: (child end, parent end).
    ///
    /// The parent end is "exactly one" when the foreign key cannot be null,
    /// else "zero or one". The child end is "zero or one" when the foreign
    /// key is unique (a 1:1 relation), else "zero or many".
    pub fn ends(&self, relation: &Relation) -> (End, End) {
        let Some(child) = self.table(&relation.from_table) else {
            return (End::ZeroOrMany, End::ExactlyOne);
        };
        let nullable = relation
            .from_columns
            .iter()
            .any(|c| child.column(c).is_none_or(|c| c.nullable));
        let parent = if nullable { End::ZeroOrOne } else { End::ExactlyOne };
        let child_end = if child.is_unique(&relation.from_columns) {
            End::ZeroOrOne
        } else {
            End::ZeroOrMany
        };
        (child_end, parent)
    }

    /// Names of the tables `table` is related to, in either direction.
    pub fn neighbours(&self, table: &str, with_inferred: bool) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for r in self.relations.iter().filter(|r| with_inferred || r.kind == RelationKind::Declared) {
            let other = if r.from_table == table {
                Some(r.to_table.as_str())
            } else if r.to_table == table {
                Some(r.from_table.as_str())
            } else {
                None
            };
            if let Some(o) = other
                && o != table
                && !out.contains(&o)
            {
                out.push(o);
            }
        }
        out
    }

    /// True when `column` of `table` is part of a foreign key.
    pub fn is_foreign(&self, table: &str, column: &str, with_inferred: bool) -> bool {
        self.relations.iter().any(|r| {
            (with_inferred || r.kind == RelationKind::Declared)
                && r.from_table == table
                && r.from_columns.iter().any(|c| c == column)
        })
    }

    /// The relation that `column` of `table` belongs to as a foreign key.
    pub fn relation_of(&self, table: &str, column: &str, with_inferred: bool) -> Option<&Relation> {
        self.relations.iter().find(|r| {
            (with_inferred || r.kind == RelationKind::Declared)
                && r.from_table == table
                && r.from_columns.iter().any(|c| c == column)
        })
    }
}

#[cfg(test)]
pub(crate) mod fixture {
    use super::*;

    pub fn col(name: &str, ty: &str, nullable: bool, primary: bool) -> Column {
        Column {
            name: name.into(),
            data_type: ty.into(),
            nullable,
            primary,
            ..Default::default()
        }
    }

    pub fn table(name: &str, columns: Vec<Column>) -> Table {
        Table {
            name: name.into(),
            columns,
            ..Default::default()
        }
    }

    pub fn fk(from: &str, fc: &str, to: &str, tc: &str) -> Relation {
        Relation {
            name: None,
            from_table: from.into(),
            from_columns: vec![fc.into()],
            to_table: to.into(),
            to_columns: vec![tc.into()],
            kind: RelationKind::Declared,
        }
    }

    /// A small shop: customers, orders, order lines, products, and a 1:1
    /// profile.
    pub fn shop() -> Schema {
        Schema {
            name: "shop".into(),
            tables: vec![
                table("customers", vec![col("id", "int", false, true), col("name", "varchar(80)", false, false)]),
                table(
                    "orders",
                    vec![
                        col("id", "int", false, true),
                        col("customer_id", "int", false, false),
                        col("placed_at", "datetime", false, false),
                    ],
                ),
                table(
                    "order_lines",
                    vec![
                        col("order_id", "int", false, true),
                        col("product_id", "int", true, true),
                        col("amount", "int", false, false),
                    ],
                ),
                table("products", vec![col("id", "int", false, true), col("title", "text", false, false)]),
                Table {
                    unique: vec![vec!["customer_id".into()]],
                    ..table(
                        "profiles",
                        vec![col("id", "int", false, true), col("customer_id", "int", true, false)],
                    )
                },
            ],
            relations: vec![
                fk("orders", "customer_id", "customers", "id"),
                fk("order_lines", "order_id", "orders", "id"),
                fk("order_lines", "product_id", "products", "id"),
                fk("profiles", "customer_id", "customers", "id"),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::shop;
    use super::*;

    #[test]
    fn ends_follow_nullability_and_uniqueness() {
        let s = shop();
        let r = |from: &str, col: &str| s.relations.iter().find(|r| r.from_table == from && r.from_columns[0] == col).unwrap();
        assert_eq!(s.ends(r("orders", "customer_id")), (End::ZeroOrMany, End::ExactlyOne));
        assert_eq!(s.ends(r("order_lines", "product_id")), (End::ZeroOrMany, End::ZeroOrOne));
        assert_eq!(s.ends(r("profiles", "customer_id")), (End::ZeroOrOne, End::ZeroOrOne));
    }

    #[test]
    fn neighbours_go_both_ways() {
        let s = shop();
        let mut n = s.neighbours("orders", true);
        n.sort_unstable();
        assert_eq!(n, ["customers", "order_lines"]);
    }

    #[test]
    fn composite_primary_key_is_unique_in_any_order() {
        let s = shop();
        let t = s.table("order_lines").unwrap();
        assert!(t.is_unique(&["product_id".into(), "order_id".into()]));
        assert!(!t.is_unique(&["order_id".into()]));
    }
}
