[<!-- OMC:START -->
<!-- OMC:VERSION:4.11.6 -->

# Nexus — High-Performance Tick-By-Tick Backtesting Engine

You are working on Nexus, a Rust-native algorithmic trading backtesting and execution platform.

## Architecture

```
nexus/
├── libs/
│   ├── tvc/           # TVC3 binary format (delta compression, mmap, seek)
│   ├── nexus/         # Core engine (ring buffer, tick buffer, backtest, portfolio, sweep)
│   └── strategy/      # Strategy trait + example strategies
└── apps/
    └── cli/           # CLI tools (ingest, backtest, sweep)
```

## Key Decisions (see .omc/plans/)

- **TVC3 16-byte anchor index** — matches Nautilus production format
- **Merged anchor index** — built once at startup, no per-iteration switching
- **TickBuffer pre-decoded** — decode once, sweep iterations are zero-decode
- **Multi-instrument portfolio** — `TickBufferSet`, `on_trade(instrument_id, tick)`, time-ordered merge cursor
- **VPIN slippage** — `compute_fill_delay(order_size, vpin, avg_tick_duration)` → delay_ns + impact_bps

## Build Commands

```bash
cargo build --workspace    # full workspace
cargo test -p tvc          # TVC format tests
cargo test -p nexus        # engine tests
cargo test --workspace     # all tests
```

## Phase Gate

A phase is complete only when ALL of:
1. Zero warnings + all tests pass
2. Integration test exists that wires the phase's consumer to its producer
3. Wiring checklist (`.omc/plans/WIRING-CHECKLIST.md`) is completed and verified
4. **Audit checklist below — all items pass for the phase being implemented**
5. **Full Nautilus feature parity — see below**

### Full Nautilus Feature Parity Requirement

**Every phase MUST implement ALL features that Nautilus has for that subsystem.** Do not skip features because they seem complex or slow. If a feature exists in Nautilus, it must exist in Nexus before the phase is marked complete.

For each phase, you MUST:
1. **Read the Nautilus source files** listed in the roadmap entry BEFORE writing any code
2. **Create a feature checklist** from the Nautilus source — enumerate every class, method, enum variant, and configuration option Nautilus has for this subsystem
3. **Verify each item** against the Nexus implementation — mark each as: ✅ implemented, ❌ missing, 🟡 partial
4. **Document gaps** in the phase's status block in the roadmap — never mark a phase complete if any item is ❌ or 🟡

**The audit report (`audit-report.md`) exists because this rule was not followed previously.** Every currently-complete phase that was marked ✅ without reading Nautilus source has gaps. Going forward, no phase may be marked complete without passing the full feature checklist.

### Feature Parity by Subsystem

When implementing a phase, compare against these Nautilus sources and verify all features:

**Data Layer (1.x):** `tvc_cython_loader.pyx`, `tvc_mmap_loader.py`, `model/instrument.pyx`, `model/synthetic.pyx`
**Backtesting (2.x):** `execution/matching_core.pyx`, `execution/emulator.pyx`, `trading/backtest.pyx`
**Strategy (3.x):** `trading/strategy.pyx`, `common/actor.pyx`, `common/component.pyx`, `indicators/`
**Execution (4.x):** `model/orders/base.pyx`, `model/orders/market.pyx`, `model/orders/limit.pyx`, `model/orders/stop_market.pyx`, `model/orders/stop_limit.pyx`, `model/orders/trailing_stop_market.pyx`, `model/orders/trailing_stop_limit.pyx`, `model/orders/market_if_touched.pyx`
**Live Trading (5.x):** `execution/client.pyx`, `execution/engine.pyx`, `trading/trader.py`, `trading/node.py`, `data/engine.pyx`, `risk/engine.pyx`, `cache/cache.pyx`

**The bar is simple: if Nautilus has it and the roadmap scope includes it, Nexus must have it. Implementation complexity is not an excuse to omit features.**

**Wiring checklist** verifies:
- Component X's public API is called by all its consumers
- Data flows from producer → consumer without dropped messages
- State transitions are propagated (e.g., `RiskEngine.on_trade` called after fills)
- No hardcoded fake IDs in live or paper paths
- No `Arc<Mutex<T>>` fields that are never used in the struct
- No `fn on_*` handlers that remain no-op stubs after the phase

**Every phase plan must include a completed wiring checklist before it is considered done.**

## Nautilus Source Convention

Before implementing any phase, read the referenced Nautilus source file(s). Path convention: `~/Nautilus/nautilus_trader/nautilus_trader/<path>`. This prevents stub code and ensures Nexus behavior matches Nautilus production. The roadmap entry for each phase lists the specific source files to read first.

## Audit-Driven Implementation Checklist

**Before declaring a phase complete, verify ALL of the following:**

### Per-Component Checklist
- [ ] Trait methods: ALL declared methods are implemented (not just declared) — check for `unimplemented!()`, `todo!()`, or no-op bodies
- [ ] `EngineContext` / `BacktestEngine` / etc. actually CALL the trait methods they claim to implement
- [ ] No `Arc<Mutex<T>>` fields that are never used
- [ ] No `fn on_*` handlers that remain no-op stubs after the phase
- [ ] State transitions propagated (e.g., `RiskEngine.on_trade` called after fills)
- [ ] No hardcoded fake IDs in live or paper paths

### Strategy Framework Checklist
- [ ] `EngineContext` implements `StrategyCtx` (all 10 methods)
- [ ] `SignalBus` is present in `BacktestEngine` and published to in the tick loop
- [ ] `on_start`, `on_stop`, `on_reset` lifecycle hooks exist on Strategy trait
- [ ] `clock` access exists on strategy context
- [ ] Indicator `update` methods return real values (not always `None`)
- [ ] `clone_box` works correctly for all strategy types

### Order/Execution Checklist
- [ ] Order types match Nautilus enum exactly (TrailingStopMarket, TrailingStopLimit, etc.)
- [ ] `MatchingCore` or equivalent price-time priority matching exists (not just probabilistic queue)
- [ ] `LiquiditySide` enum exists (not just `is_maker: bool`)
- [ ] `TimeInForce` / `expire_time` on Order struct
- [ ] `PostOnly` and `ReduceOnly` flags
- [ ] `ContingencyType` and linked order IDs

### Live Trading Checklist
- [ ] `Trader` struct exists with `add_actor`, `start`, `stop` methods
- [ ] `DataEngine` registered as MsgBus component
- [ ] `RiskEngine` registered as MsgBus component
- [ ] `BinanceMarketDataAdapter` implemented for quote/OB routing
- [ ] `ExchangeRouter` for multi-venue routing
- [ ] Bybit/OKX `get_order_status` returns real `executed_qty` (not hardcoded "0")
- [ ] `DataEngine.replay()` is wired or removed (no dead code)

### Synthetic Instruments Checklist
- [ ] N-component formula support (not hardcoded to 2)
- [ ] `price_precision` field
- [ ] `price_increment` property
- [ ] Registered in `InstrumentRegistry`
- [ ] Formula validation at construction

### Order Book Checklist
- [ ] `OrderBookDeltas` incremental processing (not full rebuild from trade stream)
- [ ] `MatchingCore` price-time priority matching
- [ ] Queue depth metadata from `walk_book`

### Audit Reference
- Full audit report: `audit-report.md` (at project root)
- Critical gaps by phase: C1 (5.7 Trader), C2 (3.6 SignalBus), C3 (3.2 StrategyCtx)
- High gaps: H1-H19 — see audit report for detail

## CODING RULES
- Before every phase planning, you must reference `.omc/plans/nexus-roadmap-v3.md` file.
- Every phase that you implement should be compared with Nautilus Trader for parity. EVERYTHING matters, every single feature, vendor support, platform support. Nothing can be glossed over as 'We don't need that.'
- Every phase shall only be planned with /ralplan and executed with /ralph by default.
- After every phase you must update ONLY that particular section in `.omc/plans/nexus-roadmap-v3.md`.
- You are prohibited from modifying the architecture if not directly prompted by the user to do so.

## Verification criteria
- Compare that particular phase with Nautilus's solution. Not just one file but the whole pipeline.
- If any bugs are found you automatically enter autopilot mode, invoke the ralplan skill and devide the task into smaller digestable tasks before allocating appropriate agents to fix bugs, such as cyrcle dependancy, missing features or duplicate code.
- Do not change code if Nexus's architecture in that regard is fundamentally different from Nautilus's
- Every major phase completion like full 1.1-2.1 you need to perform a deep dive in the code and dispatch agents to explore both nexus and Nautilus codebases to check parity and update the roadmap `.omc/plans/nexus-roadmap-v3.md` if we need to go back to a feature marked as implemented even though it's not fully implemented.

## No AI/UI/DSL

Core engine only. No AI features, no UI, no strategy DSL. Plain Rust strategies via `Box<dyn Strategy>`.

<!-- OMC:END -->
