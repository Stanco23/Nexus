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
use tvc::TradeTick;

/// A set of RingBuffers, one per instrument, with a merged anchor index.
///
/// The merged anchor index is built once at startup (O(n log n) where n = total anchors).
/// Per-tick access is pure memory reads via binary search on the merged index.
///
/// Stores ALL buffers in a Vec (not HashMap) to support multi-file same-instrument
/// scenarios like backtesting across multiple daily files for one instrument.
#[derive(Debug, Clone)]
pub struct RingBufferSet {
    /// All buffers, kept in order. Vec to preserve all files (HashMap overwrote).
    buffers: Vec<(InstrumentId, Arc<RingBuffer>)>,
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
    /// Local tick index within the buffer (not the same as global_tick_index).
    pub local_tick_index: u64,
    /// Buffer index for O(1) buffer lookup (vs instrument_id lookup which only gets first buffer).
    pub buffer_idx: usize,
}

impl RingBufferSet {
    /// Create a new RingBufferSet from a list of (path, instrument_id) pairs.
    pub fn from_files<I>(files: I) -> Result<Self, RingBufferError>
    where
        I: IntoIterator<Item = (PathBuf, InstrumentId)>,
    {
        // Vec to keep ALL buffers — HashMap overwrote same-instrument files.
        let mut buffers: Vec<(InstrumentId, Arc<RingBuffer>)> = Vec::new();
        let mut all_anchors: Vec<AnchorInfo> = Vec::new();
        let mut total_ticks: u64 = 0;

        for (path, instrument_id) in files {
            let buffer = Arc::new(RingBuffer::open(&path, instrument_id.clone())?);
            let num_ticks = buffer.num_ticks();
            let buffer_idx = buffers.len(); // Capture index BEFORE push

            // Collect anchors from this buffer
            for entry in buffer.anchor_index().iter() {
                all_anchors.push(AnchorInfo {
                    instrument_id: instrument_id.clone(),
                    byte_offset: entry.byte_offset,
                    local_tick_index: entry.tick_index,
                    buffer_idx,
                });
            }

            total_ticks += num_ticks;
            // Push, don't insert — we need all files regardless of instrument_id
            buffers.push((instrument_id, buffer));
        }

        // Sort by buffer_idx then local_tick_index — this is the ONLY way to ensure
        // correct merged_anchors when same-instrument multi-file has varying tick counts.
        // The previous sort by (instrument_id.id, local_tick_index) broke global_offset
        // assignment when instrument_id.id values didn't match file loading order.
        all_anchors.sort_by_key(|a| (a.buffer_idx, a.local_tick_index));

        // Build merged anchor index - one global tick per anchor across ALL buffers
        // Track by buffer index since same-instrument multi-file has same instrument_id
        let mut merged_anchors: Vec<MergedAnchor> = Vec::new();
        let mut current_buffer_idx: Option<usize> = None;
        let mut global_offset: u64 = 0;

        for anchor in &all_anchors {
            if current_buffer_idx != Some(anchor.buffer_idx) {
                // New buffer — advance global offset by previous buffer's ticks
                if let Some(old_idx) = current_buffer_idx {
                    global_offset += buffers[old_idx].1.num_ticks();
                }
                current_buffer_idx = Some(anchor.buffer_idx);
            }

            merged_anchors.push(MergedAnchor {
                global_tick_index: global_offset + anchor.local_tick_index,
                instrument_id: anchor.instrument_id.clone(),
                byte_offset: anchor.byte_offset,
                local_tick_index: anchor.local_tick_index,
                buffer_idx: anchor.buffer_idx,
            });
        }

        // Add final buffer's tick count
        if let Some(last_idx) = current_buffer_idx {
            global_offset += buffers[last_idx].1.num_ticks();
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
    /// Returns the first buffer with matching instrument_id (Vec scan — O(n), but n is small).
    pub fn get(&self, instrument_id: &InstrumentId) -> Option<&Arc<RingBuffer>> {
        self.buffers
            .iter()
            .find(|(id, _)| id == instrument_id)
            .map(|(_, buf)| buf)
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
        // Use buffer_idx directly for O(1) lookup instead of get() which only returns first buffer
        let buffer = self.buffers.get(anchor.buffer_idx).map(|(_, b)| b)?;
        Some((anchor, buffer))
    }

    /// Get iterator state for starting iteration from a global tick index.
    /// Returns (buffer, byte_offset, local_tick_index, anchor_slot) for creating a RingIter.
    /// This streams ALL ticks from that anchor forward (anchors + deltas).
    pub fn iter_state_from_global_tick(
        &self,
        global_tick_index: u64,
    ) -> Option<(&RingBuffer, usize, u64, usize)> {
        let anchor = self.merged_anchors.get(global_tick_index as usize)?;
        let buffer = self.buffers.get(anchor.buffer_idx)?.1.as_ref();

        // Compute anchor_slot: which index into buffer's anchor_index corresponds to this anchor
        let byte_offset = anchor.byte_offset as u64;
        let anchor_idx = buffer.anchor_index();
        let anchor_slot = anchor_idx.iter().position(|e| e.byte_offset == byte_offset).unwrap_or(0);

        Some((buffer, anchor.byte_offset as usize, anchor.local_tick_index, anchor_slot))
    }

    /// Get all unique instrument IDs in order of first occurrence.
    pub fn instrument_ids(&self) -> Vec<InstrumentId> {
        let mut seen = HashMap::new();
        let mut ids = Vec::new();
        for (id, _) in &self.buffers {
            if !seen.contains_key(id) {
                seen.insert(id.clone(), ());
                ids.push(id.clone());
            }
        }
        ids
    }

    /// Iterate all buffers.
    pub fn buffers(&self) -> &[(InstrumentId, Arc<RingBuffer>)] {
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
    /// Vec of (instrument_id, tick_buffer) to preserve ALL files.
    /// HashMap would overwrite same-instrument files — Vec keeps them all.
    buffers: Vec<(InstrumentId, Arc<TickBuffer>)>,
    /// Deduplicated instrument IDs for iteration.
    instrument_ids: Vec<InstrumentId>,
}

impl TickBufferSet {
    /// Create a TickBufferSet from a list of (path, instrument_id) pairs.
    ///
    /// Each file is opened as a RingBuffer, decoded to a TickBuffer, and stored.
    /// Vec-based storage preserves all files for same-instrument multi-file backtests.
    pub fn from_files<I>(files: I) -> Result<Self, RingBufferError>
    where
        I: IntoIterator<Item = (PathBuf, InstrumentId)>,
    {
        let mut files_vec: Vec<(PathBuf, InstrumentId)> = files.into_iter().collect();
        let mut instrument_ids: Vec<InstrumentId> = Vec::new();
        let mut buffers_vec: Vec<(InstrumentId, Arc<TickBuffer>)> = Vec::new();

        for (path, instrument_id) in files_vec.drain(..) {
            let rb = RingBuffer::open(&path, instrument_id.clone())?;
            let tb = TickBuffer::from_ring_buffer(&rb, 50)
                .map_err(|e| RingBufferError::InvalidHeader(e.to_string()))?;
            // Vec keeps all buffers — dedup instrument_ids only
            if !buffers_vec.iter().any(|(id, _)| id == &instrument_id) {
                instrument_ids.push(instrument_id.clone());
            }
            buffers_vec.push((instrument_id, Arc::new(tb)));
        }

        Ok(Self {
            buffers: buffers_vec,
            instrument_ids,
        })
    }

    /// Create a TickBufferSet from pre-built RingBuffers.
    pub fn from_ring_buffers<I>(ring_buffers: I, num_buckets: u32) -> Result<Self, RingBufferError>
    where
        I: IntoIterator<Item = (InstrumentId, RingBuffer)>,
    {
        let mut buffers_vec: Vec<(InstrumentId, Arc<TickBuffer>)> = Vec::new();
        let mut instrument_ids: Vec<InstrumentId> = Vec::new();

        for (instrument_id, rb) in ring_buffers {
            let tb = TickBuffer::from_ring_buffer(&rb, num_buckets)
                .map_err(|e| RingBufferError::InvalidHeader(e.to_string()))?;
            if !buffers_vec.iter().any(|(id, _)| id == &instrument_id) {
                instrument_ids.push(instrument_id.clone());
            }
            buffers_vec.push((instrument_id, Arc::new(tb)));
        }

        Ok(Self {
            buffers: buffers_vec,
            instrument_ids,
        })
    }

    /// Get the number of instruments (deduplicated).
    pub fn num_instruments(&self) -> usize {
        self.instrument_ids.len()
    }

    /// Get all instrument IDs.
    pub fn instrument_ids(&self) -> &[InstrumentId] {
        &self.instrument_ids
    }

    /// Get the total tick count across all instruments.
    pub fn total_ticks(&self) -> u64 {
        self.buffers.iter().map(|(_, b)| b.num_ticks()).sum()
    }

    /// Get a reference to a specific instrument's TickBuffer.
    /// Returns the first buffer with matching instrument_id.
    pub fn get(&self, instrument_id: &InstrumentId) -> Option<&Arc<TickBuffer>> {
        self.buffers
            .iter()
            .find(|(id, _)| id == instrument_id)
            .map(|(_, buf)| buf)
    }

    /// Get all buffers as a slice.
    pub fn buffers(&self) -> &[(InstrumentId, Arc<TickBuffer>)] {
        &self.buffers
    }

    /// Create a merge cursor for time-ordered iteration across all instruments.
    pub fn merge_cursor(&self) -> MergeCursor<'_> {
        MergeCursor::new(self)
    }
}

/// A tick event from a specific instrument in a multi-instrument context.
/// InstrumentId is cloned once per instrument boundary, not per tick.
#[derive(Debug)]
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
            if let Some(tb) = buffer_set.get(instrument_id) {
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

    /// Resolve an instrument index to the actual InstrumentId.
    /// The InstrumentId is cloned — only use this at instrument boundaries, not per tick.
    pub fn instrument_id(&self, idx: usize) -> InstrumentId {
        self.iterators[idx].instrument_id.clone()
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
    buffer_idx: usize, // Index into buffers Vec (0-based, in file loading order)
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
