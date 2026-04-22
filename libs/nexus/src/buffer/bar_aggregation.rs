//! Bar Aggregation — resample tick stream into OHLCV bars.
//!
//! # Overview
//! - `Bar`: OHLCV bar with timestamp, open, high, low, close, volume
//! - `BarAggregator`: rolling window aggregation (1s, 1m, 5m, 15m, 1h, 1d)
//! - `BarBuffer`: collection of pre-aggregated bars
//!
//! # Nautilus Source
//! `data/aggregation.pyx` (bar aggregation logic)
//! `data/wranglers.pyx` (bar types)

use super::tick_buffer::{TickBuffer, TradeFlowStats};
use crate::instrument::InstrumentId;
use serde::{Deserialize, Serialize};

// ============================================================================
// Foundation Enums
// ============================================================================

/// Bar aggregation method — how a bar is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BarAggregation {
    Millisecond,
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
    Tick,
    TickImbalance,
    TickRuns,
    Volume,
    VolumeImbalance,
    VolumeRuns,
    Value,
    ValueImbalance,
    ValueRuns,
    Renko,
}

impl BarAggregation {
    pub fn is_time_aggregated(&self) -> bool {
        matches!(
            self,
            BarAggregation::Millisecond
                | BarAggregation::Second
                | BarAggregation::Minute
                | BarAggregation::Hour
                | BarAggregation::Day
                | BarAggregation::Week
                | BarAggregation::Month
                | BarAggregation::Year
        )
    }
}

/// Which price to use for bar aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PriceType {
    Last,
    Bid,
    Ask,
    Mid,
}

/// Whether bars are aggregated internally or received externally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AggregationSource {
    Internal, // DataEngine aggregates from ticks
    External, // Bars received directly from venue/data provider
}

// ============================================================================
// BarSpecification
// ============================================================================

/// Specification for bar aggregation — step + aggregation method + price type.
/// Matches Nautilus BarSpecification (data.pyx:344).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BarSpecification {
    pub step: u32,
    pub aggregation: BarAggregation,
    pub price_type: PriceType,
}

impl BarSpecification {
    pub fn new(step: u32, aggregation: BarAggregation, price_type: PriceType) -> Self {
        Self {
            step,
            aggregation,
            price_type,
        }
    }

    /// Time-based bar specification.
    pub fn time(step: u32, aggregation: BarAggregation) -> Self {
        Self::new(step, aggregation, PriceType::Last)
    }

    /// Tick-based bar specification.
    pub fn tick(step: u32) -> Self {
        Self::new(step, BarAggregation::Tick, PriceType::Last)
    }

    /// Volume-based bar specification.
    pub fn volume(step: u32) -> Self {
        Self::new(step, BarAggregation::Volume, PriceType::Last)
    }

    /// Value-based bar specification (notional in quote currency).
    pub fn value(step: u64) -> Self {
        Self::new(step as u32, BarAggregation::Value, PriceType::Last)
    }

    /// Returns true if this spec uses time-based aggregation.
    pub fn is_time_aggregated(&self) -> bool {
        self.aggregation.is_time_aggregated()
    }

    /// Returns true if this spec uses threshold-based aggregation (tick/volume/value/imbalance/runs).
    pub fn is_threshold_aggregated(&self) -> bool {
        !self.is_time_aggregated()
    }

    /// Returns interval_ns for time aggregations, None for threshold aggregations.
    pub fn get_interval_ns(&self) -> Option<u64> {
        if self.is_time_aggregated() {
            let mult = match self.aggregation {
                BarAggregation::Millisecond => 1_000_000,
                BarAggregation::Second => 1_000_000_000,
                BarAggregation::Minute => 60_000_000_000,
                BarAggregation::Hour => 3_600_000_000_000,
                BarAggregation::Day => 86_400_000_000_000,
                BarAggregation::Week => 604_800_000_000_000,
                BarAggregation::Month => 2_629_800_000_000_000,
                BarAggregation::Year => 31_557_600_000_000_000,
                _ => return None,
            };
            Some(self.step as u64 * mult)
        } else {
            None
        }
    }
}

// ============================================================================
// ParseBarTypeError
// ============================================================================

/// Error type for BarType parsing failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseBarTypeError(String);

impl std::fmt::Display for ParseBarTypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ParseBarTypeError: {}", self.0)
    }
}

impl std::error::Error for ParseBarTypeError {}

// ============================================================================
// BarType
// ============================================================================

/// Fully-qualified bar type — instrument + specification + aggregation source.
/// Matches Nautilus BarType (data.pyx:1122).
///
/// Storage: symbol + venue strings (NOT InstrumentId hash).
/// Rationale: InstrumentId.as_str() returns "INSTR{:08X}" — hash-only, cannot reconstruct.
/// BarType string format (for compatibility): "BTCUSDT.BINANCE-1m-BETWEEN"
/// or "BTCUSDT.BINANCE-100-TICK-LAST" for non-time bars.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BarType {
    pub symbol: String,
    pub venue: String,
    pub spec: BarSpecification,
    pub aggregation_source: AggregationSource,
}

impl BarType {
    /// Create a new BarType from symbol, venue, spec, and source.
    pub fn new(symbol: &str, venue: &str, spec: BarSpecification, source: AggregationSource) -> Self {
        Self {
            symbol: symbol.to_string(),
            venue: venue.to_string(),
            spec,
            aggregation_source: source,
        }
    }

