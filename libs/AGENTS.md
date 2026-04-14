<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-14 | Updated: 2026-04-14 -->

# libs/

Rust crates that form the core of Nexus. All are workspace members.

## Subdirectories

| Directory | Crate | Phase | Purpose |
|-----------|-------|-------|---------|
| `tvc/` | `tvc` | 1.1 | TVC3 binary tick storage format |
| `nexus/` | `nexus` | 2.x | Core backtesting engine |
| `strategy/` | `nexus-strategy` | 3.x | Strategy trait and example strategies |

## For AI Agents

### Adding a New Crate
1. Add to workspace `Cargo.toml` members list
2. Create `libs/<name>/Cargo.toml` with proper workspace references
3. Add module stubs to `libs/<name>/src/lib.rs`
4. Create AGENTS.md in `libs/<name>/`

### Phase Order (Critical Path)
`tvc` (1.1) → `nexus` (2.x, depends on tvc) → `strategy` (3.x, depends on nexus)

## Dependencies

All crates depend on `tvc` once Phase 1.1 is complete.

<!-- MANUAL: -->
