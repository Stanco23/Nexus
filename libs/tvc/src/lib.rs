//! TVC3 binary tick storage format.
//!
//! TVC3 is a delta-compressed, memory-mapped binary format for high-performance
//! tick storage and random-access playback.
//!
//! ## Format Layout
//! ```
//! [128-byte header]
//! [anchor tick 30B][delta tick 4B or 15B]...[anchor][delta]...
//!                                             ↑ index at EOF
//! [index: 4B num_anchors + (16B × num_anchors)]
//! ```
//!
//! ## Key Types
//! - [`TvcHeader`] — 128-byte file header
//! - [`AnchorTick`] — 30-byte full tick stored every anchor_interval
//! - [`AnchorIndexEntry`] — 16-byte random-access index entry

pub mod types;
pub mod compression;
pub mod reader;
pub mod writer;

pub use types::{TvcHeader, AnchorTick, AnchorIndexEntry};
pub use compression::{pack_delta, unpack_delta};
pub use reader::TvcReader;
pub use writer::TvcWriter;
