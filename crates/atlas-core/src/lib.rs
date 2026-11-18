//! The parts of Atlas that are plain data and plain functions: the schema
//! model, relation inference, layout, springs, SQL building and checking,
//! dump parsing, Mermaid export and the stored formats. Nothing here touches
//! GTK or a database, so `cargo test -p atlas-core` runs on any machine with
//! a Rust toolchain.

pub mod dump;
pub mod problem;
pub mod guard;
pub mod infer;
pub mod layout;
pub mod mermaid;
pub mod schema;
pub mod spring;
pub mod sql;
pub mod store;
pub mod tokens;
