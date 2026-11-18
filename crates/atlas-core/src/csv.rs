//! Reading CSV for the import dialog (RFC 4180, plus `;` and tab as the
//! separators spreadsheets in German locales write). Small enough that a
//! crate is not worth it.

/// The separator that splits the first line into the most fields, of
/// `,` `;` and tab, ignoring separators inside quotes.
pub fn guess_separator(text: &str) -> char {
    let first = first_record(text);
    let count = |sep: char| {
        let mut quoted = false;
        first
            .chars()
            .filter(|&c| {
                if c == '"' {
                    quoted = !quoted;
                }
                !quoted && c == sep
            })
            .count()
    };
    // Ties go to the comma.
    let mut best = (',', count(','));
    for sep in [';', '\t'] {
        let n = count(sep);
        if n > best.1 {
            best = (sep, n);
        }
    }
    best.0
}

fn first_record(text: &str) -> &str {
    let mut quoted = false;
    for (i, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '\n' if !quoted => return &text[..i],
            _ => {}
        }
    }
    text
}

/// All records. Quoted fields may hold separators, quotes (doubled) and
/// line breaks. A byte order mark and a trailing empty line are dropped.
pub fn parse(text: &str, separator: char) -> Vec<Vec<String>> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut records = Vec::new();
    let mut record = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut any = false;
    while let Some(c) = chars.next() {
        any = true;
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            c if c == separator => record.push(std::mem::take(&mut field)),
            '\r' => {}
            '\n' => {
                record.push(std::mem::take(&mut field));
                records.push(std::mem::take(&mut record));
                any = false;
            }
            c => field.push(c),
        }
    }
    if any {
        record.push(field);
        records.push(record);
    }
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_separators_and_line_breaks() {
        let rows = parse("a,b\r\n\"x,y\",\"say \"\"hi\"\"\"\n\"two\nlines\",\n", ',');
        assert_eq!(rows, [vec!["a", "b"], vec!["x,y", "say \"hi\""], vec!["two\nlines", ""]]);
    }

    #[test]
    fn guesses_semicolon_and_tab() {
        assert_eq!(guess_separator("name;\"a,b\";c\n1;2;3"), ';');
        assert_eq!(guess_separator("a\tb\n"), '\t');
        assert_eq!(guess_separator("single"), ',');
    }

    #[test]
    fn bom_and_no_final_newline() {
        assert_eq!(parse("\u{feff}a;b\n1;2", ';'), [vec!["a", "b"], vec!["1", "2"]]);
    }

    /// What `sql::csv` writes, this reads back.
    #[test]
    fn round_trip() {
        let rows = vec![vec![Some("x,\"y\"\nz".to_owned()), None]];
        let text = crate::sql::csv(&["a".into(), "b".into()], &rows);
        assert_eq!(parse(&text, ','), [vec!["a", "b"], vec!["x,\"y\"\nz", ""]]);
    }
}
