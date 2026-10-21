//! The vocabulary shared by the loop and its hosts.
//!
//! No I/O and no execution: a tool pack, a UI, or an embedder can depend on this
//! without dragging the loop or the enforcement layer along.

pub mod error;
pub mod event;
pub mod message;
pub mod notice;
pub mod policy;
pub mod scope;
pub mod tool;
pub mod trace;