//! Relations guessed from names, for databases without declared foreign
//! keys (MyISAM tables, quick SQLite files, many PHP projects).
//!
//! A column `<stem>_id` or `<stem>Id` (also `id_<stem>`) points to a table
//! whose name is `<stem>` or a plural of it, when that table has a one-column
//! primary key. Columns already in a declared foreign key are left alone, and
//! so is a table's own primary key.

use crate::schema::{Relation, RelationKind, Schema};

/// The stem a column name points at, if it looks like a reference.
fn stem(column: &str) -> Option<String> {
    let lower = column.to_lowercase();
    if lower == "id" {
        return None;
    }
    if let Some(s) = lower.strip_suffix("_id").or_else(|| lower.strip_suffix("_fk")) {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    if let Some(s) = lower.strip_prefix("id_").or_else(|| lower.strip_prefix("fk_")) {
        return (!s.is_empty()).then(|| s.to_owned());
    }
    // camelCase: customerId
    if column.len() > 2 && column.ends_with("Id") {
        let s = &column[..column.len() - 2];
        if s.chars().last().is_some_and(char::is_lowercase) {
            return Some(s.to_lowercase());
        }
    }
    None
}

/// Table names a stem may be stored under: singular, English plurals, and
/// the common German ones (`kunde` → `kunden`, `artikel` → `artikel`).
fn candidates(stem: &str) -> Vec<String> {
    let mut out = vec![
        stem.to_owned(),
        format!("{stem}s"),
        format!("{stem}es"),
        format!("{stem}n"),
        format!("{stem}en"),
        format!("{stem}e"),
    ];
    if let Some(base) = stem.strip_suffix('y') {
        out.push(format!("{base}ies"));
    }
    // A prefix such as `parent_`, `main_` is not part of the table's name:
    // `parent_category_id` → `category`.
    if let Some((_, last)) = stem.rsplit_once('_') {
        out.extend(candidates(last));
    }
    out
}

/// Adds inferred relations to `schema` and returns how many were added.
pub fn infer(schema: &mut Schema) -> usize {
    let mut found = Vec::new();
    for table in &schema.tables {
        for column in &table.columns {
            if column.primary && table.primary_key().len() == 1 {
                continue;
            }
            if schema.is_foreign(&table.name, &column.name, false) {
                continue;
            }
            let Some(stem) = stem(&column.name) else { continue };
            let target = candidates(&stem).into_iter().find_map(|name| {
                schema.tables.iter().find(|t| t.name.to_lowercase() == name)
            });
            let Some(target) = target else { continue };
            let pk = target.primary_key();
            if pk.len() != 1 {
                continue;
            }
            // A reference to itself by the same column is no reference.
            if target.name == table.name && pk[0] == column.name {
                continue;
            }
            found.push(Relation {
                name: None,
                from_table: table.name.clone(),
                from_columns: vec![column.name.clone()],
                to_table: target.name.clone(),
                to_columns: vec![pk[0].to_owned()],
                kind: RelationKind::Inferred,
                ..Default::default()
            });
        }
    }
    let n = found.len();
    schema.relations.extend(found);
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::fixture::{col, table};

    fn schema() -> Schema {
        Schema {
            name: "x".into(),
            tables: vec![
                table("kunden", vec![col("id", "int", false, true)]),
                table("categories", vec![col("id", "int", false, true)]),
                table(
                    "bestellungen",
                    vec![
                        col("id", "int", false, true),
                        col("kunde_id", "int", false, false),
                        col("parent_category_id", "int", true, false),
                        col("customerId", "int", true, false),
                        col("valid", "bool", false, false),
                    ],
                ),
            ],
            relations: vec![],
            ..Default::default()
        }
    }

    #[test]
    fn finds_german_and_english_plurals_and_prefixes() {
        let mut s = schema();
        assert_eq!(infer(&mut s), 2);
        let targets: Vec<_> = s.relations.iter().map(|r| (r.from_columns[0].as_str(), r.to_table.as_str())).collect();
        assert!(targets.contains(&("kunde_id", "kunden")));
        assert!(targets.contains(&("parent_category_id", "categories")));
        assert!(s.relations.iter().all(|r| r.kind == RelationKind::Inferred));
    }

    #[test]
    fn declared_keys_are_not_guessed_again() {
        let mut s = schema();
        s.relations.push(crate::schema::fixture::fk("bestellungen", "kunde_id", "kunden", "id"));
        infer(&mut s);
        assert_eq!(s.relations.iter().filter(|r| r.from_columns[0] == "kunde_id").count(), 1);
    }

    #[test]
    fn stems() {
        assert_eq!(stem("customerId").as_deref(), Some("customer"));
        assert_eq!(stem("id_kunde").as_deref(), Some("kunde"));
        assert_eq!(stem("id"), None);
        assert_eq!(stem("paid"), None);
    }
}
