//! Buffer module — RingBuffer, TickBuffer, BarAggregation, and RingBufferSet.
//!
//! Provides zero-copy TVC file access, pre-decoded tick buffers with VPIN,
//! and OHLCV bar aggregation.

pub mod bar_aggregation;
pub mod buffer_set;
pub mod ring_buffer;
pub mod tick_buffer;

pub use bar_aggregation::{Bar, BarAggregator, BarBuffer, BarIter, BarPeriod};
pub use buffer_set::{InstrumentId, RingBufferSet};
pub use ring_buffer::{RingBuffer, RingBufferError, RingIter};
pub use tick_buffer::{TickBuffer, TickBufferError, TickIter, TradeFlowStats};
