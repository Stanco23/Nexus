<!-- Parent: ../AGENTS.md -->
<!-- Generated: 2026-04-14 | Updated: 2026-04-14 -->

# strategy/

Strategy trait and example strategies crate.

**Nautilus Reference:** `~/Nautilus/nautilus_trader/nautilus_trader/trading/strategy.pyx` (Strategy class, lines 109+)

## Purpose

The interface all strategies must implement, plus reference implementations.

## Module Structure

### `src/strategy_trait.rs` (Phase 3.1)
```rust
pub trait Strategy: Send + Sync + Clone {
    fn name(&self) -> &str;
    fn parameters(&self) -> Vec<ParameterSchema>;
    fn mode(&self) -> BacktestMode; // Tick, Bar, Hybrid
    fn subscribed_instruments(&self) -> Vec<InstrumentId>;
    fn on_trade(&mut self, instrument_id: InstrumentId, tick: &Tick,
                 ctx: &mut dyn StrategyCtx) -> Option<Signal>;
    fn on_bar(&mut self, instrument_id: InstrumentId, bar: &Bar,
              ctx: &mut dyn StrategyCtx) -> Option<Signal>;
    fn on_init(&mut self) {}
    fn on_finish(&mut self) {}
    fn clone_box(&self) -> Box<dyn Strategy>;
}

pub enum Signal {
    Buy { size: f64, stop_loss: Option<f64> },
    Sell { size: f64, stop_loss: Option<f64> },
    Close,
}
```

### `src/context.rs` (Phase 3.2)
```rust
pub trait StrategyCtx: Send + Sync {
    fn current_price(&self, instrument_id: InstrumentId) -> f64;
    fn position(&self, instrument_id: InstrumentId) -> Option<PositionSide>;
    fn account_equity(&self) -> f64;
    fn unrealized_pnl(&self, instrument_id: InstrumentId) -> f64;
    fn pending_orders(&self, instrument_id: InstrumentId) -> Vec<Order>;
    fn subscribe_instruments(&mut self, instruments: Vec<InstrumentId>);
}
```

### `src/strategies/` (Phase 3.4)
Example strategies:
- EMA Cross
- RSI Overbought/Oversold
- Breakout
- VWAP Mean Reversion

## Indicators (Phase 3.3)

**Nautilus Reference:** `~/Nautilus/nautilus_trader/nautilus_trader/indicators/`

| Indicator | Nautilus Source |
|-----------|----------------|
| SMA, EMA | `indicators/averages.pyx` |
| RSI, Stochastic | `indicators/momentum.pyx` |
| MACD, ADX | `indicators/trend.pyx` |
| Bollinger Bands, ATR | `indicators/volatility.pyx` |
| VWAP, OBV | `indicators/volume.pyx` |

## For AI Agents

### Strategy Must Be Send + Sync + Clone
`clone_box()` returns fresh instance per sweep combo. No shared mutable state.

### Dependencies
Depends on `nexus` crate (BacktestEngine must exist before Strategy can be written).

## Testing

Example strategies must compile and run a backtest producing non-trivial equity curves.

<!-- MANUAL: -->
