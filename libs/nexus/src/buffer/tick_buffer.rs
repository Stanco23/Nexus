//! TickBuffer — pre-decoded tick data with VPIN bucketing.
//!
//! # Overview
//! - `TradeFlowStats`: pre-decoded tick with cumulative volume tracking and VPIN
//! - `TickBuffer`: decodes RingBuffer once, stores VPIN-bucketed data
//! - `Arc<TickBuffer>` shared across rayon workers for zero-copy sweeps
//!
//! # VPIN Formula
//! VPIN = |cum_buy - cum_sell| / (cum_buy + cum_sell)
//! Computed per bucket at bucket boundaries.
//!
//! # Nautilus Source
//! `backtest/engine.pyx` (VPIN concepts)

use super::ring_buffer::RingBuffer;
use crate::buffer::buffer_set::InstrumentId;

/// Pre-decoded trade tick with VPIN and cumulative volume tracking.
///
/// Stored in TickBuffer, indexed by tick position.
#[derive(Debug, Clone)]
pub struct TradeFlowStats {
    pub timestamp_ns: u64,
    pub price_int: i64,
    pub size_int: i64,
    pub side: u8,
    pub cum_buy_volume: i64,
    pub cum_sell_volume: i64,
    pub vpin: f64,
    pub bucket_index: u32,
}

/// Errors for TickBuffer operations.
#[derive(Debug)]
pub enum TickBufferError {
    RingBuffer(String),
    EmptyBuffer,
}

impl std::fmt::Display for TickBufferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TickBufferError::RingBuffer(s) => write!(f, "RingBuffer error: {}", s),
            TickBufferError::EmptyBuffer => write!(f, "Buffer has no ticks"),
        }
    }
}

impl std::error::Error for TickBufferError {}

impl From<crate::buffer::ring_buffer::RingBufferError> for TickBufferError {
    fn from(e: crate::buffer::ring_buffer::RingBufferError) -> Self {
        TickBufferError::RingBuffer(e.to_string())
    }
}

// =============================================================================
// TickBuffer
// =============================================================================

/// Pre-decoded tick buffer with VPIN bucketing.
///
/// Decodes a RingBuffer once and stores all ticks with VPIN metadata.
/// Shared across rayon workers via `Arc<TickBuffer>` for zero-copy sweeps.
#[derive(Debug, Clone)]
pub struct TickBuffer {
    stats: Vec<TradeFlowStats>,
    bucket_vpins: Vec<f64>,
    bucket_size: usize,
    num_buckets: u32,
    instrument_id: InstrumentId,
}

impl TickBuffer {
    /// Build a TickBuffer from a RingBuffer with VPIN bucketing.
    ///
    /// Decodes all ticks from the RingBuffer, tracking cumulative buy/sell volumes
    /// and computing VPIN at each bucket boundary.
    ///
    /// # Arguments
    /// * `rb` - Source RingBuffer to decode
    /// * `num_buckets` - Number of VPIN buckets (default 50)
    pub fn from_ring_buffer(rb: &RingBuffer, num_buckets: u32) -> Result<Self, TickBufferError> {
        let total_ticks = rb.num_ticks() as usize;
        if total_ticks == 0 {
            return Err(TickBufferError::EmptyBuffer);
        }

        let bucket_size = (total_ticks / num_buckets as usize).max(1);
        let mut stats = Vec::with_capacity(total_ticks);
        let mut bucket_vpins = Vec::with_capacity(num_buckets as usize);

        let mut cum_buy: i64 = 0;
        let mut cum_sell: i64 = 0;

        for (tick_idx, tick) in rb.iter().enumerate() {
            if tick.side == 0 {
                cum_buy += tick.size_int;
            } else {
                cum_sell += tick.size_int;
            }

            let bucket_index = (tick_idx / bucket_size) as u32;
            let total_vol = cum_buy + cum_sell;
            let vpin = if total_vol > 0 {
                (cum_buy - cum_sell).abs() as f64 / total_vol as f64
            } else {
                0.0
            };

            stats.push(TradeFlowStats {
                timestamp_ns: tick.timestamp_ns,
                price_int: tick.price_int,
                size_int: tick.size_int,
                side: tick.side,
                cum_buy_volume: cum_buy,
                cum_sell_volume: cum_sell,
                vpin,
                bucket_index,
            });

            if (tick_idx + 1) % bucket_size == 0 {
                bucket_vpins.push(vpin);
            }
        }

        let num_buckets_computed = bucket_vpins.len() as u32;

        Ok(Self {
            stats,
            bucket_vpins,
            bucket_size,
            num_buckets: num_buckets_computed,
            instrument_id: rb.instrument_id(),
        })
    }

    /// Get the number of ticks in this buffer.
    pub fn num_ticks(&self) -> u64 {
        self.stats.len() as u64
    }

    /// Get the instrument ID for this buffer.
    pub fn instrument_id(&self) -> InstrumentId {
        self.instrument_id
    }

    /// Get the number of buckets.
    pub fn num_buckets(&self) -> u32 {
        self.num_buckets
    }

    /// Get the bucket size (ticks per bucket).
    pub fn bucket_size(&self) -> usize {
        self.bucket_size
    }

    /// Get VPIN for a specific bucket.
    ///
    /// Returns VPIN value at the time the bucket was closed.
    /// Bucket 0 is the first bucket (ticks 0 to bucket_size-1).
    pub fn bucket_vpin(&self, bucket_idx: usize) -> Option<f64> {
        self.bucket_vpins.get(bucket_idx).copied()
    }

    /// Get VPIN at a specific timestamp.
    ///
    /// Returns the VPIN value for the bucket containing the given timestamp.
    pub fn vpin_at_timestamp(&self, ts: u64) -> Option<f64> {
        self.stats
            .binary_search_by(|s| s.timestamp_ns.cmp(&ts))
            .ok()
            .map(|idx| self.stats[idx].vpin)
    }

    /// Iterate over all TradeFlowStats.
    pub fn iter(&self) -> TickIter<'_> {
        TickIter {
            buffer: self,
            index: 0,
        }
    }

    /// Get a tick by index.
    pub fn get(&self, index: usize) -> Option<&TradeFlowStats> {
        self.stats.get(index)
    }
}

/// Iterator over TradeFlowStats in a TickBuffer.
#[derive(Debug, Clone)]
pub struct TickIter<'a> {
    buffer: &'a TickBuffer,
    index: usize,
}

impl<'a> Iterator for TickIter<'a> {
    type Item = &'a TradeFlowStats;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index < self.buffer.stats.len() {
            let item = &self.buffer.stats[self.index];
            self.index += 1;
            Some(item)
        } else {
            None
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.buffer.stats.len() - self.index;
        (remaining, Some(remaining))
    }
}

impl<'a> ExactSizeIterator for TickIter<'a> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_instrument_id_type() {
        let id: InstrumentId = 12345u64;
        assert_eq!(id, 12345);
    }
}
