# Nexus — Full Project Roadmap (v2)

## What's New in v2

Added from gap analysis vs Nautilus Trader:

- **Phase 1.8**: Full Instrument Hierarchy (14 instrument types, margin models, funding rates)
- **Phase 1.9**: Synthetic Instruments (derived instruments from formulas)
- **Phase 2.8**: OrderEmulator (queue position, time-priority matching, sophisticated limit order fill simulation)
- **Phase 3.6**: Signals Framework (Signal subsystem via MessageBus, independent of strategy callbacks)
- **Phase 4.6**: Margin System (MarginModel, StandardMargin, LeveragedMargin, MarginAccount)
- **Phase 5.0a**: Actor + MessageBus Architecture (core of live/paper trading)
- **Phase 5.0b**: Clock + Time Events (scheduled events, test clock, real clock)
- **Phase 5.0c**: Cache + Database Layer (in-memory cache, Redis/ilia/dyn persistence for live trading)
- **Phase 7.5**: Detailed Fill Reports (commission + slippage + queue position attribution)

> **Nautilus Reference Convention**: Every phase's scope includes a `Nautilus Source` section. When implementing, read the referenced Nautilus file first. The file path is relative to `~/Nautilus/nautilus_trader/nautilus_trader/`. This prevents stub code and ensures we match Nautilus behavior.

---

## How to Read This

**Numbering:** `X.Y` where `X` = subsystem, `Y` = phase within subsystem.
Example: `3.2` = Subsystem 3 (Backtesting Engine), Phase 2 (Multi-Instrument Portfolio).

**Dependency notation:** `depends: X.Y` means this phase cannot start until phase X.Y is complete.

**Nautilus Source** block: always read this file first before implementing. Path is relative to `~/Nautilus/nautilus_trader/nautilus_trader/`.

---

## Subsystem 1 — Data Layer (Storage + Ingestion)

> The foundation. Everything else depends on this.

| Phase | Name | Scope | Dependencies |
|-------|------|-------|-------------|
| **1.1** | TVC3 Binary Format | Types, compression, reader, writer, SHA256 verification | — |
| **1.2** | Ring Buffer | Per-file binary search, merged anchor index across files, O(log n) seek | 1.1 |
| **1.3** | TickBuffer + VPIN | Pre-decoded TradeFlowStats, VPIN bucketing, bucket_vpin | 1.2 |
| **1.4** | Bar Aggregation | Resample ticks to OHLCV bars (1s, 1m, 5m, 15m, 1h), BarBuffer | 1.3 |
| **1.5** | Multi-Instrument Data | TickBufferSet, time-ordered merge cursor, per-instrument ID routing | 1.3 |
| **1.6** | Exchange Ingestion | Binance, Bybit, OKX, Coinbase WebSocket adapters → TVC3 writer | 1.1 |
| **1.7** | Data Catalog | Track ingested files, instrument metadata, date ranges, query API | 1.6 |
| **1.8** | Full Instrument Hierarchy | 14 instrument types, margin models, funding rates, option exercise | 1.5 |
| **1.9** | Synthetic Instruments | Derived instruments from formulas, spread instruments | 1.8 |

### 1.1 — TVC3 Binary Format
**Scope:** The on-disk format for all tick storage.
- `TvcHeader` (128B `#[repr(C, packed)]`): magic, version, decimal_precision, anchor_interval, instrument_id (FNV-1a hash), start/end_time_ns, num_ticks, num_anchors, index_offset, reserved
- `AnchorTick` (30B `#[repr(C, packed)]`): timestamp_ns, price_int, size_int, side, flags, sequence
- `AnchorIndexEntry` (16B `#[repr(C, packed)]`): tick_index, byte_offset — **16 bytes, NOT 24** (matches Nautilus production)
- `pack_delta`/`unpack_delta`: 4-byte base (20-bit ts, 18-bit zigzag price, 1-bit side, 1-bit flags), 15-byte overflow (0xFF escape)
- `TvcWriter`: write_tick (anchor every anchor_interval), finalize (index + SHA256 sidecar)
- `TvcReader`: mmap + SHA256 verify, seek_to_tick binary search, decode_tick_at
- Tests: roundtrip 1M synthetic ticks byte-for-byte identical
- **Nautilus Source**: `persistence/tvc_cython_loader.pyx` (TVC3 format reference, INDEX_ENTRY_SIZE=16 at line ~57), `persistence/tvc_mmap_loader.py` (pure Python loader reference)
**Exit:** `cargo test -p tvc` passes. Files compatible with Nautilus Python/Cython loaders.

### 1.2 — Ring Buffer
**Scope:** Zero-copy TVC file access with merged cross-file seeks.
- `RingBuffer`: single-file, `Arc<Mmap>`, per-file binary search on anchors
- `RingIter`: zero-copy sequential iteration over mmap slices
- `RingBufferSet`: HashMap of RingBuffers, k-way merged anchor index (built once at startup, not per-iteration)
- `seek_to_tick(tick_index)`: binary search across merged anchors → (file_index, byte_offset) → decode forward
- `iter_range(start_ns, end_ns)`: time-ordered iteration
- **Nautilus Source**: `persistence/tvc_mmap_loader.py` (mmap patterns, seek logic)
**Note:** Merged anchor build is O(n log n) startup cost. Per-tick access is pure memory reads, no file switching.
**Exit:** `cargo test -p nexus --lib buffer` passes. Merged iterator correct tick count and order.

### 1.3 — TickBuffer + VPIN
**Scope:** Pre-decoded tick data with VPIN bucketing — decode once, iterate zero-copy.
- `TradeFlowStats`: timestamp_ns, price_int, size_int, side, cum_buy_volume, cum_sell_volume, volume_imbalance (VPIN), bucket_index
- `TickBuffer::from_ring_buffer(rb, num_buckets)`: sequential decode pass through RingBuffer, compute VPIN per bucket
- `bucket_size = total_ticks / num_buckets` (default 50 buckets)
- VPIN formula: `|cum_buy - cum_sell| / (cum_buy + cum_sell)` per bucket
- `bucket_vpin(bucket_idx)`: compute VPIN from cumulative volume deltas across bucket boundaries
- `Arc<TickBuffer>` shared across rayon workers (zero-copy per sweep iteration)
**Exit:** VPIN test passes ±1e-9. Build time < 10% of sequential decode.

