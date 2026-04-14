<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-14 | Updated: 2026-04-14 -->

# cli/

Command-line tools application crate.

## Purpose

CLI tools wrapping the library crates for end users.

## Planned Commands

| Command | Purpose | Phase |
|---------|---------|-------|
| `tvc ingest` | Ingest exchange WebSocket data → TVC3 files | 1.6 |
| `tvc backtest` | Load TVC3 → run portfolio backtest → JSON | 2.1+ |
| `tvc sweep` | Load TVC3 → run parameter sweep → top-N results | 2.6 |

## For AI Agents

Do not implement CLI until core libraries are functional (at least Phase 2.1).

## Dependencies

- `tvc` — TVC3 format
- `nexus` — BacktestEngine
- `strategy` — Strategy trait

<!-- MANUAL: -->