    /// Create from a string like "BTCUSDT.BINANCE-1m-BETWEEN" or "BTCUSDT.BINANCE-100-TICK-LAST".
    /// Parses the instrument from the prefix and the spec from the suffix.
    pub fn parse_bar_type(s: &str) -> Result<Self, ParseBarTypeError> {
        // Format: "SYMBOL.VENUE-STEP-AGGREGATION-PRICE_TYPE"
        // Example: "BTCUSDT.BINANCE-1-MINUTE-LAST"
        // Example: "BTCUSDT.BINANCE-100-TICK-LAST"
        let parts: Vec<&str> = s.split('-').collect();
        if parts.len() < 3 {
            return Err(ParseBarTypeError(format!(
                "BarType string '{}' must have at least 3 dash-separated parts",
                s
            )));
        }

        // First part is SYMBOL.VENUE
        let instrument_part = parts[0];
        let instrument_parts: Vec<&str> = instrument_part.split('.').collect();
        if instrument_parts.len() != 2 {
            return Err(ParseBarTypeError(format!(
                "BarType string '{}' must have SYMBOL.VENUE format",
                s
            )));
        }
        let symbol = instrument_parts[0].to_string();
        let venue = instrument_parts[1].to_string();

        // Parse step
        let step = parts[1]
            .parse::<u32>()
            .map_err(|_| ParseBarTypeError(format!("Invalid step '{}' in BarType '{}'", parts[1], s)))?;

        // Parse aggregation
        let aggregation = match parts[2].to_uppercase().as_str() {
            "MILLISECOND" => BarAggregation::Millisecond,
            "SECOND" => BarAggregation::Second,
            "MINUTE" => BarAggregation::Minute,
            "HOUR" => BarAggregation::Hour,
            "DAY" => BarAggregation::Day,
            "WEEK" => BarAggregation::Week,
            "MONTH" => BarAggregation::Month,
            "YEAR" => BarAggregation::Year,
            "TICK" => BarAggregation::Tick,
            "TICKIMBALANCE" => BarAggregation::TickImbalance,
            "TICKRUNS" => BarAggregation::TickRuns,
            "VOLUME" => BarAggregation::Volume,
            "VOLUMEIMBALANCE" => BarAggregation::VolumeImbalance,
            "VOLUMERUNS" => BarAggregation::VolumeRuns,
            "VALUE" => BarAggregation::Value,
            "VALUEIMBALANCE" => BarAggregation::ValueImbalance,
            "VALUERUNS" => BarAggregation::ValueRuns,
            "RENKO" => BarAggregation::Renko,
            _ => {
                return Err(ParseBarTypeError(format!(
                    "Unknown aggregation '{}' in BarType '{}'",
                    parts[2], s
                )))
            }
        };

        // Parse price type (default to LAST if not provided)
        let price_type = if parts.len() >= 4 {
            match parts[3].to_uppercase().as_str() {
                "LAST" => PriceType::Last,
                "BID" => PriceType::Bid,
                "ASK" => PriceType::Ask,
                "MID" => PriceType::Mid,
                _ => {
                    return Err(ParseBarTypeError(format!(
                        "Unknown price_type '{}' in BarType '{}'",
                        parts[3], s
                    )))
                }
            }
        } else {
            PriceType::Last
        };

        Ok(Self::new(
            &symbol,
            &venue,
            BarSpecification::new(step, aggregation, price_type),
            AggregationSource::Internal,
        ))
    }

    /// Returns the canonical string representation for map-key compatibility.
    /// Format: "{symbol}.{venue}-{step}-{aggregation}-{price_type}"
    /// Example: "BTCUSDT.BINANCE-1-MINUTE-LAST"
    pub fn as_str(&self) -> String {
        let agg_name = match &self.spec.aggregation {
            BarAggregation::Millisecond => "MILLISECOND",
            BarAggregation::Second => "SECOND",
            BarAggregation::Minute => "MINUTE",
            BarAggregation::Hour => "HOUR",
            BarAggregation::Day => "DAY",
            BarAggregation::Week => "WEEK",
            BarAggregation::Month => "MONTH",
            BarAggregation::Year => "YEAR",
            BarAggregation::Tick => "TICK",
            BarAggregation::TickImbalance => "TICKIMBALANCE",
            BarAggregation::TickRuns => "TICKRUNS",
            BarAggregation::Volume => "VOLUME",
            BarAggregation::VolumeImbalance => "VOLUMEIMBALANCE",
            BarAggregation::VolumeRuns => "VOLUMERUNS",
            BarAggregation::Value => "VALUE",
            BarAggregation::ValueImbalance => "VALUEIMBALANCE",
            BarAggregation::ValueRuns => "VALUERUNS",
            BarAggregation::Renko => "RENKO",
        };
        let price_name = match self.spec.price_type {
            PriceType::Last => "LAST",
            PriceType::Bid => "BID",
            PriceType::Ask => "ASK",
            PriceType::Mid => "MID",
        };
        format!(
            "{}.{}-{}-{}-{}",
            self.symbol, self.venue, self.spec.step, agg_name, price_name
        )
    }

    /// Returns InstrumentId by hashing symbol+venue on demand.
    pub fn instrument_id(&self) -> InstrumentId {
        InstrumentId::new(&self.symbol, &self.venue)
    }

    /// Returns the bar spec for this BarType.
    pub fn spec(&self) -> &BarSpecification {
        &self.spec
    }
}

