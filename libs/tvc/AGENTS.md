<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-14 | Updated: 2026-04-14 -->

# tvc/

TVC3 binary tick storage format crate. The foundation of the entire system.

**Nautilus Reference:** `~/Nautilus/nautilus_trader/nautilus_trader/persistence/tvc_cython_loader.pyx` (format spec, INDEX_ENTRY_SIZE=16 at line ~57)

## Purpose

Delta-compressed, memory-mapped binary format for high-performance tick storage and random-access playback.

## File Layout

```
[128-byte header]
[anchor tick 30B][delta tick 4B or 15B]...[anchor][delta]...
[index: 4B num_anchors + (16B × num_anchors)]
```

## Key Files

| File | Purpose |
|------|---------|
| `src/lib.rs` | Module declarations |
| `src/types.rs` | `TvcHeader`, `AnchorTick`, `AnchorIndexEntry` structs |
| `src/compression.rs` | `pack_delta()`, `unpack_delta()` |
| `src/writer.rs` | `TvcWriter` |
| `src/reader.rs` | `TvcReader` |
| `tests/roundtrip_test.rs` | Roundtrip test: 1M synthetic ticks |

## Module Structure

### `src/types.rs`
```rust
// TvcHeader — 128 bytes #[repr(C, packed)]
// magic: b"TVC3"(4B), version: u8, decimal_precision: u8
// anchor_interval: u32, instrument_id: u32 (FNV-1a hash)
// start_time_ns: u64, end_time_ns: u64, num_ticks: u64
// num_anchors: u32, index_offset: u64
// reserved: [u8; 78]

// AnchorTick — 30 bytes #[repr(C, packed)]
// timestamp_ns: u64, price_int: u64, size_int: u64
// side: u8, flags: u8, sequence: u32

// AnchorIndexEntry — 16 bytes #[repr(C, packed)] (NOT 24)
// tick_index: u64, byte_offset: u64
// 16 bytes matches Nautilus production format
```

### `src/compression.rs`
```rust
// Delta compression — 4 bytes base:
// bits 0-19: timestamp_delta (20 bits, max ~524ms)
// bits 20-37: price_zigzag (18 bits, zigzag i32)
// bit 38: side (1 bit: 0=Buy, 1=Sell)
// bit 39: flags (1 bit: 1=trade)
// Overflow record (15 bytes): 0xFF | ts_extra(2B) | price_extra(4B) | size_extra(4B) | base(4B)
```

### `src/writer.rs`
```rust
TvcWriter::new(path, instrument_id, anchor_interval) -> Self
TvcWriter::write_tick(&mut self, tick: &TradeTick) -> Result<()>
TvcWriter::finalize(self) -> Result<Hash> // writes index + SHA256 sidecar
```

### `src/reader.rs`
```rust
TvcReader::open(path) -> Result<Self> // mmap + SHA256 verify
TvcReader::seek_to_tick(&self, tick_index: u64) -> Result<u64> // byte offset
TvcReader::decode_tick_at(&self, byte_offset: u64) -> Result<TradeTick>
TvcReader::decode_anchor(&self, byte_offset: u64) -> Result<AnchorTick>
```

## For AI Agents

### Phase 1.1 Exit Gate
`cargo test -p tvc` passes. Roundtrip test with 1M synthetic ticks. Byte count within 5% of theoretical.

### Implementation Notes
- Use `static_assertions::const_assert!` to verify struct sizes at compile time
- Index is written at EOF: `4B num_anchors + (16B × num_anchors)`
- SHA256 sidecar for integrity verification
- Files must be compatible with Nautilus Python/Cython loaders

## Testing

Roundtrip test: encode N synthetic ticks → decode → assert byte-for-byte identical.

## Dependencies

### External
- `memmap2` — memory-mapped I/O
- `static_assertions` — compile-time size assertions
- `sha2` — SHA256 for file verification

<!-- MANUAL: -->