### 1.4 — Bar Aggregation
**Scope:** Resample tick stream into OHLCV bars.
- `Bar`: timestamp_ns, open, high, low, close, volume, buy_volume, sell_volume, tick_count
- `BarAggregator`: rolling window aggregator (1s, 1m, 5m, 15m, 1h, 1d)
- `BarBuffer`: collection of pre-aggregated bars per instrument
- Bar mode in BacktestEngine uses pre-computed bars instead of tick iteration
- Cached: bar aggregation happens once at buffer build time, not per backtest
- **Nautilus Source**: `data/aggregation.pyx` (bar aggregation logic), `data/wranglers.pyx` (bar types)
**Exit:** Bar output matches manual tick-by-tick aggregation ±0.01 on close price.

### 1.5 — Multi-Instrument Data
**Scope:** Handle multiple instruments simultaneously in data layer.
- `TickBufferSet = Arc<HashMap<InstrumentId, Arc<TickBuffer>>>`
- `RingBufferSet = HashMap<InstrumentId, RingBuffer>` with merged cross-file anchors per instrument
- `from_date_range(instruments: Vec<InstrumentId>, start, end)` → build all instrument buffers
- `next_event()`: merge cursor across all instrument TickBuffers, return earliest (timestamp, instrument_id, tick)
- Instruments identified by (exchange, symbol) pairs, not integer IDs
**Exit:** Two instruments, interleaved ticks → correct time-ordered delivery.

### 1.6 — Exchange Ingestion
**Scope:** WebSocket adapters for exchange market data → TVC3 files.
- **Binance adapter**: WebSocket stream → normalize to TVC tick format. Handle timestamp precision (ms before 2025-01-01, μs after). Ingest SPOT and USDT-M futures.
- **Bybit adapter**: WebSocket v3 streams, tick normalization
- **OKX adapter**: WebSocket streams, tick normalization
- **Coinbase adapter**: Level2 order book + matches
- `TvcWriter::checkpoint()`: flush every N ticks (default 100K) to avoid memory buildup
- Backpressure handling: if writer falls behind, drop or queue
- Reconnection: exponential backoff on WebSocket disconnect
- **Nautilus Source**: `adapters/binance/` (Binance WebSocket normalization), `adapters/bybit/`, `adapters/okx/`, `adapters/coinbase/`
**Exit:** Ingest 1M Binance ticks → write TVC3 → read back → count matches.

### 1.7 — Data Catalog
**Scope:** Metadata database for all ingested TVC files.
- Catalog stores: (instrument_id, start_time, end_time, num_ticks, file_path, checksum)
- Query API: `catalog.query(instrument, start, end)` → list of TVC files covering range
- `Catalog::merge()`: when ingesting, detect overlapping ranges and merge into unified index
- Persisted to JSON or SQLite
- **Nautilus Source**: `data/catalog/` (catalog management), `data/loaders.py` (data loading patterns)
**Exit:** `catalog.query("BTCUSDT", 2024-01-01, 2024-01-07)` returns list of daily TVC files.

### 1.8 — Full Instrument Hierarchy
**Scope:** All instrument types from Nautilus with their specific behaviors.
- **Instrument types** (14 total):
  - `CurrencyPair` — FX pairs (pip conventions, lot size, overnight swap)
  - `CryptoFuture` — BTC-PERP, ETH-PERP etc (funding rate, settlement)
  - `CryptoPerpetual` — USDT-M linear perps (funding rate, mark price)
  - `CryptoOption` — option contracts (exercise style, delta)
  - `FuturesContract` — traditional futures (expiry, settlement price)
  - `Commodity` — gold, oil (unit, tick size)
  - `Equity` — stocks (dividend adjustments, corporate actions)
  - `Betting` — sports betting (odds, stake)
  - `BinaryOption` — binary outcomes
  - `Cfd` — contract for difference
  - `FuturesSpread` — calendar spreads
  - `Index` — equity index
  - `Synthetic` — derived from formula (see Phase 1.9)
  - `TokenizedAsset` — tokenized securities
- **Per-type behaviors**: tick_size, lot_size, margin_rate, funding_rate, exercise_style (American/European), settlement_type
- **InstrumentRegistry**: lookup instrument by ID, validate instrument compatibility
- **Nautilus Source**: `model/instruments/` (all 14 instrument type files: `currency_pair.pyx`, `crypto_perpetual.pyx`, `futures_contract.pyx`, `option_contract.pyx`, etc.), `model/identifiers.pyx` (InstrumentId)
**Exit:** InstrumentRegistry loads all 14 types. Margin rate applied correctly per type. Funding rate applied per Perpetual tick.

### 1.9 — Synthetic Instruments
**Scope:** Instruments derived from formulas of other instruments.
- `SyntheticInstrument`: formula e.g., `spread = instrument_A - instrument_B`
- Formula parsing: `A - B`, `A / B`, `(A + B) / 2`
- Synthetic prices computed on-demand from constituent prices
- Arbitrage strategies: trade spread when it deviates from fair value
- **Nautilus Source**: `model/instruments/synthetic.pyx`
**Exit:** Synthetic BTC-ETH spread from BTCUSDT + ETHUSDT. Spread trades when deviation > 2σ.

---

## Subsystem 2 — Backtesting Engine

> Depends entirely on Subsystem 1 (Data Layer). Cannot build until 1.3+ are done.

| Phase | Name | Scope | Dependencies |
|-------|------|-------|-------------|
| **2.1** | Core Engine (Single Instrument) | Tick/bar mode dispatch, EngineContext, position, equity, commission math | 1.3 |
| **2.2** | VPIN Slippage Model | compute_fill_delay, fill price simulation, adverse selection | 2.1 |
| **2.3** | SL/TP + Order Management | Stop-loss, take-profit, pending orders, auto-close | 2.1 |
| **2.4** | Multi-Instrument Portfolio Engine | Portfolio state, per-instrument positions, portfolio equity | 1.5, 2.1 |
| **2.5** | L2 Order Book Simulation | Synthetic LOB reconstruction from tick stream, bid/ask spread modeling | 1.4 |
| **2.6** | Parameter Sweeps (Portfolio) | Rayon parallel grid search, Arc<TickBufferSet> shared across workers | 2.4 |
| **2.7** | Monte Carlo + Walk-Forward | Random trade shuffling, walk-forward analysis, out-of-sample validation | 2.6 |
| **2.8** | OrderEmulator (Queue + Limit Fill) | Queue position, time-priority matching, sophisticated limit order simulation | 1.4, 2.2 |