/// OHLCV bar structure.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    pub ts_event: u64,          // nanoseconds — bar close time (period start)
    pub ts_init: u64,           // nanoseconds — when bar was built
    pub timestamp_ns: u64,      // compatibility alias for ts_event
    pub open: i64,
    pub high: i64,
    pub low: i64,
    pub close: i64,
    pub volume: i64,
    pub buy_volume: i64,
    pub sell_volume: i64,
    pub tick_count: u32,
    pub instrument_id: InstrumentId,
}

// ============================================================================
// BarBuilder
// ============================================================================

/// Shared bar building block for all aggregator types.
/// Tracks open/high/low/close/volume across multiple ticks.
/// Supports sub-bar aggregation (building 1h bars from 1m bars).
#[derive(Debug, Clone)]
pub struct BarBuilder {
    instrument_id: InstrumentId,
    // Internal state
    initialized: bool,
    ts_last: u64,
    count: u32,
    last_close: Option<i64>,
    open: Option<i64>,
    high: Option<i64>,
    low: Option<i64>,
    close: Option<i64>,
    volume: i64,
    buy_volume: i64,
    sell_volume: i64,
}

impl BarBuilder {
    /// Create a new BarBuilder.
    pub fn new(instrument_id: InstrumentId) -> Self {
        Self {
            instrument_id,
            initialized: false,
            ts_last: 0,
            count: 0,
            last_close: None,
            open: None,
            high: None,
            low: None,
            close: None,
            volume: 0,
            buy_volume: 0,
            sell_volume: 0,
        }
    }

    /// Update with a single trade.
    pub fn update(&mut self, price: i64, size: i64, ts_init: u64) {
        self.ts_last = ts_init;
        self.count += 1;

        if !self.initialized {
            self.open = Some(price);
            self.high = Some(price);
            self.low = Some(price);
            self.initialized = true;
        }

        self.close = Some(price);
        if let Some(h) = self.high {
            if price > h {
                self.high = Some(price);
            }
        }
        if let Some(l) = self.low {
            if price < l {
                self.low = Some(price);
            }
        }

        self.volume += size;
    }

    /// Update with a bar (for sub-bar aggregation).
    /// Merges OHLCV fields from an input bar into this builder.
    pub fn update_bar(&mut self, bar: &Bar) {
        self.ts_last = bar.ts_init;
        self.count += bar.tick_count;

        if !self.initialized {
            self.open = Some(bar.open);
            self.high = Some(bar.high);
            self.low = Some(bar.low);
            self.initialized = true;
        } else {
            if let Some(h) = self.high {
                if bar.high > h {
                    self.high = Some(bar.high);
                }
            }
            if let Some(l) = self.low {
                if bar.low < l {
                    self.low = Some(bar.low);
                }
            }
        }

        self.close = Some(bar.close);
        self.volume += bar.volume;
        self.buy_volume += bar.buy_volume;
        self.sell_volume += bar.sell_volume;
    }

    /// Reset the builder state.
    pub fn reset(&mut self) {
        self.initialized = false;
        self.ts_last = 0;
        self.count = 0;
        self.last_close = self.close;
        self.open = None;
        self.high = None;
        self.low = None;
        self.close = None;
        self.volume = 0;
        self.buy_volume = 0;
        self.sell_volume = 0;
    }

    /// Build a bar with the given ts_event.
    /// If no ticks received, uses last_close for all OHLC prices.
    pub fn build(&mut self, ts_event: u64, ts_init: u64) -> Bar {
        let open = self.open.unwrap_or_else(|| self.last_close.unwrap_or(0));
        let high = self.high.unwrap_or_else(|| self.last_close.unwrap_or(0));
        let low = self.low.unwrap_or_else(|| self.last_close.unwrap_or(0));
        let close = self.close.unwrap_or_else(|| self.last_close.unwrap_or(0));

        let bar = Bar {
            ts_event,
            ts_init,
            timestamp_ns: ts_event, // compat
            open,
            high,
            low,
            close,
            volume: self.volume,
            buy_volume: self.buy_volume,
            sell_volume: self.sell_volume,
            tick_count: self.count,
            instrument_id: self.instrument_id.clone(),
        };

        self.reset();
        bar
    }

    /// Build a bar with ts_event = ts_init = last timestamp.
    pub fn build_now(&mut self) -> Bar {
        self.build(self.ts_last, self.ts_last)
    }
}

/// Bar time period in nanoseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BarPeriod {
    OneSecond,
    OneMinute,
    FiveMinutes,
    FifteenMinutes,
    OneHour,
    OneDay,
}

impl BarPeriod {
    pub fn as_ns(&self) -> u64 {
        match self {
            BarPeriod::OneSecond => 1_000_000_000,
            BarPeriod::OneMinute => 60_000_000_000,
            BarPeriod::FiveMinutes => 300_000_000_000,
            BarPeriod::FifteenMinutes => 900_000_000_000,
            BarPeriod::OneHour => 3_600_000_000_000,
            BarPeriod::OneDay => 86_400_000_000_000,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            BarPeriod::OneSecond => "1s",
            BarPeriod::OneMinute => "1m",
            BarPeriod::FiveMinutes => "5m",
            BarPeriod::FifteenMinutes => "15m",
            BarPeriod::OneHour => "1h",
            BarPeriod::OneDay => "1d",
        }
    }
}

/// Time-based bar aggregator.
/// Closes bars when the time period boundary is crossed.
pub struct TimeBarAggregator {
    instrument_id: InstrumentId,
    period_ns: u64,
    current_bar: Option<Bar>,
}

/// Time-based bar aggregator (alias for existing TimeBarAggregator).
pub type BarAggregator = TimeBarAggregator;

