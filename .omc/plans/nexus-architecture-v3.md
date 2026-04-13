# Nexus — High-Performance Tick-By-Tick Backtesting Engine (v3)

## Context

Nautilus Trader is a production-grade algorithmic trading framework in Rust with a Python API. Its backtesting engine uses Parquet/Arrow for storage (slow decode, large files). Nexus aims to build a Rust-only, library-first backtesting engine that surpasses Nautilus in raw performance, with multi-instrument portfolio backtesting as a core differentiator.

**Scope:** Core engine only — no AI, no UI, no DSL. TVC3 format, ring buffer, tick buffer, multi-instrument engine, parameter sweeps, strategy trait.

---

## RALPLAN-DR Summary (v3)

### Principles (5)
1. **Library-first** — Engine is a `cdylib` + `rlib` Rust crate; no daemon, no service
2. **Zero-copy in hot path** — mmap storage, Arc-sharing for sweeps, no per-tick heap allocation, delta decode happens once at buffer construction
3. **Real integration tests** — Create actual TVC files, assert exact PnL/tolerance, not "it compiles"
4. **Decisions documented** — Every significant decision gets an ADR with rationale and alternatives rejected
5. **Phase-gated** — Phase done only when: zero warnings + all tests pass + integration test passes + wired to system

### Decision Drivers (Top 3)
1. **Storage efficiency + decode speed** — TVC3 ~4-5 bytes/tick vs Nautilus Parquet; O(log n) random seek
2. **Sweep parallelism** — Merged anchor index built once; TickBuffer pre-decoded; Arc<TickBufferSet> shared across rayon workers; each combo = full portfolio backtest
3. **Multi-instrument portfolio** — Cross-instrument strategies with per-instrument tick streams; 10-20 instruments supported in Phase 1

### Viable Options

#### Option A: TVC3-native, Rust-only (FAVORED)
**Approach:** Implement TVC3 from scratch with 16-byte `AnchorIndexEntry` (matching Nautilus production). Build ring buffer with merged anchor index, tick buffer with pre-decoded VPIN bucketing, multi-instrument engine.

**Pros:** Full control, ~4-5 bytes/tick, O(log n) seek, backward compatible with Nautilus TVC files
**Cons:** Must implement ingest adapters (reusable from TVC-OMC)
**Why 16 bytes:** Nautilus production loaders use 16B; TVC-OMC uses 24B (redundant timestamp); Nexus matches Nautilus for compatibility.

#### Option B: Extend Nautilus Parquet engine
**Pros:** Reuses Nautilus data model, instrument definitions, order matching
**Cons:** Parquet is slower; scope creep; upstream buy-in required; Python/Rust binding overhead

---

## Architecture

```
nexus/
├── libs/
│   ├── tvc/                      # TVC3 binary format
│   │   └── src/
│   │       ├── types.rs          # TvcHeader(128B), AnchorTick(30B), AnchorIndexEntry(16B)
│   │       ├── compression.rs    # delta pack/unpack (4B base + 15B overflow)
│   │       ├── writer.rs         # TvcWriter, finalize with index + SHA256 sidecar
│   │       └── reader.rs         # TvcReader, mmap, seek_to_tick, decode_tick_at
│   ├── nexus/                    # Core engine
│   │   └── src/
│   │       ├── buffer/
│   │       │   ├── ring_buffer.rs     # RingBuffer, RingIter (single-file)
│   │       │   ├── tick_buffer.rs     # TradeFlowStats, TickBuffer (pre-decoded)
│   │       │   └── buffer_set.rs      # TickBufferSet = Arc<HashMap<InstrumentId, TickBuffer>>
│   │       ├── engine.rs              # BacktestEngine, EngineContext, Position
│   │       ├── slippage.rs           # VPIN slippage: compute_fill_delay()
│   │       ├── portfolio.rs           # Portfolio, InstrumentState, cross-instrument logic
│   │       ├── sweep/runner.rs       # SweepRunner, Arc<TickBufferSet> shared
│   │       └── lib.rs
│   └── strategy/
│       └── src/
│           ├── strategy_trait.rs     # Strategy: Send + Sync + Clone + clone_box()
│           ├── context.rs             # StrategyCtx
│           └── strategies/           # Example strategies
└── apps/
    └── cli/src/main.rs
```