### 2.1 — Core Engine (Single Instrument)
**Scope:** The main backtest loop.
- `BacktestEngine::run()` → dispatch to tick mode or bar mode
- `EngineContext`: position (long/short/flat), equity, peak_equity, max_drawdown, equity_curve, trades
- Commission: applied on BOTH entry AND exit. Long: `(price - entry) * size`. Short: `(entry - price) * size`
- `process_signal(signal)`: Buy/Sell/Close → update position, record trade
- Auto-close: open position closed at end of data
- Bar mode: iterate pre-computed bars, call `strategy.on_bar()`
- **Nautilus Source**: `backtest/engine.pyx` (BacktestEngine class, lines 217+, run() at line 1310+)
**Exit:** Commission test (entry $100, exit $110, size=1, comm=0.001 → net PnL $9.99 ± $0.001). Position lifecycle test (long → partial close → full close).

### 2.2 — VPIN Slippage Model
**Scope:** Realistic fill simulation using Volume-synchronized Probability of Informed Trading.
- `compute_fill_delay(order_size_ticks, vpin, avg_tick_duration_ns) → (delay_ns, impact_bps)`
  - `adverse_prob = vpin * 0.5`
  - `delay_ticks = ceil(order_size_ticks * adverse_prob * 1.5)`
  - `delay_ns = min(delay_ticks * avg_tick_duration, 200_000_000)` (200ms cap)
  - `size_impact = min(sqrt(order_size_ticks)/100, 10.0)` (max 10bps)
  - `vpin_impact = vpin * 5.0` (max 5bps at vpin=1.0)
  - `impact_bps = min(size_impact + vpin_impact, 15.0)` (15bps total cap)
- `fill_price = market_price_at(timestamp + delay_ns) * (1.0 + impact_bps / 10000.0)`
- Constants exposed as `SlippageConfig` for future calibration
**Exit:** Large order at high VPIN → fill price > market price by ≥0.1bps.

### 2.3 — SL/TP + Order Management
**Scope:** Stop-loss, take-profit, and pending order handling.
- SL/TP checked **every tick** (not per bar)
- Pending limit orders: stored in `EngineContext`, filled when market crosses price
- `Order`: id, instrument, side, order_type (market/limit/stop), price, size, sl, tp
- Auto-close at end of data (flat any open positions at last tick)
- **Nautilus Source**: `model/orders/` (all order types: `market.pyx`, `limit.pyx`, `stop_market.pyx`, `stop_limit.pyx`, `trailing_stop_market.pyx`), `trading/strategy.pyx` (order handling)
**Exit:** SL triggered mid-tick → position closed at correct price ±1 tick. TP similarly.

### 2.4 — Multi-Instrument Portfolio Engine
**Scope:** Run backtests across multiple instruments simultaneously.
- `Portfolio`: manages `HashMap<InstrumentId, InstrumentState>`
- `InstrumentState`: position, pending_orders, equity, unrealized_pnl
- `BacktestEngine::run_portfolio(buffer_set, strategy, portfolio)` → time-ordered tick delivery
- Portfolio equity = sum of all instrument equities
- Per-instrument equity tracked separately
- Strategy: `on_trade(instrument_id, tick, ctx)` called per instrument tick
- **Nautilus Source**: `portfolio/portfolio.pyx` (multi-instrument portfolio management), `model/position.pyx` (position tracking)
**Exit:** Two instruments both traded → portfolio equity = sum of both ± $0.01. Time-ordering verified.

### 2.5 — L2 Order Book Simulation
**Scope:** Reconstruct synthetic limit order book from tick stream for spread modeling.
- `OrderBook`: bids (price, size), asks (price, size), derived spread and mid-price
- Update from each tick: if tick.side=Buy + trade → remove size from bid side; if Sell → remove from ask
- Synthetic LOB reconstruction when no L2 data available (e.g., Binance SPOT)
- Spread model: `spread = f(vpin, avg_tick_size, volatility)` for simulation
- L2 mode in engine: strategy can call `ctx.order_book(instrument_id)` → get current bid/ask
- **Nautilus Source**: `model/book.pyx` (order book structure), `execution/emulator.pyx` (OrderEmulator, sophisticated L2 simulation)
**Note:** Phase 2.8 (OrderEmulator) goes much deeper. Start with basic LOB here.
**Exit:** L2 reconstructed from tick stream → spread matches observable data ±0.1bps.

### 2.6 — Parameter Sweeps (Portfolio)
**Scope:** Rayon parallel grid search across parameter space.
- `Arc<TickBufferSet>` shared across workers (zero-copy, not cloned)
- `Strategy: Clone` bound — each combo gets fresh strategy via `clone_box()`, dropped on completion
- `run_grid(grid, filters, rank_by, top_n)` → `par_iter().filter_map()`
- Each worker: 1 combo = full multi-instrument portfolio backtest
- Filter conditions (AND logic): e.g., `sharpe > 1.0 AND max_drawdown < 0.2`
- Rank by: total_pnl, sharpe_ratio, sortino_ratio, max_drawdown, calmar_ratio
**Exit:** 100-combo sweep wall time < sequential_time / num_cpus × 1.2. Results match sequential baseline.

### 2.7 — Monte Carlo + Walk-Forward
**Scope:** Statistical validation of strategy robustness.
- **Monte Carlo**: shuffle trade sequence, recompute equity curve. Run 1000+ iterations. Report distribution of Sharpe/Sortino.
- **Walk-Forward**: rolling window optimization (e.g., 6-month in-sample, 1-month out-of-sample). Compare in-sample vs out-of-sample performance degradation.
- Both use sweep runner infrastructure
**Exit:** Monte Carlo with 1000 iterations runs in <10x single backtest time. Walk-forward produces degradation metrics.

### 2.8 — OrderEmulator (Queue Position + Limit Order Fill)
**Scope:** Sophisticated order book emulation for realistic limit order fill simulation.
- **Queue position tracking**: when limit order placed, assign position in queue at that price level
- **Time-priority matching**: earlier orders at same price get filled first (FIFO)
- **Fill probability model**: limit order fill probability = f(queue_position, order_size, market_volume)
- **Partial fill modeling**: large limit order consumes multiple queue positions
- **Market impact**: large order consuming queue → price moves through levels
- **OrderEmulator** in backtest: simulates fill vs resting orders in synthetic LOB
- **Nautilus Source**: `execution/emulator.pyx` (OrderEmulator class, lines 81+), `execution/matching_core.pyx` (MatchingCore, lines 31+)
**Exit:** Limit order at level 3 with queue_position=5 → fills after 5 trades at that level consume ahead of it. Fill price matches queue position correctly.

