//! What Atlas keeps between runs, as JSON in its config folder:
//! connections (never with a password), and per database the positions of
//! the tables, which ones are unfolded, and the SQL console's history.

use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::layout::Point;
use crate::sql::Dialect;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    /// Stable id, also the key of the layout file.
    pub id: String,
    pub name: String,
    pub dialect: Dialect,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    /// A Unix socket instead of host and port, e.g. `/run/mysqld/mysqld.sock`.
    #[serde(default)]
    pub socket: Option<String>,
    #[serde(default)]
    pub ssh: Option<Ssh>,
    /// Opened read-write. Off unless the user turns it on for this
    /// connection.
    #[serde(default)]
    pub writable: bool,
    /// The secret lives in the desktop keyring (Secret portal), never in
    /// this file. Off unless the user turns it on.
    #[serde(default)]
    pub keyring: bool,
}

/// An SSH tunnel through the system's `ssh`, so `~/.ssh/config`, the agent
/// and `known_hosts` apply as on the command line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Ssh {
    /// A host or a `Host` alias from `~/.ssh/config`.
    pub host: String,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
}

impl Connection {
    pub fn default_port(dialect: Dialect) -> u16 {
        match dialect {
            Dialect::MySql => 3306,
            Dialect::Postgres => 5432,
            Dialect::Sqlite => 0,
        }
    }

    pub fn subtitle(&self) -> String {
        let place = match &self.socket {
            Some(s) => s.clone(),
            None => format!("{}:{}", self.host, self.port),
        };
        let via = self.ssh.as_ref().map(|s| format!(" via {}", s.host)).unwrap_or_default();
        format!("{}@{place}/{}{via}", self.user, self.database)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Camera {
    pub x: f64,
    pub y: f64,
    pub zoom: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// Tables the user has placed; the rest is laid out automatically.
    pub positions: BTreeMap<String, Point>,
    pub unfolded: BTreeSet<String>,
    #[serde(default)]
    pub camera: Option<Camera>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    /// Dump and SQLite files opened, newest first.
    pub files: Vec<PathBuf>,
}

pub const HISTORY_MAX: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct History {
    /// Newest first.
    pub queries: Vec<String>,
}

impl History {
    pub fn push(&mut self, sql: &str) {
        let sql = sql.trim();
        if sql.is_empty() {
            return;
        }
        self.queries.retain(|q| q != sql);
        self.queries.insert(0, sql.to_owned());
        self.queries.truncate(HISTORY_MAX);
    }
}

/// A query the user named and kept, per database.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SavedQuery {
    pub name: String,
    pub sql: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub queries: Vec<SavedQuery>,
}

impl Saved {
    /// Adds or replaces the query of that name.
    pub fn put(&mut self, name: &str, sql: &str) {
        let (name, sql) = (name.trim(), sql.trim());
        if name.is_empty() || sql.is_empty() {
            return;
        }
        self.queries.retain(|q| q.name != name);
        self.queries.push(SavedQuery { name: name.to_owned(), sql: sql.to_owned() });
        self.queries.sort_by_key(|q| q.name.to_lowercase());
    }

    pub fn remove(&mut self, name: &str) {
        self.queries.retain(|q| q.name != name);
    }
}

/// The text of the console's open tabs, per database.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tabs {
    pub tabs: Vec<String>,
    #[serde(default)]
    pub selected: usize,
}

/// A file name from any key (a connection id, a file path).
pub fn key_file(key: &str) -> String {
    // FNV-1a: stable across runs and Rust versions, unlike DefaultHasher.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in key.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}.json")
}

pub fn load<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// Writes through a temporary file, so a crash never leaves half a file.
pub fn save<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(value).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_missing_file() {
        let dir = std::env::temp_dir().join(format!("atlas-store-{}", std::process::id()));
        let path = dir.join("layout.json");
        let mut layout = Layout::default();
        layout.positions.insert("t".into(), Point { x: 1.0, y: 2.0 });
        layout.unfolded.insert("t".into());
        save(&path, &layout).unwrap();
        assert_eq!(load::<Layout>(&path), layout);
        assert_eq!(load::<Layout>(&dir.join("none.json")), Layout::default());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn connections_never_hold_a_password() {
        let c = Connection {
            id: "1".into(),
            name: "local".into(),
            dialect: Dialect::MySql,
            host: "localhost".into(),
            port: 3306,
            user: "u".into(),
            database: "d".into(),
            socket: None,
            ssh: None,
            writable: true,
            keyring: true,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert!(!json.contains("password"));
    }

    #[test]
    fn old_connections_stay_read_only() {
        let c: Connection = serde_json::from_str(
            r#"{"id":"1","name":"x","dialect":"MySql","host":"h","port":1,"user":"u","database":"d"}"#,
        )
        .unwrap();
        assert!(!c.writable && !c.keyring);
    }

    #[test]
    fn saved_queries_replace_by_name() {
        let mut s = Saved::default();
        s.put("b", "SELECT 2");
        s.put("a", "SELECT 1");
        s.put("b", "SELECT 3");
        assert_eq!(s.queries.iter().map(|q| (q.name.as_str(), q.sql.as_str())).collect::<Vec<_>>(), [("a", "SELECT 1"), ("b", "SELECT 3")]);
    }

    #[test]
    fn history_moves_repeats_to_the_top() {
        let mut h = History::default();
        h.push("SELECT 1");
        h.push("SELECT 2");
        h.push(" SELECT 1 ");
        assert_eq!(h.queries, ["SELECT 1", "SELECT 2"]);
    }

    #[test]
    fn key_files_are_stable() {
        assert_eq!(key_file("abc"), key_file("abc"));
        assert_ne!(key_file("abc"), key_file("abd"));
    }
}