### Data Flow
```
Exchange → TVC3 files (ingest)
                    ↓
        RingBufferSet: HashMap<InstrumentId, RingBuffer>
        [Each RingBuffer: mmap'd TVC3, merged anchor index per file]
                    ↓
        TickBufferSet: Arc<HashMap<InstrumentId, Arc<TickBuffer>>>
        [Each TickBuffer: pre-decoded TradeFlowStats, VPIN bucketing, sequential in-memory]
                    ↓
        SweepRunner::run_grid():
        Arc<TickBufferSet> shared across rayon workers
        Each worker: 1 combo = full portfolio backtest (all instruments, time-ordered)
                    ↓
        Portfolio::evaluate(timestamp, instrument_id, tick) → Signal
        [Per-instrument state; cross-instrument signals possible]
                    ↓
        open_position() → VPIN slippage fill simulation
                    ↓
        BacktestResult { portfolio_stats, per_instrument_stats, equity_curve } → JSON
```

### Multi-Instrument Design

**TickBufferSet** — one `TickBuffer` per instrument, time-ordered:
```rust
pub struct TickBufferSet {
    buffers: HashMap<InstrumentId, Arc<TickBuffer>>,
    // Global time cursor: the minimum timestamp across all instrument buffers
    next_tick_time: u64,
}

pub struct InstrumentState {
    position: Option<Position>,
    pending_orders: Vec<Order>,
    equity: f64,
}
```

**Strategy receives per-instrument tick streams:**
```rust
pub trait Strategy: Send + Sync + Clone {
    fn subscribed_instruments(&self) -> Vec<InstrumentId>;

    // Called when any subscribed instrument receives a tick
    fn on_trade(&mut self, instrument_id: InstrumentId, tick: &Tick, ctx: &mut dyn StrategyCtx)
        -> Option<Signal>;

    fn on_bar(&mut self, instrument_id: InstrumentId, bar: &Bar, ctx: &mut dyn StrategyCtx)
        -> Option<Signal>;

    fn clone_box(&self) -> Box<dyn Strategy>;
}
```

**Sweep: one combo = full portfolio backtest:**
```rust
impl SweepRunner {
    pub fn run_grid(&self, grid: &ParameterGrid, ...) -> Vec<SweepResult> {
        let buffer_set = Arc::clone(&self.buffer_set);  // cheap Arc clone
        combos.par_iter()  // rayon: each combo = 1 CPU core
            .filter_map(|params| {
                let strategy = self.strategy_factory(params.clone()); // fresh instance
                let portfolio = Portfolio::new(strategy.subscribed_instruments());
                let mut engine = BacktestEngine::new(self.config.clone());
                engine.run_portfolio(&buffer_set, strategy, &mut portfolio)  // all instruments, time-ordered
                    .map(|result| SweepResult { params: params.clone(), stats: result.portfolio_stats })
            })
            .collect()
    }
}
```

**Per-combo strategy cleanup:** Each combo gets a fresh strategy instance via `clone_box()`. When the combo completes (engine returns), the strategy is dropped. No cross-worker sharing, no locking.

---

## TVC3 Format Spec

### File Layout
```
[128-byte header]
[anchor tick 30B][delta tick 4B or 15B]...[anchor][delta]...
                                            ↑ index at EOF
[index: 4B num_anchors + (16B × num_anchors)]
```

### `TvcHeader` — 128 bytes `#[repr(C, packed)]`
```rust
// magic: b"TVC3" (4B), version: u8, decimal_precision: u8
// anchor_interval: u32, instrument_id: u32 (FNV-1a hash of symbol)
// start_time_ns: u64, end_time_ns: u64, num_ticks: u64
// num_anchors: u32, index_offset: u64 (byte offset of index section at EOF)
// reserved: [u8; 78]
static_assertions::const_assert!(size_of::<TvcHeader>() == 128);
```

### `AnchorTick` — 30 bytes `#[repr(C, packed)]`
```rust
// timestamp_ns: u64, price_int: u64, size_int: u64
// side: u8, flags: u8, sequence: u32
static_assertions::const_assert!(size_of::<AnchorTick>() == 30);
```

### `AnchorIndexEntry` — 16 bytes `#[repr(C, packed)]`
```rust
// tick_index: u64   (cumulative tick number within file)
// byte_offset: u64  (byte position of anchor within file)
static_assertions::const_assert!(size_of::<AnchorIndexEntry>() == 16);
```
**timestamp_ns NOT stored** — derived from decoded anchor tick. (Nexus uses 16B matching Nautilus; TVC-OMC uses 24B with redundant timestamp.)