---

## Subsystem 3 — Strategy Framework

> Depends on Subsystem 2 (engine must exist before strategies can be written against it).

| Phase | Name | Scope | Dependencies |
|-------|------|-------|-------------|
| **3.1** | Strategy Trait | Send+Sync+Clone trait, on_trade/on_bar, clone_box, StrategyCtx | 2.1 |
| **3.2** | Strategy Context | current_price, position, account_equity, pending_orders | 2.3 |
| **3.3** | Indicator Library | SMA, EMA, RSI, MACD, Bollinger Bands, ATR, VWAP, Stochastic | 3.1 |
| **3.4** | Example Strategies | EMA Cross, RSI Overbought/Oversold, Breakout, VWAP Mean Reversion | 3.3 |
| **3.5** | Strategy Optimization | CMA-ES, genetic algorithms, Bayesian optimization ( Optuna) | 2.6, 3.4 |
| **3.6** | Signals Framework | Signal subsystem via MessageBus, independent signal subscription | 3.1 |

### 3.1 — Strategy Trait
**Scope:** The interface all strategies must implement.
```rust
pub trait Strategy: Send + Sync + Clone {
    fn name(&self) -> &str;
    fn parameters(&self) -> Vec<ParameterSchema>;
    fn mode(&self) -> BacktestMode; // Tick, Bar, Hybrid
    fn subscribed_instruments(&self) -> Vec<InstrumentId>;
    fn on_trade(&mut self, instrument_id: InstrumentId, tick: &Tick, ctx: &mut dyn StrategyCtx) -> Option<Signal>;
    fn on_bar(&mut self, instrument_id: InstrumentId, bar: &Bar, ctx: &mut dyn StrategyCtx) -> Option<Signal>;
    fn on_init(&mut self) {}
    fn on_finish(&mut self) {}
    fn clone_box(&self) -> Box<dyn Strategy>;
}
```
- `Send + Sync`: safe to share across rayon threads
- `Clone`: each sweep combo gets fresh instance via `clone_box()`
- `Signal`: Buy(size, stop_loss), Sell(size, stop_loss), Close
- **Nautilus Source**: `trading/strategy.pyx` (Strategy class, lines 109+), `trading/strategy.pxd` (Cython definition)
**Exit:** Trait compiles. Example strategy implementing it compiles and runs.

### 3.2 — Strategy Context
**Scope:** What strategies can query from the engine during execution.
```rust
pub trait StrategyCtx: Send + Sync {
    fn current_price(&self, instrument_id: InstrumentId) -> f64;
    fn position(&self, instrument_id: InstrumentId) -> Option<PositionSide>; // Long/Short/Flat
    fn account_equity(&self) -> f64;
    fn unrealized_pnl(&self, instrument_id: InstrumentId) -> f64;
    fn pending_orders(&self, instrument_id: InstrumentId) -> Vec<Order>;
    fn subscribe_instruments(&mut self, instruments: Vec<InstrumentId>);
}
```
**Exit:** Strategy can query all context fields during backtest. Values match EngineContext state.

### 3.3 — Indicator Library
**Scope:** Technical indicators for use in strategies.
- **Trend**: SMA, EMA (single/double/triple), MACD, ADX, Parabolic SAR
- **Momentum**: RSI, Stochastic, CCI, Williams %R, ROC
- **Volatility**: Bollinger Bands, ATR, Standard Deviation, Keltner Channels
- **Volume**: VWAP, OBV, Volume Profile, Money Flow Index
- **Custom**: Rolling window of any OHLCV field; generic `Indicator<T>` trait for composability
- All indicators: `update(value) -> indicator_state`, `reset()`, O(1) per update
- **Nautilus Source**: `indicators/averages.pyx` (SMA, EMA), `indicators/momentum.pyx` (RSI, Stochastic), `indicators/trend.pyx` (MACD, ADX), `indicators/volatility.pyx` (Bollinger, ATR), `indicators/volume.pyx` (VWAP, OBV)
**Exit:** Each indicator produces correct values against known dataset. EMA(20) of [1,2,3,...,20] matches reference.

### 3.4 — Example Strategies
**Scope:** Reference implementations demonstrating the strategy API.
- **EMA Cross**: fast_ema vs slow_ema crossover → buy/sell signal
- **RSI Overbought/Oversold**: RSI < 30 → buy, RSI > 70 → sell
- **Breakout**: price突破20日高点 → buy, 跌破20日低点 → sell
- **VWAP Mean Reversion**: price > VWAP + 2σ → sell, price < VWAP - 2σ → buy
- Each strategy: complete, runnable, with default parameters
- **Nautilus Source**: `examples/backtest/crypto_ema_cross_ethusdt_trade_ticks.py`, `examples/backtest/fx_ema_cross_audusdt_bars_from_ticks.py` (strategy examples)
**Exit:** Each strategy compiles and runs a backtest producing non-trivial equity curves.

### 3.5 — Strategy Optimization
**Scope:** Advanced parameter optimization beyond grid search.
- **CMA-ES**: covariance matrix adaptation evolution strategy (libcmaes or manual)
- **Genetic Algorithm**: selection, crossover, mutation, population
- **Bayesian Optimization**: Gaussian Process surrogate model (using `rs` or `bayesian` crate)
- Integration with sweep runner: replace grid with optimizer
- Optuna integration for Rust (or Python interop)
**Exit:** CMA-ES finds better Sharpe than grid search on EMA cross strategy.

### 3.6 — Signals Framework
**Scope:** Decoupled signal subscription independent of strategy callbacks.
- `Signal`: named signal (e.g., "RSI_OVERSOLD", "BB_LOW") with value and timestamp
- `SignalBus`: pub/sub channel for signals, registered via `StrategyCtx::subscribe_signal()`
- Strategy subscribes to signals → receives via callback when signal fires
- Use case: indicators in separate module produce signals, strategy combines them
- Difference from `Signal` in Phase 3.1: Phase 3.1 `Signal` is a trading action (Buy/Sell/Close). Phase 3.6 `Signal` is a data event (RSI crossing threshold)
- **Nautilus Source**: `common/signal.py` (Signal definitions), `common/actor.pyx` (Actor with message bus), `trading/strategy.pyx` (signal subscription patterns)
**Exit:** Strategy subscribes to "RSI_OVERSOLD" signal. Indicator emits signal when RSI < 30. Strategy receives callback and acts.

