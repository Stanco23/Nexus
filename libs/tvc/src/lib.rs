//! TVC3 binary tick storage format.
//!
//! TVC3 is a delta-compressed, memory-mapped binary format for high-performance
//! tick storage and random-access playback.
//!
//! ## Format Layout
//! ```text
//! [128-byte header]
//! [anchor tick 30B][delta tick 4B or 15B]...[anchor][delta]...
//!                                             ^ index at EOF
//! [index: 4B num_anchors + (16B * num_anchors)]
//! ```
//!
//! ## Key Types
//! - [`TvcHeader`] — 128-byte file header
//! - [`AnchorTick`] — 30-byte full tick stored every anchor_interval
//! - [`AnchorIndexEntry`] — 16-byte random-access index entry (matches Nautilus production)
//!
//! ## Nautilus Compatibility
//! Index entry is **16 bytes** (tick_index u64 + byte_offset u64), matching Nautilus
//! production loaders (`tvc_cython_loader.pyx` line 57: `INDEX_ENTRY_SIZE = 16`).
//! TVC-OMC used 24 bytes (with redundant timestamp) — Nexus uses 16 for compatibility.

pub mod compression;
pub mod reader;
pub mod types;
pub mod writer;

pub use compression::{
    pack_delta, unpack_anchor_at, unpack_base_delta, unpack_delta_at, unpack_overflow_delta,
};
pub use reader::TvcReader;
pub use types::{AnchorIndexEntry, AnchorTick, DecodedTick, HeaderError, TradeTick, TvcHeader};
pub use writer::{CheckpointWriterError, TvcWriter};
