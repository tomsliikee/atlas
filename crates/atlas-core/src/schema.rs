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
    /// Stored procedures and functions.
    #[serde(default)]
    pub routines: Vec<Routine>,
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
    /// Every index except the primary key, unique ones included.
    #[serde(default)]
    pub indexes: Vec<Index>,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub triggers: Vec<Trigger>,
    /// The SELECT behind a view, as the server prints it.
    #[serde(default)]
    pub definition: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Index {
    pub name: String,
    /// Column names, or the expression text for an expression index.
    pub columns: Vec<String>,
    pub unique: bool,
    /// `btree`, `hash`, `gin`, `fulltext`… as the server names it; empty
    /// when unknown.
    #[serde(default)]
    pub kind: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: Option<String>,
    /// The condition without the surrounding `CHECK (…)`.
    pub expr: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Trigger {
    pub name: String,
    /// `BEFORE`, `AFTER` or `INSTEAD OF`.
    pub timing: String,
    /// `INSERT`, `UPDATE`, `DELETE`, or several joined by ` OR `.
    pub event: String,
    /// The full statement or body as the server keeps it.
    pub body: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutineKind {
    #[default]
    Procedure,
    Function,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Routine {
    pub name: String,
    pub kind: RoutineKind,
    /// The full `CREATE …` text when the server hands it out, else the body.
    pub definition: String,
}

/// What a foreign key does when its parent row is deleted or updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FkAction {
    NoAction,
    Restrict,
    Cascade,
    SetNull,
    SetDefault,
}

impl FkAction {
    pub const ALL: [FkAction; 5] =
        [FkAction::NoAction, FkAction::Restrict, FkAction::Cascade, FkAction::SetNull, FkAction::SetDefault];

    pub fn sql(self) -> &'static str {
        match self {
            FkAction::NoAction => "NO ACTION",
            FkAction::Restrict => "RESTRICT",
            FkAction::Cascade => "CASCADE",
            FkAction::SetNull => "SET NULL",
            FkAction::SetDefault => "SET DEFAULT",
        }
    }

    /// Reads `CASCADE`, `set null`, `SET_NULL`, and PostgreSQL's one-letter
    /// codes (`a r c n d`).
    pub fn parse(text: &str) -> Option<FkAction> {
        let t = text.trim().to_ascii_uppercase().replace('_', " ");
        Some(match t.as_str() {
            "NO ACTION" | "A" => FkAction::NoAction,
            "RESTRICT" | "R" => FkAction::Restrict,
            "CASCADE" | "C" => FkAction::Cascade,
            "SET NULL" | "N" => FkAction::SetNull,
            "SET DEFAULT" | "D" => FkAction::SetDefault,
            _ => return None,
        })
    }
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
    /// The allowed values of an ENUM column (MariaDB `enum(…)`, PostgreSQL
    /// enum types).
    #[serde(default)]
    pub enum_values: Option<Vec<String>>,
    /// The expression of a generated (computed) column.
    #[serde(default)]
    pub generated: Option<String>,
    /// AUTO_INCREMENT, SERIAL/IDENTITY, or SQLite's INTEGER PRIMARY KEY rowid.
    #[serde(default)]
    pub auto_increment: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RelationKind {
    /// A declared foreign key.
    #[default]
    Declared,
    /// Guessed from names (`kunde_id` → `kunden.id`), drawn dashed.
    Inferred,
}

/// `from` (the child, holding the foreign key) refers to `to` (the parent).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Relation {
    pub name: Option<String>,
    pub from_table: String,
    pub from_columns: Vec<String>,
    pub to_table: String,
    pub to_columns: Vec<String>,
    pub kind: RelationKind,
    #[serde(default)]
    pub on_delete: Option<FkAction>,
    #[serde(default)]
    pub on_update: Option<FkAction>,
}

/// How many rows sit at one end of a relation, in crow's-foot terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    ExactlyOne,
    ZeroOrOne,
    ZeroOrMany,
}

