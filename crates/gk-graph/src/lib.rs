//! Commit-graph layout: date-ordered topological sorting and lane assignment
//! for drawing the history graph. Works on dense `u32` commit indices so the
//! layout of a million commits stays a few tens of megabytes.

mod layout;
mod topo;

pub use layout::{Edge, EdgeKind, Layout, Row};
pub use topo::date_order;

/// Marker for a parent that is not part of the walked set (shallow clones,
/// filtered history).
pub const NONE: u32 = u32::MAX;
