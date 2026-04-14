//! Ring buffer — zero-copy TVC file access with binary search.
//!
//! Provides single-file random access via memory-mapped files, and
//! sequential iteration over decoded TradeTicks.

pub mod buffer_set;
pub mod ring_buffer;

pub use buffer_set::{InstrumentId, RingBufferSet};
pub use ring_buffer::{RingBuffer, RingBufferError, RingIter};
