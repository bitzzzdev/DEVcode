//! Core storage and durable event log for the Rust build. Mirrors the storage
//! half of packages/core: SQLite bootstrap (schema.gen.ts) and the durable
//! per-aggregate log (bus.ts / event/sql.ts). The TUI, tools, providers and
//! projectors are not here yet; see crates/README.md for the port ledger.

pub mod db;
mod event;
mod identifier;
mod initial_schema;

pub use db::{database_path, BootstrapError, Database};
pub use event::{
    append_durable, latest_sequence, read_log, reserve_sequence, AppendError, SerializedEvent,
};
pub use identifier::ascending;