impl TimeBarAggregator {
    pub fn new(period: BarPeriod, instrument_id: InstrumentId) -> Self {
        Self {
            instrument_id,
            period_ns: period.as_ns(),
            current_bar: None,
        }
    }

    pub fn with_period_ns(period_ns: u64, instrument_id: InstrumentId) -> Self {
        Self {
            instrument_id,
            period_ns,
            current_bar: None,
        }
    }

    fn bar_start(&self, timestamp_ns: u64) -> u64 {
        (timestamp_ns / self.period_ns) * self.period_ns
    }

    pub fn update(&mut self, tick: &TradeFlowStats) -> Option<Bar> {
        let bar_start = self.bar_start(tick.timestamp_ns);

        if let Some(ref mut bar) = self.current_bar {
            if bar.ts_event == bar_start {
                self.update_existing_bar(tick);
                None
            } else {
                let completed = self.current_bar.take();
                self.start_new_bar(tick, bar_start);
                completed
            }
        } else {
            self.start_new_bar(tick, bar_start);
            None
        }
    }

    fn start_new_bar(&mut self, tick: &TradeFlowStats, bar_start: u64) {
        let volume = tick.size_int;
        let (buy_vol, sell_vol) = if tick.side == 0 {
            (tick.size_int, 0)
        } else {
            (0, tick.size_int)
        };

        self.current_bar = Some(Bar {
            ts_event: bar_start,
            ts_init: tick.timestamp_ns,
            timestamp_ns: bar_start, // compat
            open: tick.price_int,
            high: tick.price_int,
            low: tick.price_int,
            close: tick.price_int,
            volume,
            buy_volume: buy_vol,
            sell_volume: sell_vol,
            tick_count: 1,
            instrument_id: self.instrument_id.clone(),
        });
    }

    fn update_existing_bar(&mut self, tick: &TradeFlowStats) {
        if let Some(ref mut bar) = self.current_bar {
            bar.high = bar.high.max(tick.price_int);
            bar.low = bar.low.min(tick.price_int);
            bar.close = tick.price_int;
            bar.volume += tick.size_int;
            if tick.side == 0 {
                bar.buy_volume += tick.size_int;
            } else {
                bar.sell_volume += tick.size_int;
            }
            bar.tick_count += 1;
        }
    }

    pub fn update_raw(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        let tick = TradeFlowStats {
            price_int: price,
            size_int: size,
            side,
            timestamp_ns,
            cum_buy_volume: 0,
            cum_sell_volume: 0,
            vpin: 0.0,
            bucket_index: 0,
        };
        self.update(&tick)
    }

    pub fn flush(&mut self) -> Option<Bar> {
        self.current_bar.take()
    }

    pub fn bar_type(&self) -> &BarType {
        // TimeBarAggregator uses instrument_id directly, not a BarType
        // Return a dummy for trait compatibility
        unimplemented!("TimeBarAggregator does not have a BarType")
    }

    /// Called by DataEngine on each clock advance.
    /// If the timestamp crosses a bar period boundary and a bar is open,
    /// closes and returns it. This enables clock-driven bar closing
    /// independent of tick arrivals.
    pub fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        let bar_start = self.bar_start(timestamp_ns);

        if let Some(ref mut bar) = self.current_bar {
            if bar.ts_event < bar_start {
                // Clock moved past this bar's period — close it.
                self.current_bar.take()
            } else {
                None
            }
        } else {
            None
        }
    }

    pub fn is_empty(&self) -> bool {
        self.current_bar.is_none()
    }
}

/// Build bars from a TickBuffer.
pub fn build_bars(tick_buffer: &TickBuffer, period: BarPeriod) -> Vec<Bar> {
    let mut aggregator = BarAggregator::new(period, tick_buffer.instrument_id());
    let mut bars = Vec::new();

    for tick in tick_buffer.iter() {
        if let Some(bar) = aggregator.update(tick) {
            bars.push(bar);
        }
    }

    if let Some(bar) = aggregator.flush() {
        bars.push(bar);
    }

    bars
}

// ============================================================================
// TickBarAggregator
// ============================================================================

/// Aggregator that closes bars based on tick count.
/// Closes when tick_count == step.
#[derive(Debug)]
pub struct TickBarAggregator {
    bar_type: BarType,
    builder: BarBuilder,
    step: u32,
}

impl TickBarAggregator {
    pub fn new(bar_type: BarType) -> Self {
        let step = bar_type.spec.step;
        let instrument_id = bar_type.instrument_id();
        Self {
            bar_type,
            builder: BarBuilder::new(instrument_id),
            step,
        }
    }

    pub fn update(&mut self, price: i64, size: i64, _side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.builder.update(price, size, timestamp_ns);
        if self.builder.count == self.step {
            return Some(self.builder.build_now());
        }
        None
    }

    pub fn advance_time(&mut self, _timestamp_ns: u64) -> Option<Bar> {
        // Time-based clock close is a no-op for tick aggregators
        None
    }

    pub fn flush(&mut self) -> Option<Bar> {
        if self.builder.count > 0 {
            Some(self.builder.build_now())
        } else {
            None
        }
    }

    pub fn bar_type(&self) -> &BarType {
        &self.bar_type
    }
}

// ============================================================================
// VolumeBarAggregator
// ============================================================================

