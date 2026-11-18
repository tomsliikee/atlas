//! What went wrong when connecting, in words a person can act on.
//!
//! sqlx hands back the server's or the operating system's message as text.
//! This sorts the common ones by their codes and phrases (MariaDB/MySQL
//! error numbers, PostgreSQL SQLSTATEs, OS errors), so the window can say
//! "this user may not open database x" instead of `1044 (42000)`.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Problem {
    /// User or password wrong (MySQL 1045, PostgreSQL 28P01/28000).
    Login,
    /// The database does not exist (MySQL 1049, PostgreSQL 3D000).
    NoDatabase(String),
    /// The database exists but this user may not open it (MySQL 1044,
    /// PostgreSQL 42501 on CONNECT).
    NoAccess(String),
    /// The server does not accept this user from this machine (MySQL 1130,
    /// PostgreSQL pg_hba.conf).
    HostRefused,
    /// Nothing listens there (connection refused).
    Unreachable,
    /// The host name does not resolve.
    UnknownHost,
    /// No answer in time.
    Timeout,
    /// The SSH tunnel did not open; the text is ssh's own.
    Ssh,
    Other,
}

impl Problem {
    /// True when choosing another database could help.
    pub fn is_about_database(&self) -> bool {
        matches!(self, Problem::NoDatabase(_) | Problem::NoAccess(_))
    }
}

/// The name in `… database 'name'` or `database "name" …`. The last match
/// counts: sqlx starts its own text with "error returned from database:".
fn quoted_after(text: &str, marker: &str) -> Option<String> {
    let rest = &text[text.rfind(marker)? + marker.len()..];
    let rest = rest.trim_start();
    let quote = rest.chars().next().filter(|c| *c == '\'' || *c == '"')?;
    let inner = &rest[1..];
    Some(inner[..inner.find(quote)?].to_owned())
}

pub fn classify(message: &str) -> Problem {
    let m = message;
    let lower = m.to_lowercase();
    if lower.starts_with("ssh") {
        return Problem::Ssh;
    }
    if m.contains("1044") || lower.contains("permission denied for database") {
        let name = quoted_after(m, "to database").or_else(|| quoted_after(m, "for database")).unwrap_or_default();
        return Problem::NoAccess(name);
    }
    if m.contains("1049") || lower.contains("unknown database") {
        return Problem::NoDatabase(quoted_after(m, "Unknown database").unwrap_or_default());
    }
    if m.contains("3D000") || (lower.contains("database \"") && lower.contains("does not exist")) {
        return Problem::NoDatabase(quoted_after(m, "database").unwrap_or_default());
    }
    if m.contains("1045") || m.contains("28P01") || lower.contains("password authentication failed") || lower.contains("access denied for user") {
        return Problem::Login;
    }
    if m.contains("1130") || lower.contains("pg_hba.conf") || lower.contains("is not allowed to connect") {
        return Problem::HostRefused;
    }
    if lower.contains("connection refused") || lower.contains("os error 111") {
        return Problem::Unreachable;
    }
    if lower.contains("failed to lookup address") || lower.contains("name or service not known") || lower.contains("os error -2") {
        return Problem::UnknownHost;
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return Problem::Timeout;
    }
    Problem::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mariadb_messages() {
        assert_eq!(
            classify("error returned from database: 1044 (42000): Access denied for user 'viewer'@'localhost' to database 'inventory'"),
            Problem::NoAccess("inventory".into())
        );
        assert_eq!(classify("error returned from database: 1049 (42000): Unknown database 'shopx'"), Problem::NoDatabase("shopx".into()));
        assert_eq!(
            classify("error returned from database: 1045 (28000): Access denied for user 'viewer'@'localhost' (using password: YES)"),
            Problem::Login
        );
        assert_eq!(classify("error returned from database: 1130 (HY000): Host '10.0.0.5' is not allowed to connect"), Problem::HostRefused);
    }

    #[test]
    fn postgres_messages() {
        assert_eq!(
            classify("error returned from database: database \"shopx\" does not exist"),
            Problem::NoDatabase("shopx".into())
        );
        assert_eq!(classify("error returned from database: password authentication failed for user \"atlas\""), Problem::Login);
        assert_eq!(
            classify("error returned from database: permission denied for database \"shop\""),
            Problem::NoAccess("shop".into())
        );
        assert_eq!(classify("error returned from database: no pg_hba.conf entry for host \"1.2.3.4\""), Problem::HostRefused);
    }

    #[test]
    fn network_messages() {
        assert_eq!(classify("error communicating with database: Connection refused (os error 111)"), Problem::Unreachable);
        assert_eq!(classify("error communicating with database: failed to lookup address information: Name or service not known"), Problem::UnknownHost);
        assert_eq!(classify("pool timed out while waiting for an open connection"), Problem::Timeout);
        assert_eq!(classify("ssh: Permission denied (publickey)."), Problem::Ssh);
        assert_eq!(classify("something else"), Problem::Other);
        assert!(Problem::NoAccess("x".into()).is_about_database());
        assert!(!Problem::Login.is_about_database());
    }
}
