<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-14 | Updated: 2026-04-14 -->

# nexus/

Core backtesting engine crate. All backtesting logic lives here.

**Nautilus Reference:** `~/Nautilus/nautilus_trader/nautilus_trader/backtest/engine.pyx` (BacktestEngine, lines 217+, 1310+)

## Purpose

High-performance tick-by-tick backtesting with:
- Ring buffer for zero-copy TVC file access
- Pre-decoded TickBuffer with VPIN bucketing
- Multi-instrument portfolio support
- Parameter sweeps via rayon parallelism

## Subdirectories

| Directory | Purpose | Phase |
|-----------|---------|-------|
| `src/buffer/` | RingBuffer, TickBuffer, TickBufferSet | 1.2, 1.3 |
| `src/engine.rs` | BacktestEngine, EngineContext | 2.1 |
| `src/slippage.rs` | VPIN slippage: `compute_fill_delay()` | 2.2 |
| `src/portfolio.rs` | Portfolio, InstrumentState | 2.4 |
| `src/sweep/` | SweepRunner, parameter sweeps | 2.6 |

## Module Structure

### Phase 1.2 — Ring Buffer (`src/buffer/ring_buffer.rs`)
```rust
RingBuffer // single-file, Arc<Mmap>, per-file binary search on anchors
RingIter   // zero-copy sequential iteration over mmap slices
RingBufferSet // HashMap<InstrumentId, RingBuffer>
// seek_to_tick(tick_index) → binary search across merged anchors
// iter_range(start_ns, end_ns) → time-ordered iteration
```

### Phase 1.3 — TickBuffer + VPIN (`src/buffer/tick_buffer.rs`)
```rust
TradeFlowStats // timestamp_ns, price_int, size_int, side,
//               cum_buy_volume, cum_sell_volume, vpin, bucket_index
TickBuffer::from_ring_buffer(rb, num_buckets) // decode once, VPIN bucketing
Arc<TickBuffer> // shared across rayon workers
```

### Phase 2.1 — Core Engine (`src/engine.rs`)
```rust
BacktestEngine::run() // tick mode or bar mode
EngineContext // position, equity, peak_equity, max_drawdown, equity_curve, trades
process_signal(signal) // Buy/Sell/Close → update position, record trade
```

### Phase 2.2 — VPIN Slippage (`src/slippage.rs`)
```rust
compute_fill_delay(order_size_ticks, vpin, avg_tick_duration_ns) -> (u64, f64)
// adverse_prob = vpin * 0.5
// delay_ticks = ceil(order_size * adverse_prob * 1.5)
// delay_ns = min(delay_ticks * avg_tick_duration, 200_000_000)  // 200ms cap
// size_impact = sqrt(order_size) / 100  // max 10bps
// vpin_impact = vpin * 5.0  // max 5bps
// impact_bps = min(size_impact + vpin_impact, 15.0)  // cap 15bps
```

### Phase 2.4 — Portfolio (`src/portfolio.rs`)
```rust
Portfolio // manages HashMap<InstrumentId, InstrumentState>
BacktestEngine::run_portfolio(buffer_set, strategy, portfolio)
// next_event() → merge cursor across all instrument buffers
// Strategy: on_trade(instrument_id, tick, ctx)
```

### Phase 2.6 — Sweeps (`src/sweep/runner.rs`)
```rust
Arc<TickBufferSet> // shared across rayon workers
Strategy: Clone // each combo gets fresh instance via clone_box()
run_grid(grid, filters, rank_by, top_n) -> par_iter().filter_map()
```

## For AI Agents

### Phase Gate
Each phase exits only when: zero warnings + all tests pass + integration test + wired to system.

### Implementation Order
```
1.2 (RingBuffer) ← 1.1
1.3 (TickBuffer) ← 1.2
2.1 (CoreEngine) ← 1.3
2.2 (VPINSlip) ← 2.1
2.3 (SL/TP) ← 2.1
2.4 (MultiInst) ← 1.5, 2.1
2.5 (L2 Sim) ← 1.4
2.8 (OrderEmulator) ← 1.4, 2.2
2.6 (Sweeps) ← 2.4
2.7 (MC/WF) ← 2.6
```

## Dependencies

### Internal
- `tvc` — TVC3 format

### External
- `memmap2` — mmap I/O
- `rayon` — parallelism
- `thiserror` — errors
- `serde` — serialization

<!-- MANUAL: -->