---

## Subsystem 4 — Execution + Risk

> Depends on Subsystem 2 (engine) and ideally Subsystem 3 (strategies).

| Phase | Name | Scope | Dependencies |
|-------|------|-------|-------------|
| **4.1** | Order Types | Market, limit, stop, stop-limit, iceberg, TWAP, VWAP, trailing stop | 2.3 |
| **4.2** | Order Matching | Price-time priority matching, fill computation, partial fills | 4.1 |
| **4.3** | Position Sizing + Risk | Fixed, Kelly criterion, volatility-based (ATR), risk-parity | 2.4, 4.2 |
| **4.4** | Risk Controls | Max position size, max drawdown, circuit breaker | 4.3 |
| **4.5** | VPIN Slippage Calibration | Calibrate VPIN constants against real market data | 2.2 |
| **4.6** | Margin System | MarginModel, StandardMargin, LeveragedMargin, MarginAccount | 1.8, 4.4 |

### 4.1 — Order Types
**Scope:** All order types supported by Nautilus.
- **Market**: execute immediately at best available price
- **Limit**: execute at specified price or better
- **Stop**: triggered when market crosses stop price → becomes market order
- **Stop-limit**: triggered → becomes limit order at stop-limit price
- **Iceberg**: hidden size, display only fraction, reload as filled
- **TWAP**: time-weighted average price, slice into intervals
- **VWAP**: volume-weighted average price, slice proportional to expected volume
- **Trailing stop**: dynamic stop that follows price by offset
- All orders: validate size, price, instrument before submission
- **Nautilus Source**: `model/orders/market.pyx`, `model/orders/limit.pyx`, `model/orders/stop_market.pyx`, `model/orders/stop_limit.pyx`, `model/orders/trailing_stop_market.pyx`, `model/orders/trailing_stop_limit.pyx`, `model/orders/list.pyx` (order lists)
**Exit:** Each order type executes correctly in simulated backtest.

### 4.2 — Order Matching
**Scope:** Simulated order book matching engine.
- **Price-time priority**: earlier orders at same price get filled first
- **Fill simulation**: for market orders, walk the book; for limit orders, match against resting orders
- **Partial fills**: large orders consume multiple levels of the book
- **Slippage reporting**: record actual fill price vs quote price for each order
- Maker/taker fee modeling: fee on maker (limit order) vs taker (market order)
- **Nautilus Source**: `execution/matching_core.pyx` (MatchingCore, lines 31+), `execution/engine.pyx` (ExecutionEngine)
**Exit:** Large market order partially fills across 3 price levels. Fill price, size, and fees correct.

### 4.3 — Position Sizing + Risk
**Scope:** How position size is determined per signal.
- **Fixed size**: constant quantity per trade
- **Fixed value**: constant notional per trade
- **Kelly criterion**: f* = (bp - q) / b where p=win rate, b=odds, q=1-p
- **Volatility-based**: size = target_risk / ATR; target_risk = fixed % of equity × ATR
- **Risk-parity**: equal risk contribution across all positions
- `SizingConfig` passed to engine, applied per signal
- **Nautilus Source**: `risk/sizing.pyx` (PositionSizer, FixedRiskSizer), `risk/config.py`
**Exit:** Volatility-based sizing: with 1% equity risk and ATR=100, size = equity × 0.01 / 100.

### 4.4 — Risk Controls
**Scope:** Pre-trade and intraday risk management.
- **Position limits**: max position size per instrument, max notional exposure
- **Drawdown circuit breaker**: halt new entries if equity drawdown > X%
- **Per-order risk check**: reject order if it would breach max drawdown
- **Daily loss limit**: if daily PnL < -X%, disable new orders for the day
- Risk controls in backtest AND live execution (same code)
- **Nautilus Source**: `risk/engine.pyx` (RiskEngine, lines 77+), `risk/config.py`
**Exit:** Drawdown > 10% → no new orders accepted. Equity at -9.9% → orders allowed. -10.1% → blocked.

### 4.5 — VPIN Slippage Calibration
**Scope:** Calibrate VPIN model constants against real fill data.
- Collect real market data with known order sizes and actual fills
- Fit: adverse_prob factor (0.5 default), delay multiplier (1.5 default), impact factors
- `SlippageConfig` becomes tunable with fitted parameters
- Report calibration quality: R² of predicted vs actual fill deviation
- Benchmark: calibrated VPIN slippage should predict actual fills better than flat slippage
**Exit:** Calibrated VPIN: predicted fill vs actual fill within 1bps on >80% of orders.

### 4.6 — Margin System
**Scope:** Full margin modeling for leveraged instruments.
- **MarginModel trait**: `compute_margin(position_size, entry_price, margin_rate) -> margin_required`
- **StandardMarginModel**: standard margin = notional × margin_rate
- **LeveragedMarginModel**: isolated margin per position, cross-margin option
- **MarginAccount**: tracks margin used, margin available, margin call events
- **Margin call**: when margin_used / equity > threshold → liquidation warning → liquidation
- **Perpetual funding**: funding payments every 8 hours (or configurable interval)
- **Nautilus Source**: `accounting/margin_models.pyx` (MarginModel, StandardMarginModel, LeveragedMarginModel), `accounting/accounts/margin.pyx` (MarginAccount), `accounting/calculators.pyx` (RolloverInterestCalculator for funding)
**Exit:** Open 10x leveraged LONG on BTC-PERP at $100K → margin_used = $10K. Price drops to $90K → liquidation at $90K × 10x = $9K margin call.

---

## Subsystem 5 — Live Trading

> Depends on Subsystem 4 (execution) and ideally Subsystem 1.6 (exchange adapters).
> Subsystem 5.0a/b/c (Actor/Msgbus/Clock/Cache) are architectural foundations that MUST be built before 5.1.

| Phase | Name | Scope | Dependencies |
|-------|------|-------|-------------|
| **5.0a** | Actor + MessageBus Architecture | Actor, Component, MessageBus, endpoint registration, pub/sub | 4.4 |
| **5.0b** | Clock + Time Events | TestClock, RealClock, TimeEvent, scheduled events | 5.0a |
| **5.0c** | Cache + Database Layer | Cache (in-memory), Database (Redis/ilia/dyn), state persistence | 5.0a |
| **5.1** | Paper Trading | Simulated execution using live data, no real orders | 1.6, 4.2, 5.0a, 5.0b |
| **5.2** | Live Execution | Submit real orders to exchange via REST/WebSocket | 5.1, 5.0c |
| **5.3** | Order Management System | Track open orders, fills, positions in real-time | 5.2 |
| **5.4** | Multi-Exchange Support | Unified interface across exchanges, handle differences | 5.3 |

