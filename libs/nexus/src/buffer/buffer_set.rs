//! RingBufferSet — multiple RingBuffers with merged anchor index.
//!
//! Provides cross-file random access via a merged anchor index built once at startup.
//! Used by TickBufferSet for multi-instrument backtesting.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ring_buffer::{RingBuffer, RingBufferError};

/// Instrument ID type.
///
/// Currently using a u64. In production this could be a FNV-1a hash of (exchange, symbol).
pub type InstrumentId = u64;

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
#[derive(Debug, Clone, Copy)]
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
            let buffer = Arc::new(RingBuffer::open(&path, instrument_id)?);
            let num_ticks = buffer.num_ticks();

            // Collect anchors from this buffer
            for entry in buffer.anchor_index().iter() {
                all_anchors.push(AnchorInfo {
                    instrument_id,
                    byte_offset: entry.byte_offset,
                    local_tick_index: entry.tick_index,
                });
            }

            total_ticks += num_ticks;
            buffers.insert(instrument_id, buffer);
        }

        // Sort by instrument_id, then by local_tick_index
        all_anchors.sort_by_key(|a| (a.instrument_id, a.local_tick_index));

        // Build merged anchor index - group by instrument
        let mut merged_anchors: Vec<MergedAnchor> = Vec::new();
        let mut current_instrument: Option<InstrumentId> = None;
        let mut global_offset: u64 = 0;
        let mut instrument_start_tick: u64 = 0;

        for anchor in &all_anchors {
            if Some(anchor.instrument_id) != current_instrument {
                // New instrument - update global offset
                current_instrument = Some(anchor.instrument_id);
                if let Some(buffer) = buffers.get(&anchor.instrument_id) {
                    // Ticks from this instrument start at current global offset
                    instrument_start_tick = global_offset;
                    global_offset += buffer.num_ticks();
                }
            }

            merged_anchors.push(MergedAnchor {
                global_tick_index: instrument_start_tick + anchor.local_tick_index,
                instrument_id: anchor.instrument_id,
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
    pub fn get(&self, instrument_id: InstrumentId) -> Option<&Arc<RingBuffer>> {
        self.buffers.get(&instrument_id)
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
        self.buffers.keys().copied().collect()
    }

    /// Iterate all instruments' buffers.
    pub fn buffers(&self) -> &HashMap<InstrumentId, Arc<RingBuffer>> {
        &self.buffers
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
    fn test_instrument_id_type() {
        let id: InstrumentId = 12345u64;
        assert_eq!(id, 12345);
    }
}