### Delta Compression — 4 bytes base
```
bits 0-19:  timestamp_delta (20 bits, max ~524ms at ns precision)
bits 20-37: price_zigzag   (18 bits, zigzag i32)
bit 38:     side          (1 bit: 0=Buy, 1=Sell)
bit 39:     flags         (1 bit: 1=trade)
```
Overflow record (15 bytes): `0xFF | ts_extra(2B) | price_extra(4B) | size_extra(4B) | base(4B)`

---

## VPIN Slippage Model

### Formula (exact implementation required)
```rust
const MAX_DELAY_NS: u64 = 200_000_000; // 200ms cap
const MAX_IMPACT_BPS: f64 = 15.0;

fn compute_fill_delay(
    order_size_ticks: f64,    // size / avg_tick_size
    vpin: f64,               // |BV - SV| / (BV + SV), [0.0, 1.0]
    avg_tick_duration_ns: u64,
) -> (u64, f64) {
    // Phase 1: adverse selection probability from VPIN
    let adverse_prob = vpin * 0.5;

    // Phase 2: delay in nanoseconds
    let delay_ticks = (order_size_ticks * adverse_prob * 1.5).ceil();
    let delay_ns = delay_ticks
        .saturating_mul(avg_tick_duration_ns)
        .min(MAX_DELAY_NS);

    // Phase 3: price impact in basis points
    let size_impact = (order_size_ticks.sqrt() / 100.0).min(10.0);  // max 10 bps
    let vpin_impact = vpin * 5.0;                                    // max 5 bps at vpin=1.0
    let impact_bps = (size_impact + vpin_impact).min(MAX_IMPACT_BPS); // cap 15 bps

    (delay_ns, impact_bps)
}
```

**Fill price:**
```
fill_timestamp = timestamp_ns + delay_ns
fill_price = market_price_at(fill_timestamp) * (1.0 + impact_bps / 10000.0)
```

### VPIN Bucket Computation
```
bucket_size = num_ticks / NUM_BUCKETS (default 50)
VPIN[bucket] = |cum_buy_vol[bucket] - cum_sell_vol[bucket]| / (cum_buy_vol[bucket] + cum_sell_vol[bucket])
```

---

## Implementation Phases

### Phase 1: TVC3 Binary Format
- [ ] `libs/tvc/src/types.rs` — `TvcHeader`(128B), `AnchorTick`(30B), `AnchorIndexEntry`(16B, NOT 24B), `static_assertions`
- [ ] `libs/tvc/src/compression.rs` — `pack_delta()`, `unpack_delta()`, overflow detection; base(4B) / overflow(15B)
- [ ] `libs/tvc/src/writer.rs` — `TvcWriter::write_tick()`, anchor every `anchor_interval`, `finalize()` writes index + SHA256 sidecar
- [ ] `libs/tvc/src/reader.rs` — `TvcReader::open()` (mmap + SHA256 verify), `seek_to_tick()` binary search, `decode_tick_at()`
- [ ] `libs/tvc/tests/roundtrip_test.rs` — encode N synthetic ticks → decode → assert byte-exact match
- [ ] `libs/tvc/CLAUDE.md` — format spec with byte layouts; INDEX_ENTRY_SIZE=16 (matches Nautilus)

**Phase 1 Exit Gate:** `cargo test -p tvc` passes; roundtrip test with 1M ticks; byte count within 5% of theoretical.

### Phase 2: Ring Buffer + Merged Anchors
- [ ] `libs/nexus/src/buffer/ring_buffer.rs` — `RingBuffer` with `Arc<Mmap>`, per-file binary search on anchors
- [ ] `RingIter` — zero-copy sequential iteration over mmap slices
- [ ] `RingBufferSet` — `HashMap<InstrumentId, RingBuffer>`, merged anchor index across all files for an instrument
- [ ] `from_date_range()` — glob TVC files for date range, build merged anchor index (k-way merge, O(total_anchors × log(num_files)))
- [ ] `seek_to_tick(tick_index)` — binary search across merged anchors → (file_index, byte_offset) → decode forward
- [ ] `libs/nexus/tests/ring_buffer_test.rs` — two 100K-tick TVC files → merged tick count = sum

**Phase 2 Exit Gate:** `cargo test -p nexus --lib buffer` passes; merged iterator produces correct tick count and order.

### Phase 3: TickBuffer + VPIN
- [ ] `libs/nexus/src/buffer/tick_buffer.rs` — `TradeFlowStats`, `TickBuffer::from_ring_buffer()`, sequential decode + VPIN bucketing
- [ ] `TickBufferSet` — `Arc<HashMap<InstrumentId, Arc<TickBuffer>>>`, all instruments in memory
- [ ] `bucket_vpin(bucket_idx)` — compute VPIN from cumulative volume deltas across bucket boundaries
- [ ] VPIN bucket size configurable at construction (default 50 buckets)
- [ ] `libs/nexus/tests/tick_buffer_test.rs` — known volume series → known VPIN; VPIN=0 at 50/50 buy/sell split ±1e-9

