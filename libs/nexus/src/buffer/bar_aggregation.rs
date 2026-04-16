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

/// OHLCV bar structure.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub timestamp_ns: u64,
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

/// Aggregator for building bars from tick data.
pub struct BarAggregator {
    instrument_id: InstrumentId,
    period_ns: u64,
    current_bar: Option<Bar>,
}

impl BarAggregator {
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
            if bar.timestamp_ns == bar_start {
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
            timestamp_ns: bar_start,
            open: tick.price_int,
            high: tick.price_int,
            low: tick.price_int,
            close: tick.price_int,
            volume,
            buy_volume: buy_vol,
            sell_volume: sell_vol,
            tick_count: 1,
            instrument_id: self.instrument_id,
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

    pub fn flush(&mut self) -> Option<Bar> {
        self.current_bar.take()
    }

    /// Called by DataEngine on each clock advance.
    /// If the timestamp crosses a bar period boundary and a bar is open,
    /// closes and returns it. This enables clock-driven bar closing
    /// independent of tick arrivals.
    pub fn advance_time(&mut self, timestamp_ns: u64) -> Option<Bar> {
        let bar_start = self.bar_start(timestamp_ns);

        if let Some(ref mut bar) = self.current_bar {
            if bar.timestamp_ns < bar_start {
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
        self.instrument_id
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
