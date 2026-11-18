//! Suggestions for the SQL console: keywords, tables, and the columns of
//! the tables the statement mentions. `alias.` and `table.` narrow to that
//! table's columns.

use crate::schema::Schema;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    Keyword,
    Table,
    Column,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub text: String,
    pub kind: SuggestionKind,
    /// For columns: the table and the type.
    pub detail: String,
}

const KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "NULL", "IS", "IN", "LIKE", "BETWEEN", "JOIN", "LEFT", "RIGHT",
    "INNER", "OUTER", "ON", "AS", "GROUP BY", "ORDER BY", "HAVING", "LIMIT", "OFFSET", "DISTINCT", "COUNT", "SUM",
    "AVG", "MIN", "MAX", "CASE", "WHEN", "THEN", "ELSE", "END", "UNION", "WITH", "EXISTS", "ASC", "DESC", "INSERT INTO",
    "VALUES", "UPDATE", "SET", "DELETE FROM", "CREATE TABLE", "ALTER TABLE", "DROP TABLE", "EXPLAIN",
];

/// The word being typed at the end of `before`, and the qualifier before a
/// dot (`o.na` gives `("o", "na")`).
pub fn current_word(before: &str) -> (Option<&str>, &str) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = before.char_indices().rev().take_while(|&(_, c)| is_word(c)).last().map_or(before.len(), |(i, _)| i);
    let word = &before[start..];
    let head = &before[..start];
    if let Some(head) = head.strip_suffix('.') {
        let qstart = head.char_indices().rev().take_while(|&(_, c)| is_word(c)).last().map_or(head.len(), |(i, _)| i);
        let q = &head[qstart..];
        if !q.is_empty() {
            return (Some(q), word);
        }
    }
    (None, word)
}

/// Tables named after FROM/JOIN/UPDATE/INTO, with their aliases.
fn mentioned<'a>(sql: &str, schema: &'a Schema) -> Vec<(&'a str, Option<String>)> {
    let tokens: Vec<String> = sql
        .split(|c: char| c.is_whitespace() || c == ',' || c == '(' || c == ')' || c == ';')
        .filter(|t| !t.is_empty())
        .map(|t| t.trim_matches(|c| c == '`' || c == '"').to_owned())
        .collect();
    let mut out = Vec::new();
    for (i, t) in tokens.iter().enumerate() {
        let Some(table) = schema.tables.iter().find(|x| x.name.eq_ignore_ascii_case(t)) else { continue };
        let prev = i.checked_sub(1).map(|p| tokens[p].to_ascii_uppercase());
        if !matches!(prev.as_deref(), Some("FROM" | "JOIN" | "UPDATE" | "INTO" | "TABLE")) {
            continue;
        }
        let mut alias = tokens.get(i + 1).cloned();
        if alias.as_deref().is_some_and(|a| a.eq_ignore_ascii_case("AS")) {
            alias = tokens.get(i + 2).cloned();
        }
        let reserved = |a: &str| {
            let u = a.to_ascii_uppercase();
            KEYWORDS.iter().any(|k| k.split(' ').next() == Some(u.as_str()))
                || matches!(u.as_str(), "USING" | "NATURAL" | "CROSS" | "FULL")
        };
        let alias = alias.filter(|a| !reserved(a) && a.chars().all(|c| c.is_alphanumeric() || c == '_'));
        out.push((table.name.as_str(), alias));
    }
    out
}

/// Suggestions for the cursor position: `sql` is the whole statement,
/// `before` the part of it before the cursor.
pub fn suggest(sql: &str, before: &str, schema: &Schema) -> Vec<Suggestion> {
    let (qualifier, word) = current_word(before);
    let lower = word.to_lowercase();
    let starts = |s: &str| s.to_lowercase().starts_with(&lower);
    let tables = mentioned(sql, schema);
    let mut out = Vec::new();
    if let Some(q) = qualifier {
        let target = tables
            .iter()
            .find(|(_, a)| a.as_deref().is_some_and(|a| a.eq_ignore_ascii_case(q)))
            .map(|(t, _)| *t)
            .or_else(|| schema.tables.iter().find(|t| t.name.eq_ignore_ascii_case(q)).map(|t| t.name.as_str()));
        if let Some(t) = target.and_then(|t| schema.table(t)) {
            for c in t.columns.iter().filter(|c| starts(&c.name)) {
                out.push(Suggestion { text: c.name.clone(), kind: SuggestionKind::Column, detail: c.data_type.clone() });
            }
        }
        return out;
    }
    if word.is_empty() {
        return out;
    }
    for (t, _) in &tables {
        if let Some(t) = schema.table(t) {
            for c in t.columns.iter().filter(|c| starts(&c.name)) {
                if !out.iter().any(|s: &Suggestion| s.text == c.name) {
                    out.push(Suggestion {
                        text: c.name.clone(),
                        kind: SuggestionKind::Column,
                        detail: format!("{} · {}", t.name, c.data_type),
                    });
                }
            }
        }
    }
    for t in schema.tables.iter().filter(|t| starts(&t.name)) {
        out.push(Suggestion { text: t.name.clone(), kind: SuggestionKind::Table, detail: String::new() });
    }
    for k in KEYWORDS.iter().filter(|k| starts(k) && !k.eq_ignore_ascii_case(word)) {
        out.push(Suggestion { text: (*k).to_owned(), kind: SuggestionKind::Keyword, detail: String::new() });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::fixture::shop;

    #[test]
    fn words_and_qualifiers() {
        assert_eq!(current_word("SELECT o.cu"), (Some("o"), "cu"));
        assert_eq!(current_word("SELECT na"), (None, "na"));
        assert_eq!(current_word("SELECT o."), (Some("o"), ""));
    }

    #[test]
    fn alias_columns() {
        let s = shop();
        let sql = "SELECT o.cu FROM orders o JOIN customers c ON c.id = o.customer_id";
        let got = suggest(sql, "SELECT o.cu", &s);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].text, "customer_id");
        let got = suggest(sql, "SELECT o.cu FROM orders o JOIN customers c ON c.", &s);
        assert_eq!(got.iter().map(|g| g.text.as_str()).collect::<Vec<_>>(), ["id", "name"]);
    }

    #[test]
    fn tables_columns_and_keywords() {
        let s = shop();
        let got = suggest("SELECT pla FROM orders", "SELECT pla", &s);
        assert_eq!(got[0].text, "placed_at");
        let got = suggest("SELECT * FROM or", "SELECT * FROM or", &s);
        let texts: Vec<_> = got.iter().map(|g| g.text.as_str()).collect();
        assert!(texts.contains(&"orders") && texts.contains(&"order_lines") && texts.contains(&"ORDER BY"), "{texts:?}");
    }
}
