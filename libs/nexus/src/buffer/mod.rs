//! Buffer module — RingBuffer, TickBuffer, BarAggregation, and RingBufferSet.
//!
//! Provides zero-copy TVC file access, pre-decoded tick buffers with VPIN,
//! and OHLCV bar aggregation.

pub mod bar_aggregation;
pub mod buffer_set;
pub mod ring_buffer;
pub mod tick_buffer;

pub use bar_aggregation::{
    Bar, BarAggregator, BarBuilder, BarBuffer, BarIter, BarPeriod,
    AggregationSource, BarAggregation,
    BarSpecification, BarType,
    PriceType, ParseBarTypeError,
    build_bars, build_bars_with_type,
    TimeBarAggregator, TickBarAggregator, VolumeBarAggregator,
    VolumeImbalanceBarAggregator, ValueRunsBarAggregator,
    Aggregator, AggregatorFactory,
};
pub use buffer_set::{MergeCursor, MultiInstrumentEvent, RingBufferSet, TickBufferSet};
pub use ring_buffer::{RingBuffer, RingBufferError, RingIter};
pub use tick_buffer::{TickBuffer, TickBufferError, TickIter, TradeFlowStats};