### 5.0a — Actor + MessageBus Architecture
**Scope:** The architectural foundation for all live/paper trading components.
- **Actor**: base class for all components (strategies, data clients, execution clients). Has name, ID, message bus, clock, logger.
- **Component**: more lightweight than Actor, for simpler components
- **MessageBus**: pub/sub message routing. Topics: data feeds, order updates, signal events.
  - `bus.subscribe(topic, endpoint)` — register handler
  - `bus.publish(topic, message)` — send message
  - `bus.register(endpoint, handler)` — named endpoint for direct calls
- **NautilusSource**: `common/actor.pyx` (Actor class), `common/component.pyx` (Component), `common/messages.pyx` (MessageBus)
- All components (data clients, execution clients, strategies, risk) are Actors
- **Nautilus Source**: `common/actor.pyx` (lines 1+), `common/component.pyx`, `common/messages.pyx`, `core/message.pyx`
**Note:** This is the hardest architectural phase. Nautilus built this over years. Take time to get it right.
**Exit:** Actor A can send message to Actor B via MessageBus. Both actors have clocks and loggers.

### 5.0b — Clock + Time Events
**Scope:** Time abstraction for test/real/live clock modes.
- **Clock trait**: `timestamp_ns()` → current time in nanoseconds
- **RealClock**: wall-clock time
- **TestClock**: controllable time, advance via `advance_to(timestamp_ns)` or `advance_by(duration_ns)`
- **TimeEvent**: scheduled event with callback. `clock.schedule(event, delay_ns, handler)`
- **TimeEventAccumulator**: collects due events, dispatches on `advance_to()`
- Backtest engine uses TestClock for deterministic time progression
- Live trading uses RealClock
- **Nautilus Source**: `common/actor.pyx` (clock integration), `core/datetime.pyx` (datetime utilities)
**Exit:** TestClock.advance_by(1 hour). All scheduled events within that hour fire. Order expires correctly.

### 5.0c — Cache + Database Layer
**Scope:** In-memory cache and persistent database for live trading state.
- **Cache**: in-memory key-value store for fast reads. Stores: instruments, positions, orders, account state.
  - `Cache::get(key) → Option<Value>`
  - `Cache::set(key, value)`
  - `Cache::update(key, f)` — atomic update
- **Database**: persistent storage (Redis, ilia, or dyn). Interface:
  - `db.save_orders(orders)`, `db.load_orders() → Vec<Order>`
  - `db.save_positions(positions)`, `db.load_positions()`
  - `db.save_equity(equity_curve)`, `db.load_equity()`
- Used for: restart recovery, state reconciliation, multi-session continuity
- **Nautilus Source**: `cache/cache.pyx` (CacheFacade), `cache/database.pyx` (Database), `cache/facade.pyx` (CacheFacade interface)
**Exit:** Kill and restart process → positions and orders restored from database. Cache miss → database fallback.

### 5.1 — Paper Trading
**Scope:** Simulated trading using live market data.
- Strategy connects to live WebSocket feed (same as ingestion)
- Signals generated by strategy → executed against simulated book (uses 4.2 matching)
- Paper results tracked separately: paper_equity, paper_trades
- `PaperBroker`: wraps live data feed, returns simulated fills
- Useful for: strategy validation before live deployment
- **Nautilus Source**: `trading/trader.py` (Trader class, orchestration), `trading/node.py` (trading node setup)
**Exit:** Strategy running on live data produces paper trades and equity curve.

### 5.2 — Live Execution
**Scope:** Submit real orders to exchange.
- **REST adapter**: place order, cancel order, get order status, get positions
- **WebSocket adapter**: receive fills, position updates, order status updates
- Order submission: validate → send to exchange → track pending → receive fill → update position
- Connection handling: reconnect on disconnect, maintain order book locally
- Rate limiting: respect exchange API rate limits
- **Nautilus Source**: `execution/client.pyx` (ExecutionClient), `execution/engine.pyx` (ExecutionEngine), `adapters/binance/` (live adapter examples)
**Exit:** Place market order on Binance → order_id returned → fill received via WebSocket → position updated.

### 5.3 — Order Management System (OMS)
**Scope:** Central position and order state machine.
- `OMS`: maintains canonical view of all open orders and positions
- Processes: new_order → pending → filled/partial/cancelled
- Reconciliation: compare OMS state with exchange state on reconnect
- Split orders: parent order → child orders (for ICEBERG, TWAP, VWAP)
- Position tracking: open_position, update_position, close_position
- **Nautilus Source**: `execution/manager.pyx` (ExecManager), `execution/reports.py` (fill reports)
**Exit:** OMS position matches exchange-reported position after reconnect reconciliation.

### 5.4 — Multi-Exchange Support
**Scope:** Unified abstraction across exchange implementations.
- `Exchange` trait: `place_order`, `cancel_order`, `get_positions`, `subscribe_trades`, `subscribe_orders`
- Implement for: Binance, Bybit, OKX, Coinbase
- Exchange-specific quirks: order ID format, rate limits, WebSocket message format
- `ExchangeRouter`: route requests to correct exchange based on instrument
- **Nautilus Source**: `adapters/` (all exchange adapters: `binance/`, `bybit/`, `okx/`, `coinbase/`, `interactive_brokers/`, etc.)
**Exit:** Same strategy code switches from Binance to Bybit by changing config only.

---

## Subsystem 6 — Python API

> Independent of other subsystems — can be built in parallel once core exists.

| Phase | Name | Scope | Dependencies |
|-------|------|-------|-------------|
| **6.1** | PyO3 Bindings | Rust core exposed to Python via PyO3 | 2.1 |
| **6.2** | Python Strategy Wrapper | Write strategies in Python, run on Rust engine | 3.1, 6.1 |
| **6.3** | Nautilus Data Interop | Load Nautilus Parquet/Arrow data into Nexus | 1.1, 6.1 |
| **6.4** | Jupyter Support | Run backtests in Jupyter notebooks | 6.2 |