/// Aggregator that closes bars based on volume threshold.
/// A single large tick can generate multiple bars via repeated update calls.
#[derive(Debug)]
pub struct VolumeBarAggregator {
    bar_type: BarType,
    builder: BarBuilder,
    step: u32,
    /// Leftover volume from the previous tick that couldn't fit in a bar.
    leftover_volume: i64,
}

impl VolumeBarAggregator {
    pub fn new(bar_type: BarType) -> Self {
        let step = bar_type.spec.step;
        let instrument_id = bar_type.instrument_id();
        Self {
            bar_type,
            builder: BarBuilder::new(instrument_id),
            step,
            leftover_volume: 0,
        }
    }

    pub fn update(&mut self, price: i64, size: i64, _side: u8, timestamp_ns: u64) -> Option<Bar> {
        let total_volume = self.leftover_volume + size;
        let capacity = self.step as i64 - self.builder.volume;

        if total_volume <= capacity {
            // Everything fits in current bar
            self.builder.update(price, total_volume, timestamp_ns);
            self.leftover_volume = 0;
            return None;
        }

        // Fill to threshold, build bar, carry leftover to next call
        self.builder.update(price, capacity, timestamp_ns);
        let bar = self.builder.build_now();
        self.builder.reset();
        self.leftover_volume = total_volume - capacity;
        Some(bar)
    }

    pub fn advance_time(&mut self, _timestamp_ns: u64) -> Option<Bar> {
        None
    }

    pub fn flush(&mut self) -> Option<Bar> {
        if self.builder.count > 0 {
            Some(self.builder.build_now())
        } else {
            None
        }
    }

    pub fn bar_type(&self) -> &BarType {
        &self.bar_type
    }
}

// ============================================================================
// ValueBarAggregator
// ============================================================================

/// Aggregator that closes bars based on notional value threshold.
/// Value is price * size in quote currency.
#[derive(Debug)]
pub struct ValueBarAggregator {
    bar_type: BarType,
    builder: BarBuilder,
    step: u64,   // step is u64 for value (can be large, e.g., 100000 USD)
    cum_value: i128,  // cumulative notional value
}

impl ValueBarAggregator {
    pub fn new(bar_type: BarType) -> Self {
        let step = bar_type.spec.step as u64;
        let instrument_id = bar_type.instrument_id();
        Self {
            bar_type,
            builder: BarBuilder::new(instrument_id),
            step,
            cum_value: 0,
        }
    }

    pub fn update(&mut self, price: i64, size: i64, _side: u8, timestamp_ns: u64) -> Option<Bar> {
        let value_update = (price as i128) * (size as i128);

        if self.cum_value + value_update < self.step as i128 {
            // Below threshold — just update and accumulate
            self.cum_value += value_update;
            self.builder.update(price, size, timestamp_ns);
            None
        } else if self.cum_value == 0 {
            // First tick hits or exceeds threshold
            self.cum_value += value_update;
            self.builder.update(price, size, timestamp_ns);
            // Compute how many bars this creates
            let bars_to_close = self.cum_value / self.step as i128;
            if bars_to_close >= 1 {
                let bar = self.builder.build_now();
                self.cum_value = self.cum_value % self.step as i128;
                return Some(bar);
            }
            None
        } else {
            // Already have accumulated value — compute partial fill
            let value_diff = self.step as i128 - self.cum_value;
            let size_diff = (size as i128) * (value_diff / value_update);
            self.builder.update(price, size_diff as i64, timestamp_ns);
            let bar = self.builder.build_now();
            self.cum_value = value_update - value_diff;
            self.builder.update(price, (size as i128 - size_diff) as i64, timestamp_ns);
            Some(bar)
        }
    }

    pub fn advance_time(&mut self, _timestamp_ns: u64) -> Option<Bar> {
        None
    }

    pub fn flush(&mut self) -> Option<Bar> {
        if self.builder.count > 0 {
            Some(self.builder.build_now())
        } else {
            None
        }
    }

    pub fn bar_type(&self) -> &BarType {
        &self.bar_type
    }
}

// ============================================================================
// TickImbalanceBarAggregator
// ============================================================================

/// Aggregator that closes bars based on tick imbalance threshold.
/// Imbalance = buy_ticks - sell_ticks. Closes when abs(imbalance) >= step.
#[derive(Debug)]
pub struct TickImbalanceBarAggregator {
    bar_type: BarType,
    builder: BarBuilder,
    step: u32,
    imbalance: i32,  // positive = more buys, negative = more sells
}

impl TickImbalanceBarAggregator {
    pub fn new(bar_type: BarType) -> Self {
        let step = bar_type.spec.step;
        let instrument_id = bar_type.instrument_id();
        Self {
            bar_type,
            builder: BarBuilder::new(instrument_id),
            step,
            imbalance: 0,
        }
    }

    pub fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.builder.update(price, size, timestamp_ns);

        // side: 0 = buy, 1 = sell (unknown/mixed = 2 or other)
        if side == 0 {
            self.imbalance += 1;
        } else if side == 1 {
            self.imbalance -= 1;
        }
        // For side >= 2 (unknown/mixed), don't update imbalance

        if self.imbalance.abs() >= self.step as i32 {
            let bar = self.builder.build_now();
            self.imbalance = 0;
            return Some(bar);
        }
        None
    }

    pub fn advance_time(&mut self, _timestamp_ns: u64) -> Option<Bar> {
        None
    }

    pub fn flush(&mut self) -> Option<Bar> {
        if self.builder.count > 0 {
            Some(self.builder.build_now())
        } else {
            None
        }
    }

    pub fn bar_type(&self) -> &BarType {
        &self.bar_type
    }
}

