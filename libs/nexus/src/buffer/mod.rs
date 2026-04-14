//! Buffer module — RingBuffer, TickBuffer, and RingBufferSet.
//!
//! Provides zero-copy TVC file access and pre-decoded tick buffers with VPIN.

pub mod buffer_set;
pub mod ring_buffer;
pub mod tick_buffer;

pub use buffer_set::{InstrumentId, RingBufferSet};
pub use ring_buffer::{RingBuffer, RingBufferError, RingIter};
pub use tick_buffer::{TickBuffer, TickBufferError, TickIter, TradeFlowStats};