### 6.1 — PyO3 Bindings
**Scope:** Expose Rust core to Python.
- `PyBacktestEngine`, `PyTickBuffer`, `PyStrategy` — Python-callable classes
- PyO3 `#[pymethods]`: `run_backtest`, `run_sweep`, `load_tvc`
- `PyStrategy`: subclass in Python, implement `on_trade` in Python, runs in Rust engine
- Thread safety: GIL management, `Send + Sync` enforced
- Build: `maturin` for PyO3 + Rust publishing to PyPI
- **Nautilus Source**: `core/rust/nautilus_pyo3.pyi` (PyO3 interface definition)
**Exit:** Python `from nexus import BacktestEngine; engine.run()` works end-to-end.

### 6.2 — Python Strategy Wrapper
**Scope:** Allow strategies to be written in Python.
- `class MyStrategy(Strategy)`: Python class implementing `on_trade(self, instrument, tick)`
- Translated to `Box<dyn Strategy>` via PyO3 wrapper
- Works with sweep runner: Python strategies included in parameter sweeps
**Exit:** Python EMA cross strategy runs via Rust engine, produces correct signals.

### 6.3 — Nautilus Data Interop
**Scope:** Load existing Nautilus Parquet/Arrow data into Nexus.
- `NexusCatalog.from_nautilus(path)`: read Nautilus Parquet files, convert to TVC3
- Or: direct Parquet reader in Nexus (bypass TVC3 conversion)
- Maintain instrument metadata compatibility
- **Nautilus Source**: `persistence/wranglers.pyx` (Parquet reading), `persistence/arrow/` (Arrow format)
**Exit:** Nautilus Parquet file → Nexus TickBuffer → backtest runs with identical results.

### 6.4 — Jupyter Support
**Scope:** First-class Jupyter notebook experience.
- `%load_ext nexus` magic for Jupyter
- `nexus.plot_equity()`, `nexus.plot_drawdown()`, `nexus.summary_stats()`
- Inline visualization: equity curve, drawdown, trade markers on price chart
- Integration with matplotlib/seaborn for custom charts
**Exit:** `nexus.summary_stats()` produces Sharpe, Sortino, max_drawdown in notebook cell.

---

## Subsystem 7 — Analytics + Reporting

> Depends on Subsystem 2 (engine produces trades) and ideally 6 (Python for viz).

| Phase | Name | Scope | Dependencies |
|-------|------|-------|-------------|
| **7.1** | Performance Metrics | Sharpe, Sortino, Calmar, max_drawdown, win rate, profit factor | 2.1 |
| **7.2** | Trade Attribution | Per-trade PnL, per-instrument PnL, per-day PnL | 2.4, 7.1 |
| **7.3** | Equity Curve Analysis | Rolling Sharpe, rolling max drawdown, underwater curve | 7.1 |
| **7.4** | Report Generation | JSON/HTML report with charts and stats | 7.3 |
| **7.5** | Detailed Fill Reports | Commission + slippage + queue position attribution per fill | 2.8, 7.2 |

### 7.1 — Performance Metrics
**Scope:** Standard quant performance metrics.
- **Sharpe Ratio**: (annualized return) / (annualized volatility)
- **Sortino Ratio**: (annualized return) / (downside deviation)
- **Calmar Ratio**: (annualized return) / (max_drawdown)
- **Max Drawdown**: peak-to-trough decline, duration
- **Win Rate**: % of profitable trades
- **Profit Factor**: gross profit / gross loss
- **Expectancy**: p(win) × avg_win - p(loss) × avg_loss
- All computed from `BacktestResult.trades` and `equity_curve`
**Exit:** Metrics match manual calculation from trade log. Sharpe = (mean_daily_return / std_daily_return) × sqrt(252).

### 7.2 — Trade Attribution
**Scope:** Break down performance by dimension.
- Per-trade: entry price, exit price, size, commission, net_pnl
- Per-instrument: total_pnl, win_rate, avg_trade for each instrument
- Per-day: daily_pnl, cumulative_pnl
- Heatmap: pnl by day-of-week × hour
- Export: CSV of all trades
**Exit:** Sum of per-trade net_pnl equals portfolio total return.

### 7.3 — Equity Curve Analysis
**Scope:** Time-series analysis of equity and risk.
- Rolling Sharpe (e.g., 20-trade window)
- Rolling max drawdown (peak equity - current equity)
- Underwater curve: drawdown over time
- Equity vs benchmark (buy-and-hold comparison)
**Exit:** Rolling Sharpe computed over 20-trade window matches manual calculation.

### 7.4 — Report Generation
**Scope:** Export backtest results as report.
- JSON export: full trade log, equity curve, all metrics
- HTML report: charts (equity, drawdown, monthly returns), metrics table, trade log
- PDF: print-ready version of HTML report
**Exit:** HTML report opens in browser, all charts render, metrics match JSON output.

### 7.5 — Detailed Fill Reports
**Scope:** Per-fill attribution with commission + slippage + queue position breakdown.
- Per fill record: `fill_id, timestamp, order_id, instrument, side, size, fill_price, market_price, slippage_bps, commission, queue_position, vpin_at_fill`
- Slippage breakdown: `slippage_bps = fill_price - market_price` (direction-corrected)
- Queue cost: extra slips due to queue position at time of fill
- VPIN at fill: VPIN bucket value when order was placed
- Commission breakdown: maker vs taker fee
- **Nautilus Source**: `execution/reports.py` (fill reports), `analysis/statistic.py` (performance statistics)
**Exit:** Fill report shows: fill at $100.05, market at $100.00, slippage=0.5bps, commission=$0.20, queue_position=3.

---

## Subsystem 8 — Infrastructure + DevOps

> Independent of all feature work. Can be built in parallel.

| Phase | Name | Scope | Dependencies |
|-------|------|-------|-------------|
| **8.1** | CI/CD | GitHub Actions: cargo test, cargo clippy, cargo fmt, miri | — |
| **8.2** | Benchmarking Suite | Track decode speed, backtest speed, sweep speed over time | 2.6 |
| **8.3** | Documentation | docs.rs generation, API docs, architecture docs | 1.1 |
| **8.4** | Crates.io Release | Publish tvc, nexus, nexus-strategy to crates.io | 1.1, 2.1 |

### 8.1 — CI/CD
**Scope:** Automated quality gates on every PR/commit.
- `cargo test --workspace` on every PR
- `cargo clippy --workspace` with `deny` lint level (warnings → errors)
- `cargo fmt --check` (rustfmt enforced)
- `cargo miri` for memory safety (in CI where available)
- Benchmark regression: if benchmark drops >10% vs main, fail CI
**Exit:** PR with failing test cannot be merged. Clippy warnings cause CI failure.

