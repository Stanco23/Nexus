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

A phase is complete only when: zero warnings + all tests pass + integration test + wired to system.

## No AI/UI/DSL

Core engine only. No AI features, no UI, no strategy DSL. Plain Rust strategies via `Box<dyn Strategy>`.

<!-- OMC:END -->
