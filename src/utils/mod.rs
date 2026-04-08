//! Reusable, network-free utility helpers.
//!
//! Every module in this tree must include unit tests.
//! All pure logic lives here — network code stays in `find/`.

pub mod dedup;
pub mod parsing;
pub mod slug;
pub mod validation;