### 8.2 — Benchmarking Suite
**Scope:** Track performance over time.
- `criterion` benchmarks for: TVC decode, TickBuffer build, backtest tick/s, sweep throughput
- GitHub Actions: run benchmarks on main branch, upload results to artifact
- Benchmark history: track decode speed, tick/s, sweep/s over commits
- Alert if: decode speed drops, memory usage grows significantly
**Exit:** `cargo bench` runs. Results from main commit tracked in GitHub Actions.

### 8.3 — Documentation
**Scope:** Public-facing and internal docs.
- `docs.rs` for published crates (tvc, nexus, nexus-strategy)
- Architecture docs: format spec, ADR log, design decisions
- User guide: getting started, CLI reference, strategy tutorial
- Contributor guide: setting up dev environment, running tests
**Exit:** `cargo doc --no-deps` generates docs. docs.rs shows all public items with doc comments.

### 8.4 — Crates.io Release
**Scope:** Publish crates for community use.
- `cargo publish --dry-run` for all crates
- Version strategy: semver, release tags on GitHub
- CI: automatically publish to crates.io on tag push
- Minimum Rust version: MSRV documented and tested
**Exit:** `cargo add nexus` works from crates.io. Published version passes all tests.

---

## Dependency Graph (Summary)

```
1.1 (TVC3) ─────────────────┐
1.2 (RingBuffer) ← 1.1 ──────┤
1.3 (TickBuffer) ← 1.2 ──────┤
1.4 (BarAgg) ← 1.3 ──────────┤
1.5 (Multi-inst) ← 1.3 ──────┤
1.6 (Exchange ING) ← 1.1 ────┤
1.7 (DataCatalog) ← 1.6 ────┤
1.8 (InstrHierarchy) ← 1.5 ─┤
1.9 (SyntheticInstr) ← 1.8 ──┘

2.1 (CoreEngine) ← 1.3 ───────────────┐
2.2 (VPINSlip) ← 2.1 ────────────────┤
2.3 (SL/TP) ← 2.1 ──────────────────┤
2.4 (MultiInst Port) ← 1.5, 2.1 ────┤
2.5 (L2 Sim) ← 1.4 ─────────────────┤
2.8 (OrderEmulator) ← 1.4, 2.2 ─────┤
2.6 (Sweeps) ← 2.4 ──────────────────┤
2.7 (MC/WF) ← 2.6 ───────────────────┘

3.1 (StrategyTrait) ← 2.1 ─────────────┐
3.2 (StratCtx) ← 2.3 ────────────────┤
3.3 (Indicators) ← 3.1 ──────────────┤
3.4 (Examples) ← 3.3 ────────────────┤
3.5 (Optim) ← 2.6, 3.4 ─────────────┤
3.6 (Signals) ← 3.1 ──────────────────┘

4.1 (OrderTypes) ← 2.3 ───────────────┐
4.2 (Matching) ← 4.1 ────────────────┤
4.3 (Sizing) ← 2.4, 4.2 ───────────┤
4.4 (RiskCtrl) ← 4.3 ────────────────┤
4.5 (VPIN Cal) ← 2.2 ───────────────┤
4.6 (MarginSystem) ← 1.8, 4.4 ───────┘

5.0a (Actor/Msgbus) ← 4.4 ──────────────┐
5.0b (Clock/Timer) ← 5.0a ─────────────┤
5.0c (Cache/DB) ← 5.0a ───────────────┤
5.1 (PaperTrade) ← 1.6, 4.2, 5.0a, 5.0b ┤
5.2 (LiveExec) ← 5.1, 5.0c ───────────┤
5.3 (OMS) ← 5.2 ────────────────────┤
5.4 (MultiExch) ← 5.3 ───────────────┘

6.1 (PyO3) ← 2.1 ─────────────────────┐
6.2 (PyStrat) ← 3.1, 6.1 ───────────┤
6.3 (NautilusInterop) ← 1.1, 6.1 ────┤
6.4 (Jupyter) ← 6.2 ────────────────┘

7.1 (PerfMetrics) ← 2.1 ──────────────┐
7.2 (Attribution) ← 2.4, 7.1 ───────┤
7.3 (EquityAnalysis) ← 7.1 ───────────┤
7.4 (Reporting) ← 7.3 ────────────────┤
7.5 (FillReports) ← 2.8, 7.2 ─────────┘

8.1 (CI/CD) ← [independent] ───────────┐
8.2 (Benchmarks) ← 2.6 ───────────────┤
8.3 (Docs) ← 1.1 ─────────────────────┤
8.4 (CratesRelease) ← 1.1, 2.1 ───────┘
```

---

## Implementation Order (Critical Path)

```
Phase 1 order (must be sequential):
1.1 → 1.2 → 1.3 → 1.4 → 1.5 → 1.8 → 1.9
      (1.6/1.7 can run in parallel after 1.1)

Phase 2 order (must be sequential):
2.1 → 2.2 → 2.3 → 2.4 → 2.5 → [2.8 can start after 2.2+1.4] → 2.6 → 2.7

Phase 3 order:
3.1 (after 2.1) → 3.2 (after 2.3) → 3.3 → 3.4 → 3.5 (after 2.6, 3.4) → 3.6 (after 3.1)

Phase 4 order:
4.1 → 4.2 → 4.3 → 4.4 → 4.5 → 4.6 (after 1.8, 4.4)

Phase 5 order (5.0a first, then sequential):
5.0a → 5.0b → 5.0c → 5.1 → 5.2 → 5.3 → 5.4

Phase 6 (independent tracks after 2.1):
6.1 → 6.2 → 6.3 → 6.4

Phase 7 (after 2.x, 2.8 for 7.5):
7.1 → 7.2 → 7.3 → 7.4 → 7.5 (after 2.8, 7.2)

Phase 8 (all independent):
8.1 anytime, 8.2 after 2.6, 8.3 after 1.1, 8.4 after 1.1 + 2.1
```

---

## Phase Numbering Reference

| # | Subsystem |
|---|-----------|
| 1 | Data Layer (Storage + Ingestion) |
| 2 | Backtesting Engine |
| 3 | Strategy Framework |
| 4 | Execution + Risk |
| 5 | Live Trading |
| 6 | Python API |
| 7 | Analytics + Reporting |
| 8 | Infrastructure + DevOps |
