//! Buffer module — RingBuffer, TickBuffer, and RingBufferSet.
//!
//! Provides zero-copy TVC file access and pre-decoded tick buffers with VPIN.
//! OHLCV bar data lives in TVCB (see `tvc::tvcb`); no in-process bar aggregation.

pub mod buffer_set;
pub mod ring_buffer;
pub mod tick_buffer;

pub use buffer_set::{MergeCursor, MultiInstrumentEvent, RingBufferSet, TickBufferSet};
pub use ring_buffer::{RingBuffer, RingBufferError, RingIter};
pub use tick_buffer::{TickBuffer, TickBufferError, TickIter, TradeFlowStats};
