//! RingBufferSet — multiple RingBuffers with merged anchor index.
//! TickBufferSet — multi-instrument tick buffer with time-ordered merge cursor.
//!
//! Provides cross-file random access via a merged anchor index built once at startup.
//! Used by TickBufferSet for multi-instrument backtesting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ring_buffer::{RingBuffer, RingBufferError};
use super::tick_buffer::{TickBuffer, TradeFlowStats};
use crate::instrument::InstrumentId;

/// A set of RingBuffers, one per instrument, with a merged anchor index.
///
/// The merged anchor index is built once at startup (O(n log n) where n = total anchors).
/// Per-tick access is pure memory reads via binary search on the merged index.
#[derive(Debug, Clone)]
pub struct RingBufferSet {
    /// Map from instrument ID to RingBuffer.
    buffers: HashMap<InstrumentId, Arc<RingBuffer>>,
    /// Merged anchor index across all files.
    merged_anchors: Vec<MergedAnchor>,
    /// Total tick count across all instruments.
    total_ticks: u64,
}

/// A single entry in the merged anchor index.
///
/// References a specific anchor in a specific file.
#[derive(Debug, Clone)]
pub struct MergedAnchor {
    /// Global tick index across all instruments.
    pub global_tick_index: u64,
    /// Instrument ID for this anchor.
    pub instrument_id: InstrumentId,
    /// Byte offset of this anchor within its file.
    pub byte_offset: u64,
    /// Local tick index within the instrument's file.
    pub local_tick_index: u64,
}

impl RingBufferSet {
    /// Create a new RingBufferSet from a list of (path, instrument_id) pairs.
    pub fn from_files<I>(files: I) -> Result<Self, RingBufferError>
    where
        I: IntoIterator<Item = (PathBuf, InstrumentId)>,
    {
        let mut buffers: HashMap<InstrumentId, Arc<RingBuffer>> = HashMap::new();
        let mut all_anchors: Vec<AnchorInfo> = Vec::new();
        let mut total_ticks: u64 = 0;

        for (path, instrument_id) in files {
            let buffer = Arc::new(RingBuffer::open(&path, instrument_id.clone())?);
            let num_ticks = buffer.num_ticks();

            // Collect anchors from this buffer
            for entry in buffer.anchor_index().iter() {
                all_anchors.push(AnchorInfo {
                    instrument_id: instrument_id.clone(),
                    byte_offset: entry.byte_offset,
                    local_tick_index: entry.tick_index,
                });
            }

            total_ticks += num_ticks;
            buffers.insert(instrument_id, buffer);
        }

        // Sort by instrument_id (u32 id field), then by local_tick_index
        all_anchors.sort_by_key(|a| (a.instrument_id.id, a.local_tick_index));

        // Build merged anchor index - group by instrument
        let mut merged_anchors: Vec<MergedAnchor> = Vec::new();
        let mut current_instrument: Option<InstrumentId> = None;
        let mut global_offset: u64 = 0;
        let mut instrument_start_tick: u64 = 0;

        for anchor in &all_anchors {
            if Some(&anchor.instrument_id) != current_instrument.as_ref() {
                // New instrument - update global offset
                current_instrument = Some(anchor.instrument_id.clone());
                if let Some(buffer) = buffers.get(&anchor.instrument_id) {
                    // Ticks from this instrument start at current global offset
                    instrument_start_tick = global_offset;
                    global_offset += buffer.num_ticks();
                }
            }

            merged_anchors.push(MergedAnchor {
                global_tick_index: instrument_start_tick + anchor.local_tick_index,
                instrument_id: anchor.instrument_id.clone(),
                byte_offset: anchor.byte_offset,
                local_tick_index: anchor.local_tick_index,
            });
        }

        // Sort by global tick index for binary search
        merged_anchors.sort_by_key(|a| a.global_tick_index);

        Ok(Self {
            buffers,
            merged_anchors,
            total_ticks,
        })
    }

    /// Create a RingBufferSet for a single instrument from one file.
    pub fn single(path: &Path, instrument_id: InstrumentId) -> Result<Self, RingBufferError> {
        Self::from_files([(path.to_path_buf(), instrument_id)])
    }