**Phase 3 Exit Gate:** VPIN test passes; TickBuffer build time < 10% of sequential decode.

### Phase 4: Backtest Engine (Single Instrument)
- [ ] `libs/nexus/src/engine.rs` — `BacktestEngine::run()` dispatch to tick/bar mode
- [ ] `EngineContext` — position, equity, equity_curve, trades; commission on entry AND exit
- [ ] `libs/nexus/src/slippage.rs` — `compute_fill_delay()` implementing exact formula above
- [ ] SL/TP checking per tick
- [ ] `libs/nexus/tests/engine_test.rs`:
  - Commission: entry $100, exit $110, size=1, commission=0.001 → net PnL $9.99 ± $0.001
  - Position lifecycle: long → partial close → full close → equity correct ± $0.01
  - VPIN slippage: large order at VPIN=0.8 → fill_price > market_price +0.1 bps minimum

**Phase 4 Exit Gate:** All integration tests pass; commission math verified; VPIN slippage directional correctness confirmed.

### Phase 5: Multi-Instrument Portfolio
- [ ] `libs/nexus/src/portfolio.rs` — `Portfolio`, `InstrumentState`, per-instrument position tracking
- [ ] `BacktestEngine::run_portfolio(buffer_set, strategy, portfolio)` — time-ordered iteration across all instrument TickBuffers simultaneously
- [ ] `Portfolio::next_event()` — merge cursor across all instrument buffers, return next (timestamp, instrument_id, tick)
- [ ] Strategy `on_trade(instrument_id, tick)` delivery to strategy
- [ ] Portfolio-level equity (sum of all instrument equity) and per-instrument equity tracked separately
- [ ] `libs/nexus/tests/portfolio_test.rs`:
  - Two instruments, both traded → portfolio equity = sum of both
  - One instrument flat, other has position → portfolio reflects correct net equity
  - Time-ordered delivery: ensure instrument A tick at t=100 arrives before instrument B tick at t=101

**Phase 5 Exit Gate:** Multi-instrument portfolio test passes; time-ordered tick delivery verified.

### Phase 6: Parameter Sweeps (Portfolio)
- [ ] `libs/nexus/src/sweep/runner.rs` — `SweepRunner` with `Arc<TickBufferSet>`
- [ ] `Strategy: Clone` bound; each combo gets fresh strategy via `clone_box()`, dropped on combo completion
- [ ] `run_grid()` — rayon `par_iter().filter_map()`; each worker = full portfolio backtest
- [ ] Filter conditions (AND), rank by portfolio-level metric (total Sharpe, total PnL), top_n
- [ ] `libs/nexus/tests/sweep_test.rs` — 2D grid, verify parallelism speedup; verify strategy dropped after combo

**Phase 6 Exit Gate:** 100-combo sweep wall time < sequential_time / num_cpus × 1.2.

### Phase 7: Strategy Trait + Examples
- [ ] `libs/strategy/src/strategy_trait.rs` — `Strategy: Send + Sync + Clone`
- [ ] `Tick` (timestamp_ns, price_int, size_int, side, vpin, buy_volume, sell_volume)
- [ ] `Bar` (open, high, low, close, volume, buy_volume, sell_volume, tick_count)
- [ ] `StrategyCtx`: `current_price()`, `position()`, `account_equity()`, `subscribe_instruments()`
- [ ] Example: EMA cross per instrument
- [ ] `libs/strategy/tests/strategy_test.rs`

### Phase 8: CLI + Ingestion
- [ ] `apps/cli/src/main.rs` — `tvc ingest`, `tvc backtest`, `tvc sweep`
- [ ] Binance WebSocket → TVC3 writer (checkpoint every 100K ticks)
- [ ] Backtest: load TVC3 per instrument, run portfolio engine, output JSON
- [ ] Sweep: load all instruments, run grid, output top-N results

---

## Verification