// ============================================================================
// TickRunsBarAggregator
// ============================================================================

/// Aggregator that closes bars based on consecutive same-side ticks (runs).
/// Closes when run_count >= step.
#[derive(Debug)]
pub struct TickRunsBarAggregator {
    bar_type: BarType,
    builder: BarBuilder,
    step: u32,
    current_run_side: Option<u8>,  // None = no current run started
    run_count: u32,
}

impl TickRunsBarAggregator {
    pub fn new(bar_type: BarType) -> Self {
        let step = bar_type.spec.step;
        let instrument_id = bar_type.instrument_id();
        Self {
            bar_type,
            builder: BarBuilder::new(instrument_id),
            step,
            current_run_side: None,
            run_count: 0,
        }
    }

    pub fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.builder.update(price, size, timestamp_ns);

        // For side >= 2 (unknown/mixed), don't count toward runs
        if side >= 2 {
            return None;
        }

        if let Some(current) = self.current_run_side {
            if current == side {
                self.run_count += 1;
            } else {
                // Side changed — reset run
                self.current_run_side = Some(side);
                self.run_count = 1;
            }
        } else {
            // Start a new run
            self.current_run_side = Some(side);
            self.run_count = 1;
        }

        if self.run_count >= self.step {
            let bar = self.builder.build_now();
            self.current_run_side = None;
            self.run_count = 0;
            return Some(bar);
        }
        None
    }

    pub fn advance_time(&mut self, _timestamp_ns: u64) -> Option<Bar> {
        None
    }

    pub fn flush(&mut self) -> Option<Bar> {
        if self.builder.count > 0 {
            Some(self.builder.build_now())
        } else {
            None
        }
    }

    pub fn bar_type(&self) -> &BarType {
        &self.bar_type
    }
}

// ============================================================================
// VolumeImbalanceBarAggregator
// ============================================================================

/// Aggregator that closes bars based on volume imbalance threshold.
/// Closes when abs(buy_volume - sell_volume) >= step.
#[derive(Debug)]
pub struct VolumeImbalanceBarAggregator {
    bar_type: BarType,
    builder: BarBuilder,
    step: u32,
    buy_volume: i64,
    sell_volume: i64,
}

impl VolumeImbalanceBarAggregator {
    pub fn new(bar_type: BarType) -> Self {
        let step = bar_type.spec.step;
        let instrument_id = bar_type.instrument_id();
        Self {
            bar_type,
            builder: BarBuilder::new(instrument_id),
            step,
            buy_volume: 0,
            sell_volume: 0,
        }
    }

    pub fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.builder.update(price, size, timestamp_ns);

        if side == 0 {
            self.buy_volume += size;
        } else if side == 1 {
            self.sell_volume += size;
        }

        let imbalance = (self.buy_volume - self.sell_volume).unsigned_abs() as u32;
        if imbalance >= self.step {
            let bar = self.builder.build_now();
            self.buy_volume = 0;
            self.sell_volume = 0;
            return Some(bar);
        }
        None
    }

    pub fn advance_time(&mut self, _timestamp_ns: u64) -> Option<Bar> {
        None
    }

    pub fn flush(&mut self) -> Option<Bar> {
        if self.builder.count > 0 {
            Some(self.builder.build_now())
        } else {
            None
        }
    }

    pub fn bar_type(&self) -> &BarType {
        &self.bar_type
    }
}

// ============================================================================
// ValueRunsBarAggregator
// ============================================================================

/// Aggregator that closes bars based on consecutive same-side notional value (runs).
/// Closes when cumulative value from same side >= step.
#[derive(Debug)]
pub struct ValueRunsBarAggregator {
    bar_type: BarType,
    builder: BarBuilder,
    step: u64,
    current_run_side: Option<u8>,
    run_value: i128,
}

impl ValueRunsBarAggregator {
    pub fn new(bar_type: BarType) -> Self {
        let step = bar_type.spec.step as u64;
        let instrument_id = bar_type.instrument_id();
        Self {
            bar_type,
            builder: BarBuilder::new(instrument_id),
            step,
            current_run_side: None,
            run_value: 0,
        }
    }

    pub fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.builder.update(price, size, timestamp_ns);

        if side >= 2 {
            // Unknown/mixed side — don't count toward runs
            return None;
        }

        let value = (price as i128) * (size as i128);

        if let Some(current) = self.current_run_side {
            if current == side {
                self.run_value += value;
            } else {
                // Side changed — reset run
                self.current_run_side = Some(side);
                self.run_value = value;
            }
        } else {
            self.current_run_side = Some(side);
            self.run_value = value;
        }

        if self.run_value >= self.step as i128 {
            let bar = self.builder.build_now();
            self.current_run_side = None;
            self.run_value = 0;
            return Some(bar);
        }
        None
    }

    pub fn advance_time(&mut self, _timestamp_ns: u64) -> Option<Bar> {
        None
    }

    pub fn flush(&mut self) -> Option<Bar> {
        if self.builder.count > 0 {
            Some(self.builder.build_now())
        } else {
            None
        }
    }

    pub fn bar_type(&self) -> &BarType {
        &self.bar_type
    }
}

// ============================================================================
// Aggregator Trait
// ============================================================================

/// Trait for bar aggregators (used by DataEngine live path with Box<dyn>).
pub trait Aggregator: Send + Sync {
    fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar>;
    fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar>;
    fn flush(&mut self) -> Option<Bar>;
    fn bar_type(&self) -> &BarType;
}

