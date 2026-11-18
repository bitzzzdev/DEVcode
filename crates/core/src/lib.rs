//! Core storage and durable event log for the Rust build. Mirrors the storage
//! half of packages/core: SQLite bootstrap (schema.gen.ts), the durable
//! per-aggregate log (bus.ts / event/sql.ts), and session creation. The TUI,
//! tools, providers and projectors are not here yet; see crates/README.md.

pub mod db;
mod event;
mod identifier;
mod initial_schema;
mod project;
mod session;

pub use db::{database_path, BootstrapError, Database};
pub use event::{
    append_durable, latest_sequence, read_log, reserve_sequence, AppendError, SerializedEvent,
};
pub use identifier::{ascending, descending};
pub use project::{persist as persist_project, resolve as resolve_project, Resolved};
pub use session::{
    create as create_session, create_for_directory as create_session_for_directory,
    get as get_session, list as list_sessions, CreateSession, ListFilter, SessionRecord,
};