/// The values of a MariaDB/MySQL `enum('a','b')` or `set(…)` type, quotes
/// undoubled. None for any other type.
pub fn enum_values(data_type: &str) -> Option<Vec<String>> {
    let t = data_type.trim();
    let lower = t.to_ascii_lowercase();
    let inner = if lower.starts_with("enum(") {
        &t[5..]
    } else if lower.starts_with("set(") {
        &t[4..]
    } else {
        return None;
    };
    let inner = inner.strip_suffix(')')?;
    let mut out = Vec::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\'' {
            continue;
        }
        let mut v = String::new();
        while let Some(c) = chars.next() {
            if c == '\'' {
                if chars.peek() == Some(&'\'') {
                    chars.next();
                    v.push('\'');
                } else {
                    break;
                }
            } else if c == '\\' {
                if let Some(n) = chars.next() {
                    v.push(n);
                }
            } else {
                v.push(c);
            }
        }
        out.push(v);
    }
    Some(out)
}

impl Column {
    /// NULL allowed in fact: a primary key column never is, whatever the
    /// catalog says (SQLite reports `INTEGER PRIMARY KEY` as nullable).
    pub fn accepts_null(&self) -> bool {
        self.nullable && !self.primary
    }

    /// The default as text to compare: trimmed, keywords in any case.
    pub fn default_key(&self) -> Option<String> {
        self.default.as_deref().map(|d| d.trim().to_ascii_lowercase()).filter(|d| !d.is_empty() && d != "null")
    }
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

    /// The columns that identify a row for UPDATE/DELETE: the primary key,
    /// else the first unique set whose columns are all NOT NULL.
    pub fn row_key(&self) -> Option<Vec<String>> {
        let pk = self.primary_key();
        if !pk.is_empty() {
            return Some(pk.into_iter().map(str::to_owned).collect());
        }
        let sets = self.unique.iter().cloned().chain(self.indexes.iter().filter(|i| i.unique).map(|i| i.columns.clone()));
        sets.into_iter()
            .find(|set| !set.is_empty() && set.iter().all(|c| self.column(c).is_some_and(|c| !c.nullable)))
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
        self.unique.iter().chain(self.indexes.iter().filter(|i| i.unique).map(|i| &i.columns)).any(|set| {
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
            ..Default::default()
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
            ..Default::default()
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
    fn enum_values_are_unquoted() {
        assert_eq!(enum_values("enum('a','it''s','b,c')").unwrap(), ["a", "it's", "b,c"]);
        assert_eq!(enum_values("SET('x')").unwrap(), ["x"]);
        assert_eq!(enum_values("varchar(3)"), None);
    }

    #[test]
    fn row_key_falls_back_to_a_not_null_unique_set() {
        let mut t = fixture::table("t", vec![fixture::col("a", "int", true, false), fixture::col("b", "int", false, false)]);
        assert_eq!(t.row_key(), None);
        t.indexes.push(Index { name: "u_a".into(), columns: vec!["a".into()], unique: true, kind: String::new() });
        assert_eq!(t.row_key(), None, "nullable column cannot identify a row");
        t.indexes.push(Index { name: "u_b".into(), columns: vec!["b".into()], unique: true, kind: String::new() });
        assert_eq!(t.row_key().unwrap(), ["b"]);
    }

    #[test]
    fn fk_actions_parse_names_and_pg_codes() {
        assert_eq!(FkAction::parse("set null"), Some(FkAction::SetNull));
        assert_eq!(FkAction::parse("c"), Some(FkAction::Cascade));
        assert_eq!(FkAction::parse("NO_ACTION"), Some(FkAction::NoAction));
        assert_eq!(FkAction::parse("x"), None);
    }

    #[test]
    fn composite_primary_key_is_unique_in_any_order() {
        let s = shop();
        let t = s.table("order_lines").unwrap();
        assert!(t.is_unique(&["product_id".into(), "order_id".into()]));
        assert!(!t.is_unique(&["order_id".into()]));
    }
}
