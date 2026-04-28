# Nexus — Full Parity Roadmap Tracker v5

**Goal:** Correct, fast tick-level backtesting engine with multi-venue TVC3 data infrastructure. NOT Nautilus parity — get the engine fundamentally right first.

**Core priorities (in order):**
1. Fix backtest correctness (double-path, OrderEmulator, ORB timestamp)
2. Build DataManager + multi-venue TVC3 ingestion
3. Wired hybrid bar+tick strategies
4. Performance + packaging

**Current Parity:** N/A — misleading metric; engine is broken, parity number is meaningless

**Last Verified:** 2026-04-29 against actual source at `libs/nexus/src/` and `libs/strategy/src/`

---

## CRITICAL — Active Blockers (stop ship)

| # | Blocker | File | Impact |
|---|---------|------|--------|
| 1 | **Double-path execution bug** — `process_fills` (OrderEmulator) AND market-order path BOTH fire on same signal | `portfolio.rs:542–782` | 80 trades vs 22 expected; over-trading |
| 2 | **`NoOpStrategyCtx` stub** — all methods return 0/None/empty; signals discarded | `actor_wrapper.rs:237–282` | Live trading completely non-functional |
| 3 | **`add_strategy()` missing** — not defined on `TradingNode` | `trader.rs` | Cannot attach any strategy to live node |
| 4 | **`BinanceMarketDataAdapter` not wired** — exists but never registered to `TradingNode` | `trader.rs` | No live market data feed |
| 5 | **`SystemClock::next_time_ns` stub** — returns `0` always | `actor.rs:190` | Clock-driven events won't fire |
| 6 | **`data/messages.rs` missing serde** — no `Serialize`/`Deserialize` on any struct | `data/messages.rs` | Cross-process messaging broken |
| 7 | **ORB timestamp modulo bug** — `% 86400.0` strips calendar date | `orb_backtest.rs:89` | ORB range logic applies across ALL dates not just target |
| 8 | **`TimeBarAggregator::bar_type()` panic** — `unimplemented!()` at runtime | `bar_aggregation.rs:660` | Triggered when time-based bars (Minute/Hour/Day) are used in live engine — line 1558 routes to `TimeBarAggregator::with_period_ns` |
| 9 | **Multi-venue file ingestion missing** — Bybit/OKX/Coinbase cannot produce TVC3 files | `ingestion/` | Only Binance has CSV→TVC3 converter; ring-buffer-once philosophy requires pre-built TVC3 per venue; can't build multi-venue datasets |
| 11 | **No DataManager** — no structured folder layout, no catalog, no auto-download-on-miss | `data_manager/` | Blocks the entire load-once sweep-many philosophy for any venue other than Binance |

---

## ⚠️ HYBRID ENGINE VISION — Data Infrastructure Requirement

**This is the core goal of Nexus — not an afterthought.**

### What This Means
- Strategies run on OHLCV bars (condition checking)
- **BUT** fills, slippage, and mid-bar entry signals use tick-level data
- Example: "if bar close > 20EMA AND price crosses bid-ask spread by 2bps within next 500ms → enter"
- VPIN-based fill modeling per tick, not per bar
- Multi-venue: Binance for spot, Bybit/OKX for perpetuals, all normalized to TVC3

### What's Required
| Requirement | Current State | Status |
|-------------|--------------|--------|
| TVC3 storage | ✅ | Full impl — ring buffer loads from TVC3 |
| TVC3 conversion per venue | Binance only | ❌ |
| Multi-venue TVC3 dataset | None | ❌ |
| Bar + tick hybrid backtest engine | Broken (double-path bug) | ❌ |
| VPIN per tick (not per bar) | ✅ | ✅ |

