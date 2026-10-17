//! Repository model and orchestration for GitKwaken: refs, history, diffs,
//! git operations and the background session the UI talks to.

pub mod advanced;
pub mod content;
pub mod history;
pub mod ops;
pub mod refs;
mod repo;
pub mod session;

pub use gix::ObjectId;
pub use refs::{Head, Label, RepoState, Refs};
pub use repo::{Repo, init};
pub use session::{Load, Message, MessageKind, Session, SessionOptions, Snapshot};