    /// Get the number of instruments.
    pub fn num_instruments(&self) -> usize {
        self.buffers.len()
    }

    /// Get the total tick count across all instruments.
    pub fn total_ticks(&self) -> u64 {
        self.total_ticks
    }

    /// Get the number of merged anchors.
    pub fn num_anchors(&self) -> usize {
        self.merged_anchors.len()
    }

    /// Get a reference to a specific instrument's RingBuffer.
    pub fn get(&self, instrument_id: &InstrumentId) -> Option<&Arc<RingBuffer>> {
        self.buffers.get(instrument_id)
    }

    /// Get the merged anchor index.
    pub fn merged_anchors(&self) -> &[MergedAnchor] {
        &self.merged_anchors
    }

    /// Binary search to find the merged anchor for a given global tick index.
    /// Returns the merged anchor entry and the buffer for that instrument.
    pub fn seek_to_global_tick(
        &self,
        global_tick_index: u64,
    ) -> Option<(&MergedAnchor, &RingBuffer)> {
        let mut left = 0;
        let mut right = self.merged_anchors.len();

        while left < right {
            let mid = (left + right) / 2;
            if self.merged_anchors[mid].global_tick_index <= global_tick_index {
                left = mid + 1;
            } else {
                right = mid;
            }
        }

        if left == 0 {
            return None;
        }

        let anchor = &self.merged_anchors[left - 1];
        let buffer = self.buffers.get(&anchor.instrument_id)?;
        Some((anchor, buffer))
    }

    /// Get all instrument IDs.
    pub fn instrument_ids(&self) -> Vec<InstrumentId> {
        self.buffers.keys().cloned().collect()
    }

    /// Iterate all instruments' buffers.
    pub fn buffers(&self) -> &HashMap<InstrumentId, Arc<RingBuffer>> {
        &self.buffers
    }
}

// =============================================================================
// TickBufferSet
// =============================================================================

/// A set of TickBuffers, one per instrument, with time-ordered merge cursor.
///
/// Used for multi-instrument backtesting where ticks from different instruments
/// need to be delivered in time-order regardless of which instrument they belong to.
#[derive(Debug, Clone)]
pub struct TickBufferSet {
    buffers: HashMap<InstrumentId, Arc<TickBuffer>>,
    instrument_ids: Vec<InstrumentId>,
}

impl TickBufferSet {
    /// Create a TickBufferSet from a list of (path, instrument_id) pairs.
    ///
    /// Each file is opened as a RingBuffer, decoded to a TickBuffer, and stored.
    pub fn from_files<I>(files: I) -> Result<Self, RingBufferError>
    where
        I: IntoIterator<Item = (PathBuf, InstrumentId)>,
    {
        let mut buffers: HashMap<InstrumentId, Arc<TickBuffer>> = HashMap::new();
        let mut instrument_ids: Vec<InstrumentId> = Vec::new();

        for (path, instrument_id) in files {
            let rb = RingBuffer::open(&path, instrument_id.clone())?;
            let tb = TickBuffer::from_ring_buffer(&rb, 50)
                .map_err(|e| RingBufferError::InvalidHeader(e.to_string()))?;
            buffers.insert(instrument_id.clone(), Arc::new(tb));
            instrument_ids.push(instrument_id);
        }

        Ok(Self {
            buffers,
            instrument_ids,
        })
    }

    /// Create a TickBufferSet from pre-built RingBuffers.
    pub fn from_ring_buffers<I>(ring_buffers: I, num_buckets: u32) -> Result<Self, RingBufferError>
    where
        I: IntoIterator<Item = (InstrumentId, RingBuffer)>,
    {
        let mut buffers: HashMap<InstrumentId, Arc<TickBuffer>> = HashMap::new();
        let mut instrument_ids: Vec<InstrumentId> = Vec::new();

        for (instrument_id, rb) in ring_buffers {
            let tb = TickBuffer::from_ring_buffer(&rb, num_buckets)
                .map_err(|e| RingBufferError::InvalidHeader(e.to_string()))?;
            buffers.insert(instrument_id.clone(), Arc::new(tb));
            instrument_ids.push(instrument_id);
        }

        Ok(Self {
            buffers,
            instrument_ids,
        })
    }

