//! Repository model and orchestration for GitKwaken: refs, history, diffs,
//! git operations and the background session the UI talks to.

pub mod advanced;
pub mod backup;
pub mod blame;
pub mod cache;
pub mod content;
pub mod history;
pub mod log_filter;
pub mod ops;
mod packwrite;
pub mod pool;
pub mod profiling;
pub mod refs;
mod repo;
pub mod session;
mod stage;

pub use backup::{DISCARD_REF, DiscardBackup, RestoreOutcome};
pub use gix::ObjectId;
pub use refs::{Head, Label, Refs, RepoState};
pub use repo::{Repo, init};
pub use session::{Load, Message, MessageKind, Session, SessionOptions, Snapshot};