### Current Blocker for Hybrid
1. **Only Binance has TVC3 conversion** — Bybit/OKX/Coinbase cannot produce TVC3; multi-venue strategies impossible to backtest
2. **Double-path bug** — tick fill engine and market-order path collide (blocker #1 above)
3. **No data manager** — no structured folder layout, no catalog, no auto-download-on-miss

---

## 📁 DATA MANAGER — Structured Data Storage Architecture

**Philosophy:** Load ring buffer once, sweep many. But the data must exist first — this is the ingestion layer that makes that possible.

### Target Folder Structure
```
data/
  binance/
    spot/
      BTCUSDT/
        2025-01-01.tvc
        2025-01-02.tvc
    usdt-futures/          # BTCUSDT_PERP, etc.
      BTCUSDT_PERP/
        2025-01-01.tvc
  bybit/
    spot/
      BTCUSDT/
        2025-01-01.tvc
    linear/                 # USDT perpetuals
      BTCUSDT/
        2025-01-01.tvc
  okx/
    spot/
      BTCUSDT/
        2025-01-01.tvc
    swap/                   # USDT-M perpetuals
      BTCUSDT-USD-SWAP/
        2025-01-01.tvc
  coinbase/
    spot/
      BTC-USD/
        2025-01-01.tvc
```

### Data Manager Responsibilities
1. **Catalog** — track what `.tvc` files exist locally (exchange/venue/symbol/date)
2. **Missing data detection** — given a backtest config, detect gaps
3. **Download** — pull historical data from exchange APIs (Binance Data Archive, Bybit HTTP, OKX HTTP, Coinbase)
4. **Convert** — raw → TVC3 (venue-specific parsing per adapter)
5. **Store** — write to correct structured folder path
6. **Load** — supply `TickBufferSet::from_files()` with correct paths

### Required Components (none exist today)
| Component | Status | Notes |
|-----------|--------|-------|
| Structured folder manager | ❌ | Create/migrate legacy `tvc_data/` to new structure |
| Data catalog (index of available files) | ❌ | Knows what exists without scanning filesystem |
| Multi-venue HTTP download adapters | ❌ | Binance done; Bybit/OKX/Coinbase HTTP for historical data |
| TVC3 converter per venue | ❌ | Only Binance `BinanceFileIngestor`; need Bybit/OKX/Coinbase parsers |
| DataManager facade (check → download → load) | ❌ | Single API: `DataManager::load(config) → RingBuffer` |

---

## PHASE 1 — Data Layer ✅ 93%

TVC3 binary format, ring buffer, tick buffer, VPIN pipeline, exchange adapters.

| Sub-phase | Description | Status | Verified Finding |
|-----------|-------------|--------|-----------------|
| 1.1 | TVC3 Binary Format | ✅ | Full impl in `tvc3/` |
| 1.2 | RingBuffer | ✅ | `RingBuffer<T>` in `buffer/` |
| 1.3 | TickBuffer + VPIN | ✅ | VPIN computed per tick in `binance_file.rs` |
| 1.4 | Bar Aggregation | ✅ | `RenkoBarAggregator`, `ValueImbalanceBarAggregator`, `ValueRunsBarAggregator` all implemented |
| 1.5 | Multi-Instrument | ✅ | Instrument routing in `DataEngine` |
| 1.6 | Exchange Ingestion | 🟡 | **File:** `BinanceFileIngestor` (CSV→TVC3) manual only; Bybit/OKX have WS adapters but **no file ingestion**; Coinbase has **no market data adapter**. Philosophy: load-once ring buffer requires pre-built TVC3 per venue — only Binance producible today |
| 1.7 | Data Catalog + Storage Structure | ❌ | No structured folder layout; no catalog of available TVC3 files; no `DataManager` facade; checksum validation exists in `TvcReader::open()` |
| 1.8 | Instrument Detail | ✅ | `Instrument` struct fully populated |
| 1.9 | Data Stats | 🟡 | `DataEngine` collects stats; **no data availability catalog** to know what TVC3 files exist per venue/date range |

---

## PHASE 2 — Backtesting Engine 🟡 65%

| Sub-phase | Description | Status | Verified Finding |
|-----------|-------------|--------|-----------------|
| 2.1 | Core Engine | ✅ | `Portfolio::run_portfolio()` tick loop works |
| 2.2 | VPIN Slippage | ✅ | VPIN-based fill modeling in `OrderEmulator` |
| 2.3 | SL/TP + Order Management | 🟡 | Trailing stop wired (`orders.rs:410`); circuit breaker fields exist but `is_circuit_broken()` logic unverified |
| 2.4 | Multi-Instrument Portfolio | ✅ | `HashMap<InstrumentId, InstrumentState>` |
| 2.5 | L2 Order Book Simulation | 🟡 | `MatchingCore` + `OrderEmulator` exist; **BUG: double-path fires both on same signal** |
| 2.6 | Parameter Sweeps | ✅ | `ParameterSweepRunner` in `mc_wf/` |
| 2.7 | Monte Carlo + Walk-Forward | ✅ | `MonteCarloRunner` + `WalkForwardRunner` in `mc_wf/mod.rs` |
| 2.8 | OrderEmulator | 🟡 | `process_fills` called at `portfolio.rs:566` but broken by double-path bug |

### Phase 2 Critical Bug: Double-Path Execution

**File:** `portfolio.rs` lines 542–782

When `use_matching_core = false` (OrderEmulator path):
1. `process_fills` queues and fills limit orders → updates position (`portfolio.rs:566`)
2. Market-order path at `portfolio.rs:752` ALSO fires on same `final_signal != last_sig` → **opens/closes position again**

When `use_matching_core = true` (MatchingCore path):
1. `MatchingCore::submit_limit` fills at `portfolio.rs:663` → updates position
2. Market-order path ALSO fires → **opens/closes position again**

**Result:** 80 trades vs 22 expected (Python baseline)

**Fix required:** The market-order path (`portfolio.rs:752`) must be gated behind a config flag or removed when using a fill-engine path. Only ONE execution path should fire per signal change.

---

## PHASE 3 — Strategy Framework 🟡 60%

| Sub-phase | Description | Status | Verified Finding |
|-----------|-------------|--------|-----------------|
| 3.1 | Strategy Trait | ✅ | `PortfolioStrategy` trait in `portfolio.rs` |
| 3.2 | Strategy Context | 🟡 | **`EngineContext` fully implemented** for backtest; **`NoOpStrategyCtx` is FULL STUB** for live — all 9 methods return 0/None/empty |
| 3.3 | Indicator Library | ✅ | `StochasticOscillator`, `Atr` update methods work |
| 3.4 | Example Strategies | ✅ | `OrbStrategy` in `examples/` |
| 3.5 | Strategy Optimization | ✅ | Walk-forward runner exists |
| 3.6 | Signals Framework | ✅ | `SignalBus` wired in portfolio tick loop (`portfolio.rs:638`) |
| 3.7 | Live Strategy (Actor-Based) | ❌ | `LiveStrategyCtx` trait defined in `live_strategy.rs`; **never implemented** |

### Phase 3.2 — Strategy Context Detail

**Backtest (`EngineContext`):** 13 methods implemented ✅

**Live (`NoOpStrategyCtx`):** ALL STUBS ❌
```rust
// actor_wrapper.rs:237–282
struct NoOpStrategyCtx;
impl StrategyCtx for NoOpStrategyCtx {
    fn current_price(&self, ..) -> f64 { 0.0 }
    fn position(&self, ..) -> Option<PositionSide> { None }
    fn account_equity(&self) -> f64 { 0.0 }
    fn unrealized_pnl(&self, ..) -> f64 { 0.0 }
    fn pending_orders(&self, ..) -> Vec<Order> { vec![] }
    fn subscribe_instruments(&mut self, ..) {}
    fn subscribe_signal(&mut self, ..) {}
    fn submit_limit(&mut self, ..) -> u64 { 0 }
    fn submit_market(&mut self, ..) -> u64 { 0 }  // ← signals discarded!
    fn submit_with_sl_tp(&mut self, ..) -> u64 { 0 }
    fn emit_signal(&mut self, _: Signal) {}      // ← no-op!
}
```

### Phase 3.7 — LiveStrategyCtx

`LiveStrategyCtx` trait defined at `libs/strategy/src/live_strategy.rs` — **zero implementations exist**. This is NOT the same trait as `StrategyCtx` (different crate). A real implementation must be created.

---

## PHASE 4 — Execution + Risk 🟡 52%

| Sub-phase | Description | Status | Verified Finding |
|-----------|-------------|--------|-----------------|
| 4.1 | Order Types | ✅ | All types implemented: `TrailingStopMarket`, `TrailingStopLimit`, `MarketToLimit`, `LimitIfTouched`, `MarketIfTouched`, `PostOnly`, `ReduceOnly` |
| 4.2 | Order Matching | ✅ | `MatchingCore` (FIFO) + `OrderEmulator` (probabilistic) both exist |
| 4.3 | Position Sizing | ✅ | Fixed `position_size` via `PortfolioStrategy::position_size()` |
| 4.4 | Risk Controls | ✅ | `RiskEngine` with throttler; `try_submit()` / `try_modify()` |
| 4.5 | VPIN Calibration | ✅ | `VpState` tracks `quality_bucket`, `vpin` |
| 4.6 | Margin System | ✅ | Margin calculations in `position.rs` |
| 4.7 | Account Model | 🟡 | Single venue, single currency works; multi-venue/multi-currency incomplete |

---

## PHASE 5 — Live Trading 🟡 28%

| Sub-phase | Description | Status | Verified Finding |
|-----------|-------------|--------|-----------------|
| 5.0a | Actor + MsgBus | ✅ | `Actor` trait + `MessageBus` fully implemented |
| 5.0b | Clock + Time Events | 🟡 | `TestClock` ✅; **`SystemClock::next_time_ns` returns 0** ❌ |
| 5.0c | Message Serialization | 🟡 | All `data/messages.rs` structs have `#[derive(Clone)]`/`Debug`; **ZERO have `serde::Serialize/Deserialize`** |
| 5.0d | Cache + Indices | 🟡 | Primary index works; **7 secondary indices missing** vs Nautilus |
| 5.0e | Database | ❌ | No implementation — `Database` trait exists but no SQL/NoSQL impl |
| 5.0f | TraderId on Component | ✅ | `TraderId` field on `Component` |
| 5.0g | Account + OMS | 🟡 | `ExchangeRouter` + `CoinbaseHMAC` present; multi-venue routing incomplete |
| 5.0h | Component Event Handlers | 🟡 | ~12 `on_*` methods implemented on `Actor` trait; **~38 missing** |
| 5.1 | Paper Trading | ✅ | `PaperExecutionClient` + `SimulatedExchange` trait exist |
| 5.2 | Live Execution | 🟡 | `ExecutionClient` tick size validation present; routing map exists but not fully wired |
| 5.3 | OMS Reconciliation | ✅ | Exchange confirm wired for modify + submit |
| 5.4 | Multi-Exchange | ✅ | `ExchangeRouter` + `CoinbaseHttpAdapter` with HMAC-SHA256 |
| 5.5 | Data Engine | 🟡 | Shared via `Arc<Mutex<DataEngine>>`; **`BinanceMarketDataAdapter` NOT registered to `TradingNode`** |
| 5.6 | Risk Engine (Live) | ✅ | `RiskEngine` registered on MsgBus (`risk.rs:168–169`); `on_trade` wired |
| 5.7 | Trader Node | 🟡 | `TradingNode` + `run_blocking()` + health heartbeat done; **`add_strategy()` not defined** |

### Phase 5.0b — SystemClock Stub

```rust
// actor.rs:190
impl Clock for SystemClock {
    fn next_time_ns(&self, _name: &str) -> u64 {
        0  // ← STUB — timers won't fire
    }
}
```

### Phase 5.7 — add_strategy Missing

```rust
// trader.rs — TradingNode has no add_strategy method
pub struct TradingNode { ... }
impl TradingNode {
    pub fn add_market_adapter(&mut self, adapter: Box<dyn MarketDataAdapter>) { ... }  // ← exists
    // pub fn add_strategy(&mut self, strategy: ???) → ???  ← MISSING
}
```

---

## Verified Gaps — Ground Truth (2026-04-29)

### ✅ Resolved
| Gap | File | Resolution |
|-----|------|------------|
| H2 | `trader.rs` | `TradingNode` owns `Arc<Mutex<Vec<Box<dyn MarketDataAdapter>>>>` with `add_market_adapter()` |
| H4 | `actor.rs` | `on_order_book_deltas` + `on_order_modified` added to `Actor` trait |
| M2 | `engine/risk.rs` | `Throttler` + `try_submit()` / `try_modify()` on `RiskEngine` |
| M4 | `bybit_http_adapter.rs`, `okx_http_adapter.rs` | `RateLimiter` wired into order methods |
| M16 | `buffer/bar_aggregation.rs` | `RenkoBarAggregator` + `ValueImbalanceBarAggregator` + `ValueRunsBarAggregator` |

### ❌ Still Open
| Gap | File | Severity |
|-----|------|----------|
| **Double-path bug** | `portfolio.rs:542–782` | 🔴 Critical — causes 3.6x over-trading |
| **NoOpStrategyCtx stub** | `actor_wrapper.rs:237` | 🔴 Critical — live signals discarded |
| **add_strategy missing** | `trader.rs` | 🔴 Critical — no live strategy wiring |
| **BinanceAdapter not wired** | `trader.rs` | 🔴 Critical — no live data feed |
| **SystemClock stub** | `actor.rs:190` | 🔴 Critical — clock events won't fire |
| **messages.rs no serde** | `data/messages.rs` | 🔴 Critical — IPC broken |
| **ORB modulo bug** | `orb_backtest.rs:89` | 🟡 High — wrong date filtering |
| **LiveStrategyCtx not impl** | `live_strategy.rs` | 🟡 High — Phase 3.7 blocked |
| **Database no impl** | — | 🟡 High — persistence missing |
| **7 secondary cache indices** | `cache/` | 🟡 Medium |

---

## Testing Status

| Suite | Status |
|-------|--------|
| `nexus` unit tests | 387 passing |
| Risk Engine tests | 21 passing |
| OMS tests | 11 passing |
| Data Engine tests | 12 passing |

---

## Impedance Mismatches

| Issue | Detail |
|-------|--------|
| **Two `StrategyCtx` traits** | `nexus::StrategyCtx` (EngineContext impl in `nexus`) vs `nexus_strategy::StrategyCtx` (in `strategy` crate) — different traits, different crates |
| **Two context naming conflicts** | `nexus::strategy_ctx::StrategyCtx` vs `strategy::StrategyCtx` — namespace collision |
| `LiveStrategyCtx` trait isolation | Defined in `libs/strategy/src/live_strategy.rs` but never implemented anywhere |
| Timestamp modulo bug | Both Rust (`orb_backtest.rs:89`) and Python (`orb_nautilus.py`) use `% 86400.0` which strips calendar date |

---

## Commit History (recent)

| Commit | Description |
|--------|-------------|
| `cf136c5` | fix: ingestion timestamp (µs→ns) and instrument ID parsing |
| `f79883b` | feat: file-based ingestion — CSV to TVC3 converter |
| `acfcb0d` | M4: Wire RateLimiter into Bybit and OKX HTTP adapters |
| `dfde58b` | M2: Add submit/modify throttlers to RiskEngine |
| `78dfb9b` | M16: RenkoBarAggregator + ValueImbalanceBarAggregator + ValueRunsBarAggregator |
| `b3747d5` | docs: clean up stale gap tracking after systematic verification |
| `66afd22` | Phase 5.7: TradingNode + SIGTERM + health heartbeat |
| `91e0a44` | Fix Send+Sync bounds for timer callbacks |
| `f295cb5` | Phase 5.6: wire order rejections to OMS cache and MsgBus |
| `a7bc0d5` | fix: centralized MsgBus data flow for Data Engine |
| `1d59dac` | fix: share DataEngine via Arc<Mutex> |
| `8b83fcd` | fix: implement CoinbaseHttpAdapter with HMAC-SHA256 signing |
| `87277bc` | phase 4.2: conditional MatchingCore vs OrderEmulator routing |

---

*Tracker v5 — rebuilt 2026-04-29 from direct source verification. Code is truth.*