impl Aggregator for TimeBarAggregator {
    fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.update_raw(price, size, side, timestamp_ns)
    }

    fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        TimeBarAggregator::advance_time(self, timestamp_ns)
    }

    fn flush(&mut self) -> Option<Bar> {
        TimeBarAggregator::flush(self)
    }

    fn bar_type(&self) -> &BarType {
        self.bar_type()
    }
}

impl Aggregator for TickBarAggregator {
    fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.update(price, size, side, timestamp_ns)
    }

    fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        self.advance_time(timestamp_ns)
    }

    fn flush(&mut self) -> Option<Bar> {
        self.flush()
    }

    fn bar_type(&self) -> &BarType {
        self.bar_type()
    }
}

impl Aggregator for VolumeBarAggregator {
    fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.update(price, size, side, timestamp_ns)
    }

    fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        self.advance_time(timestamp_ns)
    }

    fn flush(&mut self) -> Option<Bar> {
        self.flush()
    }

    fn bar_type(&self) -> &BarType {
        self.bar_type()
    }
}

impl Aggregator for ValueBarAggregator {
    fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.update(price, size, side, timestamp_ns)
    }

    fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        self.advance_time(timestamp_ns)
    }

    fn flush(&mut self) -> Option<Bar> {
        self.flush()
    }

    fn bar_type(&self) -> &BarType {
        self.bar_type()
    }
}

impl Aggregator for TickImbalanceBarAggregator {
    fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.update(price, size, side, timestamp_ns)
    }

    fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        self.advance_time(timestamp_ns)
    }

    fn flush(&mut self) -> Option<Bar> {
        self.flush()
    }

    fn bar_type(&self) -> &BarType {
        self.bar_type()
    }
}

impl Aggregator for TickRunsBarAggregator {
    fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.update(price, size, side, timestamp_ns)
    }

    fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        self.advance_time(timestamp_ns)
    }

    fn flush(&mut self) -> Option<Bar> {
        self.flush()
    }

    fn bar_type(&self) -> &BarType {
        self.bar_type()
    }
}

impl Aggregator for VolumeImbalanceBarAggregator {
    fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.update(price, size, side, timestamp_ns)
    }

    fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        self.advance_time(timestamp_ns)
    }

    fn flush(&mut self) -> Option<Bar> {
        self.flush()
    }

    fn bar_type(&self) -> &BarType {
        self.bar_type()
    }
}

impl Aggregator for ValueRunsBarAggregator {
    fn update(&mut self, price: i64, size: i64, side: u8, timestamp_ns: u64) -> Option<Bar> {
        self.update(price, size, side, timestamp_ns)
    }

    fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        self.advance_time(timestamp_ns)
    }

    fn flush(&mut self) -> Option<Bar> {
        self.flush()
    }

    fn bar_type(&self) -> &BarType {
        self.bar_type()
    }
}

// ============================================================================
// AggregatorFactory
// ============================================================================

/// Factory for creating boxed aggregators for the DataEngine live path.
pub struct AggregatorFactory;

impl AggregatorFactory {
    /// Create a boxed aggregator for the DataEngine live path.
    pub fn create(bar_type: BarType) -> Box<dyn Aggregator> {
        match bar_type.spec.aggregation {
            BarAggregation::Millisecond | BarAggregation::Second
            | BarAggregation::Minute | BarAggregation::Hour
            | BarAggregation::Day | BarAggregation::Week
            | BarAggregation::Month | BarAggregation::Year => {
                let period_ns = bar_type.spec.get_interval_ns().unwrap_or(60_000_000_000);
                Box::new(TimeBarAggregator::with_period_ns(period_ns, bar_type.instrument_id()))
            }
            BarAggregation::Tick => Box::new(TickBarAggregator::new(bar_type)),
            BarAggregation::Volume => Box::new(VolumeBarAggregator::new(bar_type)),
            BarAggregation::Value => Box::new(ValueBarAggregator::new(bar_type)),
            BarAggregation::TickImbalance => Box::new(TickImbalanceBarAggregator::new(bar_type)),
            BarAggregation::TickRuns => Box::new(TickRunsBarAggregator::new(bar_type)),
            BarAggregation::VolumeImbalance => Box::new(VolumeImbalanceBarAggregator::new(bar_type)),
            BarAggregation::ValueImbalance | BarAggregation::ValueRuns | BarAggregation::Renko => {
                // Not yet implemented — return time aggregator as fallback
                Box::new(TimeBarAggregator::new(BarPeriod::OneMinute, bar_type.instrument_id()))
            }
            _ => {
                // Default fallback
                Box::new(TimeBarAggregator::new(BarPeriod::OneMinute, bar_type.instrument_id()))
            }
        }
    }
}

// ============================================================================
// build_bars_with_type
// ============================================================================

