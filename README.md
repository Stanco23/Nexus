# Nexus

Nexus — High-Performance Tick-By-Tick Backtesting Engine

Nexus is a Rust-native algorithmic trading platform designed to surpass Nautilus Trader in raw backtesting performance.

## Status

Pre-alpha. Initial workspace scaffold established.

## Architecture

- **TVC3 binary format** — Delta-compressed, memory-mapped tick storage (~4-5 bytes/tick)
- **Ring buffer** — Zero-copy TVC file access with merged anchor index for O(log n) random seek
- **TickBuffer** — Pre-decoded tick data with VPIN bucketing; decode once, iterate zero-copy
- **BacktestEngine** — Tick-by-tick and bar-mode backtesting with VPIN slippage for realistic fills
- **Multi-instrument portfolio** — Per-instrument tick streams, time-ordered merge cursor, cross-instrument strategies
- **Parameter sweeps** — Rayon parallel grid search with `Arc<TickBufferSet>` shared across workers

## Roadmap

See `.omc/plans/nexus-architecture-v3.md` for the full architecture plan.

Phases:
1. TVC3 binary format
2. Ring buffer + merged anchors
3. TickBuffer + VPIN
4. Backtest engine (single instrument)
5. Multi-instrument portfolio
6. Parameter sweeps
7. Strategy trait + CLI + ingestion
8. (Future) Live trading, risk management, Python API

## Building

```bash
cargo build --workspace
cargo test --workspace
```

## License

MIT OR Apache-2.0