| # | Test | Expected | Tolerance |
|---|------|----------|-----------|
| 1 | TVC3 roundtrip 1M ticks | byte-for-byte identical | 0 diff |
| 2 | Two 100K-tick files → merged count | 200K ticks | exact |
| 3 | VPIN at 50/50 buy/sell | 0.0 | ±1e-9 |
| 4 | Commission: entry $100, exit $110, size=1, comm=0.001 | net PnL = $9.99 | ±$0.001 |
| 5 | Position: long → partial close → close | equity correct each step | ±$0.01 |
| 6 | Two instruments portfolio equity | sum of both | ±$0.01 |
| 7 | 100-combo sweep wall time | < sequential / num_cpus × 1.2 | wall clock |
| 8 | VPIN slippage: large order at high VPIN | fill > market +0.1bps | min +0.1bps |
| 9 | Strategy dropped after sweep combo | memory reclaimed | N/A (check no leaks with miri) |

---

## Risks

| Risk | Mitigation |
|------|------------|
| Merged anchor build O(n log n) for many files | One-time at `from_date_range()`; profile with 365 daily files; acceptable |
| TickBufferSet memory for 10-20 instruments | ~50M-200M ticks × 10-20 instruments; all in memory; ensure no duplicate allocation |
| VPIN bucket size fixed at construction | Document; sweep over bucket size requires TickBuffer rebuild (future Phase 9) |
| Per-instrument state in strategy grows unbounded | Strategy manages own `HashMap<InstrumentId, InstrumentState>`; no framework-managed state leak |
| 1.8B/5s benchmark is decode only | Qualify: Nexus targets same decode speed; full backtest throughput is separate measurement |

---

## ADRs

### ADR-001: TVC3 `AnchorIndexEntry` = 16 bytes (matching Nautilus)
**Decision:** 16-byte `AnchorIndexEntry` (`tick_index u64` + `byte_offset u64`).
**Drivers:** Backward compatibility with Nautilus production TVC files; smaller index.
**Alternatives:** 24-byte (TVC-OMC) with redundant `timestamp_ns` — rejected as redundant and incompatible.
**Consequences:** Nexus writes files readable by Nautilus Python/Cython loaders.

### ADR-002: Merged Anchor Index (Built Once, O(n log n))
**Decision:** `RingBufferSet::from_date_range()` builds merged anchor index across all files in one pass (k-way merge sort). This is a startup cost, not per-iteration.
**Drivers:** Avoids per-tick file-switching overhead; O(log n) seek via binary search.
**Alternatives:** Per-file RingBuffer with round-robin — rejected (introduces per-iteration switching overhead per user requirement).
**Consequences:** Startup time proportional to total anchor count (~365 files/year × 1000 anchors/file = 365K entries to merge).
**Follow-ups:** Profile for >1000 files; consider parallel anchor loading if needed.

### ADR-003: TickBuffer Pre-Decoded (Decode Once, Iterate Zero-Copy)
**Decision:** `TickBuffer` built via sequential decode pass through `RingBuffer` once at construction. Sweep iterations read from `Arc<TickBuffer>` — no delta decode compute per iteration.
**Drivers:** Eliminates redundant decode compute across sweep iterations (1000 combos × same deltas = massive savings).
**Alternatives:** Decode-on-demand from ring buffer each iteration — rejected (decode compute per iteration defeats sweep parallelism purpose).
**Consequences:** VPIN bucket size fixed at TickBuffer construction. Memory = O(total_ticks × sizeof(TradeFlowStats)).

### ADR-004: Multi-Instrument via `TickBufferSet` + Time-Ordered Merge Cursor
**Decision:** `TickBufferSet = Arc<HashMap<InstrumentId, Arc<TickBuffer>>>`. Portfolio engine iterates via `Portfolio::next_event()` — always the next earliest timestamp across all instruments, delivered to strategy as `(instrument_id, tick)`.
**Drivers:** Natural time ordering; per-instrument tick streams to strategy; rayon parallelism across combos, not across instruments.
**Alternatives:** Per-combo multi-threaded tick decode — rejected (complex; decode is one-time cost, not the hot path in sweeps).
**Consequences:** Strategy receives ticks per-instrument via `on_trade(instrument_id, tick)` and manages per-instrument state itself.

### ADR-005: VPIN Slippage Formula (Calibrated, Configurable)
**Decision:** `compute_fill_delay()` uses adverse_prob = VPIN × 0.5, delay_ticks = order_size × adverse_prob × 1.5, capped at 200ms; impact_bps = sqrt(order_size)/100 + VPIN × 5, capped at 15bps.
**Drivers:** Grounded in VPIN microstructure literature; reproducible; testable.
**Consequences:** Calibration constants (0.5, 1.5, 5.0, 200ms, 15bps) may need empirical tuning against real market data.
**Follow-ups:** Expose constants as `SlippageConfig` parameters in Phase 7.