    /// Get the number of instruments.
    pub fn num_instruments(&self) -> usize {
        self.buffers.len()
    }

    /// Get all instrument IDs.
    pub fn instrument_ids(&self) -> &[InstrumentId] {
        &self.instrument_ids
    }

    /// Get a reference to a specific instrument's TickBuffer.
    pub fn get(&self, instrument_id: &InstrumentId) -> Option<&Arc<TickBuffer>> {
        self.buffers.get(instrument_id)
    }

    /// Create a merge cursor for time-ordered iteration across all instruments.
    pub fn merge_cursor(&self) -> MergeCursor<'_> {
        MergeCursor::new(self)
    }
}

/// A tick event from a specific instrument in a multi-instrument context.
#[derive(Debug, Clone)]
pub struct MultiInstrumentEvent<'a> {
    pub instrument_id: InstrumentId,
    pub tick: &'a TradeFlowStats,
}

/// Merge cursor for time-ordered iteration across multiple instrument buffers.
#[derive(Debug)]
pub struct MergeCursor<'a> {
    #[allow(dead_code)]
    buffer_set: &'a TickBufferSet,
    iterators: Vec<MergeState<'a>>,
    current_event: Option<MultiInstrumentEvent<'a>>,
}

#[derive(Debug)]
struct MergeState<'a> {
    instrument_id: InstrumentId,
    buffer: &'a TickBuffer,
    next_index: usize,
}

impl<'a> MergeCursor<'a> {
    fn new(buffer_set: &'a TickBufferSet) -> Self {
        let mut iterators = Vec::new();

        for instrument_id in buffer_set.instrument_ids() {
            if let Some(tb) = buffer_set.get(&instrument_id) {
                iterators.push(MergeState {
                    instrument_id: instrument_id.clone(),
                    buffer: tb,
                    next_index: 0,
                });
            }
        }

        let mut cursor = Self {
            buffer_set,
            iterators,
            current_event: None,
        };
        cursor.find_next();
        cursor
    }

    fn find_next(&mut self) {
        let mut earliest_ts: u64 = u64::MAX;
        let mut earliest_idx: Option<usize> = None;

        // Find the earliest tick across all instruments
        for (i, state) in self.iterators.iter_mut().enumerate() {
            if let Some(tick) = state.buffer.get(state.next_index) {
                if tick.timestamp_ns < earliest_ts {
                    earliest_ts = tick.timestamp_ns;
                    earliest_idx = Some(i);
                }
            }
        }

        if let Some(idx) = earliest_idx {
            let state = &mut self.iterators[idx];
            if let Some(tick) = state.buffer.get(state.next_index) {
                self.current_event = Some(MultiInstrumentEvent {
                    instrument_id: state.instrument_id.clone(),
                    tick,
                });
                state.next_index += 1;
                return;
            }
        }

        self.current_event = None;
    }

    /// Get the current event without advancing.
    pub fn peek(&self) -> Option<&MultiInstrumentEvent<'a>> {
        self.current_event.as_ref()
    }

    /// Advance to the next event.
    pub fn advance(&mut self) -> Option<MultiInstrumentEvent<'a>> {
        let result = self.current_event.take();
        self.find_next();
        result
    }

    /// Check if there are more events.
    pub fn has_next(&self) -> bool {
        self.current_event.is_some()
    }
}

impl<'a> Iterator for MergeCursor<'a> {
    type Item = MultiInstrumentEvent<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let result = self.current_event.take();
        self.find_next();
        result
    }
}

/// Internal anchor info before merging.
struct AnchorInfo {
    instrument_id: InstrumentId,
    byte_offset: u64,
    local_tick_index: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer_set_empty() {
        let set = RingBufferSet::from_files(std::iter::empty());
        assert!(set.is_ok());
        assert_eq!(set.unwrap().num_instruments(), 0);
    }
}
