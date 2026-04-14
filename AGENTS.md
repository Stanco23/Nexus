<!-- Parent: none (root) -->
<!-- Generated: 2026-04-14 | Updated: 2026-04-14 -->

# Nexus

High-Performance Tick-By-Tick Backtesting Engine in Rust. Designed to surpass Nautilus Trader in raw backtesting performance.

## Purpose

Nexus is a Rust-native algorithmic trading platform providing:
- **TVC3 binary format** — Delta-compressed, memory-mapped tick storage (~4-5 bytes/tick)
- **Ring buffer** — Zero-copy TVC file access with merged anchor index for O(log n) random seek
- **TickBuffer** — Pre-decoded tick data with VPIN bucketing; decode once, iterate zero-copy
- **BacktestEngine** — Tick-by-tick and bar-mode backtesting with VPIN slippage for realistic fills
- **Multi-instrument portfolio** — Per-instrument tick streams, time-ordered merge cursor, cross-instrument strategies
- **Parameter sweeps** — Rayon parallel grid search with `Arc<TickBufferSet>` shared across workers
- **Full instrument hierarchy** — 14 instrument types (perpetuals, options, futures, FX, etc.)
- **Live trading** — Actor/MessageBus architecture, paper trading, real execution

**No AI. No UI. No DSL.** Plain Rust strategies via `Box<dyn Strategy>`.

## Key Files

| File | Description |
|------|-------------|
| `Cargo.toml` | Workspace root: tvc, nexus, strategy, cli |
| `CLAUDE.md` | OMC project configuration |
| `README.md` | Project overview and status |

## Subdirectories

| Directory | Purpose |
|-----------|---------|
| `libs/` | Crates: tvc, nexus, strategy (see `libs/AGENTS.md`) |
| `apps/` | Applications: CLI (see `apps/AGENTS.md`) |
| `.omc/plans/` | Architecture and roadmap plans |

## Architecture

```
libs/
├── tvc/          # TVC3 binary format (Phase 1.1)
├── nexus/        # Core engine (Phases 2.x)
└── strategy/    # Strategy trait + examples (Phases 3.x)
apps/
└── cli/         # CLI tools (Phase 8)
```

## Key Decisions (see `.omc/plans/`)

- **TVC3 16-byte anchor index** — matches Nautilus production format (`INDEX_ENTRY_SIZE = 16` at `persistence/tvc_cython_loader.pyx:57`)
- **Merged anchor index** — built once at startup, no per-iteration file switching
- **TickBuffer pre-decoded** — decode once, sweep iterations are zero-decode
- **Multi-instrument portfolio** — `TickBufferSet`, `on_trade(instrument_id, tick)`, time-ordered merge cursor
- **VPIN slippage** — `compute_fill_delay(order_size, vpin, avg_tick_duration)` → `(delay_ns, impact_bps)`
- **Actor/MessageBus** — architectural foundation for live/paper trading (Phase 5.0a)

## Build Commands

```bash
cargo build --workspace    # full workspace
cargo test -p tvc         # TVC format tests (Phase 1.1)
cargo test -p nexus        # engine tests (Phase 2.x)
cargo test --workspace     # all tests
cargo clippy --workspace   # linting
cargo fmt --check         # formatting
```

## Phase Gate

A phase is complete only when: **zero warnings + all tests pass + integration test + wired to system**.

## Roadmap

See `.omc/plans/nexus-roadmap-v2.md` for the full 49-phase roadmap covering:
- Subsystem 1: Data Layer (1.1–1.9)
- Subsystem 2: Backtesting Engine (2.1–2.8)
- Subsystem 3: Strategy Framework (3.1–3.6)
- Subsystem 4: Execution + Risk (4.1–4.6)
- Subsystem 5: Live Trading (5.0a–5.4)
- Subsystem 6: Python API (6.1–6.4)
- Subsystem 7: Analytics + Reporting (7.1–7.5)
- Subsystem 8: Infrastructure + DevOps (8.1–8.4)

## For AI Agents

### Before Implementing Any Phase
1. Read `.omc/plans/nexus-roadmap-v2.md` — phase scope, exit criteria, Nautilus source file
2. Read the referenced Nautilus source file first (path relative to `~/Nautilus/nautilus_trader/nautilus_trader/`)
3. Implement only what the phase scope describes — no speculative features

### Code Conventions
- `#[repr(C, packed)]` for all TVC3 binary structs
- `static_assertions::const_assert!` to verify struct sizes
- All public APIs must have doc comments
- Tests in `tests/` subdirectory per crate
- Integration tests must create real files/objects, not mocks

### Rust Patterns
- `Send + Sync + Clone` required for Strategy trait
- `Arc<TickBuffer>` shared across rayon workers in sweeps
- `thiserror` for error handling
- `memmap2` for memory-mapped I/O

## Dependencies

### Internal
- `libs/tvc/` — TVC3 format (Phase 1.1)
- `libs/nexus/` — Core engine
- `libs/strategy/` — Strategy trait

### Key External
- `memmap2` — memory-mapped file I/O
- `rayon` — data-level parallelism for sweeps
- `sha2` — SHA256 verification for TVC files
- `static_assertions` — compile-time struct size assertions
- `thiserror` — error handling
- `serde` — serialization

<!-- MANUAL: Custom project notes below -->

## Status

Pre-alpha. Phase 1.1 (TVC3 Binary Format) is the first implementation target.
