//! The schema as a Mermaid `erDiagram`, for READMEs and Obsidian.

use std::fmt::Write as _;

use crate::schema::{End, RelationKind, Schema};

/// Mermaid entity names: letters, digits, `_` and `-`.
fn entity(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    if s.chars().next().is_some_and(|c| c.is_ascii_digit()) { format!("_{s}") } else { s }
}

/// Attribute types allow letters, digits, `_`, `-`, `()` and `[]`.
fn data_type(ty: &str) -> String {
    ty.chars()
        .map(|c| match c {
            ',' => '-',
            c if c.is_alphanumeric() || "_-()[]".contains(c) => c,
            _ => '_',
        })
        .collect()
}

pub fn er_diagram(schema: &Schema, with_inferred: bool) -> String {
    let mut out = String::from("erDiagram\n");
    for table in &schema.tables {
        let _ = writeln!(out, "    {} {{", entity(&table.name));
        for c in &table.columns {
            let mut keys = Vec::new();
            if c.primary {
                keys.push("PK");
            }
            if schema.is_foreign(&table.name, &c.name, with_inferred) {
                keys.push("FK");
            }
            let _ = write!(out, "        {} {}", data_type(&c.data_type), entity(&c.name));
            if !keys.is_empty() {
                let _ = write!(out, " {}", keys.join(", "));
            }
            if let Some(comment) = &c.comment {
                let _ = write!(out, " \"{}\"", comment.replace('"', "'"));
            }
            out.push('\n');
        }
        out.push_str("    }\n");
    }
    for r in &schema.relations {
        if r.kind == RelationKind::Inferred && !with_inferred {
            continue;
        }
        let (child, parent) = schema.ends(r);
        let left = match parent {
            End::ExactlyOne => "||",
            _ => "|o",
        };
        let right = match child {
            End::ZeroOrMany => "o{",
            _ => "o|",
        };
        let line = if r.kind == RelationKind::Inferred { ".." } else { "--" };
        let _ = writeln!(
            out,
            "    {} {left}{line}{right} {} : \"{}\"",
            entity(&r.to_table),
            entity(&r.from_table),
            r.from_columns.join(", ")
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::fixture::shop;

    #[test]
    fn shop_diagram() {
        let text = er_diagram(&shop(), true);
        assert!(text.starts_with("erDiagram\n"));
        assert!(text.contains("    customers ||--o{ orders : \"customer_id\"\n"));
        assert!(text.contains("    products |o--o{ order_lines : \"product_id\"\n"));
        assert!(text.contains("    customers |o--o| profiles : \"customer_id\"\n"));
        assert!(text.contains("        varchar(80) name\n"));
        assert!(text.contains("        int customer_id FK\n"));
    }

    #[test]
    fn odd_names_are_made_safe() {
        assert_eq!(entity("order lines"), "order_lines");
        assert_eq!(entity("2fa"), "_2fa");
        assert_eq!(data_type("decimal(10,2) unsigned"), "decimal(10-2)_unsigned");
    }
}
