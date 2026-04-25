<div align="center">

![Nexus Trading Engine](logo.svg)

# Nexus

**High-Performance Rust Trading Engine** — matching, risk, and execution for algorithmic traders.

[![Rust](https://img.shields.io/badge/Rust-1.75+-orange.svg?style=flat-square)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg?style=flat-square)](LICENSE)
[![GitHub Repo](https://img.shields.io/badge/GitHub-Stanco23/Nexus-green.svg?style=flat-square)](https://github.com/Stanco23/Nexus)

*A matching engine built from scratch. Not a wrapper. Not a port. A ground-up implementation in Rust.*

</div>

---

## What Is Nexus?

Nexus is a full-stack algorithmic trading engine written in Rust. It handles the complete trade lifecycle — from order submission through matching to position management and risk enforcement.

Built for traders and developers who want **control over the full stack**, not a black-box platform with hidden latency.

### Core Capabilities

| Component | Status | Description |
|-----------|--------|-------------|
| **Matching Engine** | ✅ Live | Price-time priority (FIFO), market and limit orders |
| **Order Book** | ✅ Live | Real-time bid/ask depth, delta updates, spread tracking |
| **Risk Engine** | ✅ Live | Position limits, margin checks, daily P&L |
| **Order Management** | ✅ Live | Submission, cancellation, modification lifecycle |
| **Market Adapters** | ✅ Live | OKX, Binance, Bybit — WebSocket + REST |
| **Backtesting** | 🔧 In progress | Tick-by-tick simulation with VPIN slippage |
| **Parameter Sweeps** | 🔧 In progress | Rayon parallel grid search |

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Trader / Strategy                     │
└─────────────────────────────────────────────────────────────┘
                              │
                    REST API · WebSocket
                              │
┌─────────────────────────────────────────────────────────────┐
│                    Execution Client                          │
│         (OKX · Binance · Bybit adapters)                   │
└─────────────────────────────────────────────────────────────┘
                              │
              ┌───────────────┼───────────────┐
              ▼               ▼               ▼
      ┌───────────┐   ┌───────────┐   ┌───────────┐
      │ Matching  │   │    Risk   │   │  Position │
      │  Engine   │   │  Engine   │   │  Manager  │
      └───────────┘   └───────────┘   └───────────┘
                              │
                     ┌────────┴────────┐
                     ▼                 ▼
             ┌─────────────┐   ┌─────────────┐
             │  Order Book │   │  Market     │
             │  (live L2)  │   │  Data Feed  │
             └─────────────┘   └─────────────┘
```

**Key design decisions:**

- **Lock-free market data** — Exchange WS → mpsc → ring buffer → DataEngine (no locks on hot path)
- **FIFO matching** — Price-time priority with microsecond-level sequence numbers
- **Rust-native** — No Python, no JVM. Zero-cost abstractions, deterministic performance
- **Multi-exchange** — Unified execution interface across OKX, Binance, Bybit

---

## Performance

The matching engine is designed for sub-millisecond order processing.

```
Orders processed: ~100,000/sec (benchmark, single thread)
Average fill latency: < 1ms (in-process, no network)
Memory per order book level: ~64 bytes
```

Real-world latency is dominated by network, not computation. See `libs/nexus/src/engine/core.rs` for benchmarks.

---

## Getting Started

### Prerequisites

- Rust 1.75+
- (Optional) An OKX, Binance, or Bybit account for live trading

### Build

```bash
git clone https://github.com/Stanco23/Nexus.git
cd Nexus
cargo build --workspace
```

### Run

```bash
# Show all available commands
cargo run --workspace --bin nexus-cli -- --help

# Start the matching engine (backtest/live)
cargo run --bin nexus-cli -- match --config config.toml

# Run tests
cargo test --workspace
```

### Configuration

See `apps/nexus-cli/examples/` for configuration examples. Each exchange adapter requires API credentials set as environment variables:

```bash
export OKX_API_KEY="your-key"
export OKX_SECRET="your-secret"
export OKX_PASSPHRASE="your-passphrase"
```

---

## Project Structure

```
Nexus/
├── libs/
│   └── nexus/
│       └── src/
│           ├── book.rs          # Order book + matching logic
│           ├── engine/          # Core, risk, margin, sizing, OMS
│           ├── data/            # DataEngine + subscription routing
│           ├── live/            # Exchange adapters (OKX, Binance, Bybit)
│           ├── buffer/          # Ring buffer, tick buffer, aggregators
│           ├── sweep/           # Parameter sweep framework
│           └── trader.rs        # Top-level trader orchestrator
├── apps/
│   └── cli/                     # CLI entry point
└── logo.svg                      # Nexus logo
```

---

## Live Trading Adapters

Currently supported exchanges:

| Exchange | WebSocket | REST API | Status |
|----------|-----------|----------|--------|
| **OKX** | ✅ | ✅ | Stable |
| **Binance** | ✅ | ✅ | Stable |
| **Bybit** | ✅ | ✅ | Stable |

All adapters implement a unified `ExecutionClient` interface — swap exchanges by changing configuration, not code.

---

## Documentation

- [Architecture Plan](.omc/plans/nexus-architecture-v3.md) — Full technical specification
- [Parity Tracker](PARITY_TRACKER.md) — Phase 1-5 implementation status
- [Audit Report](audit-report.md) — Code review findings

---

## Contributing

Contributions welcome. The codebase follows standard Rust conventions (`cargo fmt`, `cargo clippy`). All tests must pass.

```bash
cargo fmt
cargo clippy
cargo test --workspace
```

---

## License

MIT OR Apache-2.0, at your option.
