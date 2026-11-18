//! Design Mode: a table's columns as lines of text for quick editing on the
//! canvas, and the design file that keeps a schema with its layout.
//!
//! One column per line: `name type [pk] [null] [auto] [default <expr>]`,
//! for example `id integer pk auto` or `note text null`.

use serde::{Deserialize, Serialize};

use crate::schema::{Column, Schema};
use crate::store::Layout;

pub fn columns_to_text(columns: &[Column]) -> String {
    let mut out = String::new();
    for c in columns {
        out.push_str(&c.name);
        out.push(' ');
        out.push_str(if c.data_type.is_empty() { "text" } else { &c.data_type });
        if c.primary {
            out.push_str(" pk");
        }
        if c.nullable && !c.primary {
            out.push_str(" null");
        }
        if c.auto_increment {
            out.push_str(" auto");
        }
        if let Some(d) = &c.default {
            out.push_str(" default ");
            out.push_str(d);
        }
        out.push('\n');
    }
    out
}

/// Reads the lines back. Types with spaces keep them (`int unsigned`,
/// `character varying(80)`): everything between the name and the first
/// flag is the type. Earlier columns of the same name keep their other
/// properties (comments, enum values).
pub fn columns_from_text(text: &str, before: &[Column]) -> Result<Vec<Column>, String> {
    let mut out: Vec<Column> = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut words = line.split_whitespace();
        let name = words.next().unwrap_or_default().to_owned();
        let rest: Vec<&str> = words.collect();
        let flag = |w: &str| matches!(w.to_ascii_lowercase().as_str(), "pk" | "null" | "auto" | "default");
        let type_end = rest.iter().position(|w| flag(w)).unwrap_or(rest.len());
        let data_type = rest[..type_end].join(" ");
        if data_type.is_empty() {
            return Err(format!("line {}: {name} needs a type", n + 1));
        }
        if out.iter().any(|c| c.name == name) {
            return Err(format!("line {}: {name} appears twice", n + 1));
        }
        let mut c = before.iter().find(|c| c.name == name).cloned().unwrap_or_default();
        c.name = name;
        c.data_type = data_type;
        c.primary = false;
        c.nullable = false;
        c.auto_increment = false;
        c.default = None;
        let mut i = type_end;
        while i < rest.len() {
            match rest[i].to_ascii_lowercase().as_str() {
                "pk" => c.primary = true,
                "null" => c.nullable = true,
                "auto" => c.auto_increment = true,
                "default" => {
                    c.default = Some(rest[i + 1..].join(" ")).filter(|d| !d.is_empty());
                    break;
                }
                other => return Err(format!("line {}: unknown word {other}", n + 1)),
            }
            i += 1;
        }
        if c.primary {
            c.nullable = false;
        }
        out.push(c);
    }
    if out.is_empty() {
        return Err("a table needs at least one column".into());
    }
    Ok(out)
}

/// A design saved as a file: the schema and where its tables stand.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct DesignFile {
    pub atlas_design: u32,
    pub schema: Schema,
    pub layout: Layout,
}

pub fn save(schema: &Schema, layout: &Layout) -> String {
    serde_json::to_string_pretty(&DesignFile { atlas_design: 1, schema: schema.clone(), layout: layout.clone() }).unwrap_or_default()
}

/// None when the text is not an Atlas design.
pub fn load(text: &str) -> Option<DesignFile> {
    let d: DesignFile = serde_json::from_str(text).ok()?;
    (d.atlas_design >= 1).then_some(d)
}

/// A table name that is not taken yet: `table`, `table_2`, …
pub fn free_name(schema: &Schema, base: &str) -> String {
    if schema.table(base).is_none() {
        return base.to_owned();
    }
    (2..).map(|n| format!("{base}_{n}")).find(|n| schema.table(n).is_none()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::fixture::shop;

    #[test]
    fn text_round_trip() {
        let s = shop();
        let cols = &s.table("orders").unwrap().columns;
        let text = columns_to_text(cols);
        assert_eq!(text, "id int pk\ncustomer_id int\nplaced_at datetime\n");
        assert_eq!(columns_from_text(&text, cols).unwrap(), *cols);
    }

    #[test]
    fn types_with_spaces_and_defaults() {
        let c = columns_from_text("n int unsigned null default 0\nname character varying(80)", &[]).unwrap();
        assert_eq!(c[0].data_type, "int unsigned");
        assert!(c[0].nullable);
        assert_eq!(c[0].default.as_deref(), Some("0"));
        assert_eq!(c[1].data_type, "character varying(80)");
        assert!(columns_from_text("x", &[]).is_err());
        assert!(columns_from_text("a int\na int", &[]).is_err());
    }

    #[test]
    fn design_file_and_names() {
        let s = shop();
        let text = save(&s, &Layout::default());
        assert_eq!(load(&text).unwrap().schema, s);
        assert!(load("{\"tables\": []}").is_none());
        assert_eq!(free_name(&s, "orders"), "orders_2");
        assert_eq!(free_name(&s, "tags"), "tags");
    }
}