/// Build bars from a TickBuffer using a BarType (general path, stack-based dispatch).
/// For time-based: uses TimeBarAggregator.
/// For threshold-based: uses the appropriate aggregator.
pub fn build_bars_with_type(tick_buffer: &TickBuffer, bar_type: &BarType) -> Vec<Bar> {
    let spec = &bar_type.spec;
    let instrument_id = bar_type.instrument_id();

    if spec.is_time_aggregated() {
        // Time-based aggregation
        let period_ns = spec.get_interval_ns().unwrap_or(60_000_000_000);
        let mut aggregator = TimeBarAggregator::with_period_ns(period_ns, instrument_id);
        let mut bars = Vec::new();
        for tick in tick_buffer.iter() {
            if let Some(bar) = aggregator.update(tick) {
                bars.push(bar);
            }
        }
        if let Some(bar) = aggregator.flush() {
            bars.push(bar);
        }
        bars
    } else {
        // Threshold-based aggregation
        match spec.aggregation {
            BarAggregation::Tick => {
                let mut agg = TickBarAggregator::new(bar_type.clone());
                let mut bars = Vec::new();
                for tick in tick_buffer.iter() {
                    if let Some(bar) = agg.update(tick.price_int, tick.size_int, tick.side, tick.timestamp_ns) {
                        bars.push(bar);
                    }
                }
                if let Some(bar) = agg.flush() {
                    bars.push(bar);
                }
                bars
            }
            BarAggregation::Volume => {
                let mut agg = VolumeBarAggregator::new(bar_type.clone());
                let mut bars = Vec::new();
                for tick in tick_buffer.iter() {
                    if let Some(bar) = agg.update(tick.price_int, tick.size_int, tick.side, tick.timestamp_ns) {
                        bars.push(bar);
                    }
                }
                if let Some(bar) = agg.flush() {
                    bars.push(bar);
                }
                bars
            }
            BarAggregation::Value => {
                let mut agg = ValueBarAggregator::new(bar_type.clone());
                let mut bars = Vec::new();
                for tick in tick_buffer.iter() {
                    if let Some(bar) = agg.update(tick.price_int, tick.size_int, tick.side, tick.timestamp_ns) {
                        bars.push(bar);
                    }
                }
                if let Some(bar) = agg.flush() {
                    bars.push(bar);
                }
                bars
            }
            BarAggregation::TickImbalance => {
                let mut agg = TickImbalanceBarAggregator::new(bar_type.clone());
                let mut bars = Vec::new();
                for tick in tick_buffer.iter() {
                    if let Some(bar) = agg.update(tick.price_int, tick.size_int, tick.side, tick.timestamp_ns) {
                        bars.push(bar);
                    }
                }
                if let Some(bar) = agg.flush() {
                    bars.push(bar);
                }
                bars
            }
            BarAggregation::TickRuns => {
                let mut agg = TickRunsBarAggregator::new(bar_type.clone());
                let mut bars = Vec::new();
                for tick in tick_buffer.iter() {
                    if let Some(bar) = agg.update(tick.price_int, tick.size_int, tick.side, tick.timestamp_ns) {
                        bars.push(bar);
                    }
                }
                if let Some(bar) = agg.flush() {
                    bars.push(bar);
                }
                bars
            }
            BarAggregation::VolumeImbalance => {
                let mut agg = VolumeImbalanceBarAggregator::new(bar_type.clone());
                let mut bars = Vec::new();
                for tick in tick_buffer.iter() {
                    if let Some(bar) = agg.update(tick.price_int, tick.size_int, tick.side, tick.timestamp_ns) {
                        bars.push(bar);
                    }
                }
                if let Some(bar) = agg.flush() {
                    bars.push(bar);
                }
                bars
            }
            _ => {
                // Not yet implemented — return empty
                Vec::new()
            }
        }
    }
}

/// Pre-computed bar buffer.
#[derive(Debug, Clone)]
pub struct BarBuffer {
    bars: Vec<Bar>,
    period: BarPeriod,
    instrument_id: InstrumentId,
}

impl BarBuffer {
    pub fn from_tick_buffer(tick_buffer: &TickBuffer, period: BarPeriod) -> Self {
        let bars = build_bars(tick_buffer, period);
        Self {
            bars,
            period,
            instrument_id: tick_buffer.instrument_id(),
        }
    }

    pub fn from_bars(bars: Vec<Bar>, period: BarPeriod, instrument_id: InstrumentId) -> Self {
        Self {
            bars,
            period,
            instrument_id,
        }
    }

    pub fn num_bars(&self) -> usize {
        self.bars.len()
    }

    pub fn period(&self) -> BarPeriod {
        self.period
    }

    pub fn instrument_id(&self) -> InstrumentId {
        self.instrument_id.clone()
    }

    pub fn get(&self, index: usize) -> Option<&Bar> {
        self.bars.get(index)
    }

    pub fn iter(&self) -> BarIter<'_> {
        BarIter {
            buffer: self,
            index: 0,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }
}

/// Iterator over bars.
#[derive(Debug, Clone)]
pub struct BarIter<'a> {
    buffer: &'a BarBuffer,
    index: usize,
}

impl<'a> Iterator for BarIter<'a> {
    type Item = &'a Bar;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.buffer.bars.len() {
            let item = &self.buffer.bars[self.index];
            self.index += 1;
            Some(item)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.buffer.bars.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl<'a> ExactSizeIterator for BarIter<'a> {}

/// Aggregation interval in nanoseconds.
pub type AggregationNs = u64;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bar_period_conversions() {
        assert_eq!(BarPeriod::OneSecond.as_ns(), 1_000_000_000);
        assert_eq!(BarPeriod::OneMinute.as_ns(), 60_000_000_000);
        assert_eq!(BarPeriod::FiveMinutes.as_ns(), 300_000_000_000);
        assert_eq!(BarPeriod::OneHour.as_ns(), 3_600_000_000_000);
    }

    #[test]
    fn test_instrument_id_in_buffers() {
        let id1 = InstrumentId::new("BTCUSDT", "BINANCE");
        let id2 = InstrumentId::new("ETHUSDT", "BINANCE");
        assert_ne!(id1, id2);
        assert_eq!(id1, id1);
    }
}
