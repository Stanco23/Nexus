//! Portfolio — multi-instrument backtest orchestration.
//!
//! # Architecture
//! - `Portfolio`: manages `HashMap<InstrumentId, InstrumentState>`
//! - `InstrumentState`: position, pending_orders, equity, unrealized_pnl
//! - `BacktestEngine::run_portfolio()`: time-ordered tick delivery across instruments
//! - Strategy: `on_trade(instrument_id, tick, ctx)` called per instrument tick
//!
//! # Portfolio Equity
//! Portfolio equity = sum of all instrument equities. Per-instrument equity tracked separately.

use crate::book::{OrderBook, OrderEmulator, Side};
use crate::buffer::buffer_set::MergeCursor;
use crate::engine::orders::TrailingOffsetType;
use crate::engine::{CommissionConfig, Signal};
use crate::instrument::InstrumentId;
use crate::live::matching_core::{MatchingCore, MatchResult};
use crate::messages::OrderSide;
use crate::signals::SignalBus;
use std::collections::HashMap;
use std::sync::Arc;

/// Portfolio-level configuration.
#[derive(Debug, Clone)]
pub struct PortfolioConfig {
    pub initial_equity_per_instrument: f64,
    pub commission: CommissionConfig,
    /// SL threshold in percent (e.g. 2.0 = 2%)
    pub stop_loss_pct: f64,
    /// TP threshold in percent (e.g. 5.0 = 5%)
    pub take_profit_pct: f64,
    /// Trading days per year for Sharpe annualization (252 = standard, 365 = calendar)
    pub trading_days_per_year: f64,
    /// Use MatchingCore (price-time FIFO) instead of OrderEmulator (probabilistic).
    /// MatchingCore requires L2 order book data; use for live simulation or
    /// backtests with synthetic book data. Defaults to false (OrderEmulator).
    pub use_matching_core: bool,
    /// Whether a fill engine is active (MatchingCore or OrderEmulator).
    /// When true, fill engine handles position updates — market-order path is disabled.
    /// When false, market-order path fires on signal changes directly.
    /// Defaults to true.
    pub use_fill_engine: bool,
}

impl PortfolioConfig {
    pub fn new(initial_equity_per_instrument: f64, commission: CommissionConfig) -> Self {
        Self {
            initial_equity_per_instrument,
            commission,
            stop_loss_pct: 2.0,
            take_profit_pct: 5.0,
            trading_days_per_year: 252.0,
            use_matching_core: false,
            use_fill_engine: true,
        }
    }

    pub fn with_stop_loss(mut self, pct: f64) -> Self {
        self.stop_loss_pct = pct;
        self
    }

    pub fn with_take_profit(mut self, pct: f64) -> Self {
        self.take_profit_pct = pct;
        self
    }

    pub fn with_trading_days(mut self, days: f64) -> Self {
        self.trading_days_per_year = days;
        self
    }

    pub fn with_matching_core(mut self) -> Self {
        self.use_matching_core = true;
        self
    }

    /// Disable the fill engine (MatchingCore / OrderEmulator).
    /// The market-order path will fire on signal changes directly.
    /// Use this for strategies that execute purely on signals without
    /// limit-order fill modeling (e.g. ORB).
    pub fn with_fill_engine_disabled(mut self) -> Self {
        self.use_fill_engine = false;
        self
    }
}

impl Default for PortfolioConfig {
    fn default() -> Self {
        Self::new(10_000.0, CommissionConfig::new(0.001))
    }
}

/// Stores trailing stop state for an instrument's position.
#[derive(Debug, Clone)]
pub struct TrailingStopState {
    /// Current trigger price for the trailing stop.
    pub trigger_price: Option<f64>,
    /// Whether the trailing stop has been activated.
    pub is_activated: bool,
    /// The activation price for the trailing stop.
    pub activation_price: Option<f64>,
    /// The trailing offset value.
    pub trailing_offset: Option<f64>,
    /// The type of trailing offset (Absolute or Percentage).
    pub offset_type: crate::engine::orders::TrailingOffsetType,
    /// Last known market high (for SELL trailing stops).
    pub last_high: f64,
    /// Last known market low (for BUY trailing stops).
    pub last_low: f64,
}

impl Default for TrailingStopState {
    fn default() -> Self {
        Self {
            trigger_price: None,
            is_activated: false,
            activation_price: None,
            trailing_offset: None,
            offset_type: crate::engine::orders::TrailingOffsetType::Absolute,
            // last_high: used for SELL trailing stop (tracks highest price since entry)
            // Initialize to 0 so first update with market_high > 0 correctly sets it
            last_high: 0.0,
            // last_low: used for BUY trailing stop (tracks lowest price since entry)
            // Initialize to MAX so first update with market_low < MAX correctly sets it
            last_low: f64::MAX,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstrumentState {
    pub position: f64,
    pub entry_price: f64,
    pub equity: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub commissions: f64,
    pub num_trades: usize,
    /// Number of closed trades with positive PnL.
    pub num_wins: usize,
    /// Number of closed trades with negative PnL.
    pub num_losses: usize,
    pub peak_equity: f64,
    pub max_drawdown: f64,
    /// Stop-loss price for this position (from submit_with_sl_tp).
    pub sl_price: Option<f64>,
    /// Take-profit price for this position (from submit_with_sl_tp).
    pub tp_price: Option<f64>,
    /// Trailing stop state (None if no trailing stop).
    pub trailing_stop: Option<TrailingStopState>,
    /// Timestamp (ns) of last close — used for circuit breaker to prevent rapid re-entry.
    last_close_ns: u64,
    /// Price at last close — used for circuit breaker price-distance check.
    last_close_price: Option<f64>,
}

impl InstrumentState {
    pub fn new(initial_equity: f64) -> Self {
        Self {
            position: 0.0,
            entry_price: 0.0,
            equity: initial_equity,
            unrealized_pnl: 0.0,
            realized_pnl: 0.0,
            commissions: 0.0,
            num_trades: 0,
            num_wins: 0,
            num_losses: 0,
            peak_equity: initial_equity,
            max_drawdown: 0.0,
            sl_price: None,
            tp_price: None,
            trailing_stop: None,
            last_close_ns: 0,
            last_close_price: None,
        }
    }

    pub fn update_unrealized_pnl(&mut self, current_price: f64) {
        if self.position == 0.0 || self.entry_price == 0.0 {
            self.unrealized_pnl = 0.0;
            return;
        }
        if self.position > 0.0 {
            self.unrealized_pnl = (current_price - self.entry_price) * self.position.abs();
        } else {
            self.unrealized_pnl = (self.entry_price - current_price) * self.position.abs();
        }
    }

    pub fn total_equity(&self) -> f64 {
        self.equity + self.unrealized_pnl
    }

    /// Update peak equity and max drawdown after a price tick.
    pub fn update_peak(&mut self, current_price: f64) {
        self.update_unrealized_pnl(current_price);
        let total = self.total_equity();
        if total > self.peak_equity {
            self.peak_equity = total;
        }
        if self.peak_equity > 0.0 {
            let dd = (self.peak_equity - total) / self.peak_equity * self.peak_equity;
            if dd > self.max_drawdown {
                self.max_drawdown = dd;
            }
        }
    }
}

#[derive(Debug)]
pub struct Portfolio {
    initial_equity_per_instrument: f64,
    states: HashMap<InstrumentId, InstrumentState>,
    signal_bus: Option<Arc<SignalBus>>,
    /// Order emulator for limit order fill simulation (used when use_matching_core = false).
    order_emulator: OrderEmulator,
    /// Matching cores per instrument (used when use_matching_core = true).
    /// Provides price-time FIFO matching instead of probabilistic fill modeling.
    matching_cores: HashMap<InstrumentId, MatchingCore>,
    /// Per-instrument order books built from tick stream.
    order_books: HashMap<InstrumentId, OrderBook>,
    /// Equity curve time series for Sharpe / performance analysis.
    /// Recorded at each tick with non-zero position change or daily.
    equity_curve: Vec<f64>,
    /// Running equity at last record point (for equity_curve tracking).
    last_equity_record: f64,
}

impl Portfolio {
    pub fn new(initial_equity_per_instrument: f64) -> Self {
        Self {
            initial_equity_per_instrument,
            states: HashMap::new(),
            signal_bus: None,
            order_emulator: OrderEmulator::new(),
            matching_cores: HashMap::new(),
            order_books: HashMap::new(),
            equity_curve: Vec::new(),
            last_equity_record: initial_equity_per_instrument,
        }
    }

    pub fn with_signal_bus(mut self, signal_bus: Arc<SignalBus>) -> Self {
        self.signal_bus = Some(signal_bus);
        self
    }

    /// Returns a clone of the SignalBus if one is configured.
    /// Strategies can use this to subscribe to signals from other instruments.
    pub fn signal_bus(&self) -> Option<Arc<SignalBus>> {
        self.signal_bus.clone()
    }

    /// Add a MatchingCore for an instrument. Called automatically when
    /// `use_matching_core = true` in config and instrument is registered.
    pub fn add_matching_core(&mut self, instrument_id: InstrumentId, maker_fee: f64, taker_fee: f64) {
        self.matching_cores
            .entry(instrument_id.clone())
            .or_insert_with(|| MatchingCore::new(instrument_id, maker_fee, taker_fee));
    }

    pub fn register_instrument(&mut self, instrument_id: InstrumentId) {
        self.states
            .entry(instrument_id)
            .or_insert_with(|| InstrumentState::new(self.initial_equity_per_instrument));
    }

    pub fn register_instruments(&mut self, instrument_ids: &[InstrumentId]) {
        for id in instrument_ids {
            self.register_instrument(id.clone());
        }
    }

    pub fn state(&self, instrument_id: &InstrumentId) -> Option<&InstrumentState> {
        self.states.get(instrument_id)
    }

    pub fn state_mut(&mut self, instrument_id: &InstrumentId) -> Option<&mut InstrumentState> {
        self.states.get_mut(instrument_id)
    }

    pub fn portfolio_equity(&self) -> f64 {
        self.states.values().map(|s| s.total_equity()).sum()
    }

    pub fn total_unrealized_pnl(&self) -> f64 {
        self.states.values().map(|s| s.unrealized_pnl).sum()
    }

    pub fn num_instruments(&self) -> usize {
        self.states.len()
    }

    pub fn open_positions(&self) -> Vec<(InstrumentId, f64, f64)> {
        self.states
            .iter()
            .filter(|(_, s)| s.position != 0.0)
            .map(|(id, s)| (id.clone(), s.position, s.entry_price))
            .collect()
    }

    /// Open or add to a position for the given instrument.
    ///
    /// # Arguments
    /// * `sl_price` - Stop-loss price (None to disable SL)
    /// * `tp_price` - Take-profit price (None to disable TP)
    /// * `trailing_stop` - Trailing stop state (None to disable trailing stop)
    #[allow(clippy::too_many_arguments)]
    pub fn open_position(
        &mut self,
        instrument_id: &InstrumentId,
        price: f64,
        size: f64,
        side: Signal,
        commission_config: &CommissionConfig,
        sl_price: Option<f64>,
        tp_price: Option<f64>,
        trailing_stop: Option<TrailingStopState>,
    ) {
        let state = self.states.entry(instrument_id.clone()).or_insert_with(|| {
            InstrumentState::new(self.initial_equity_per_instrument)
        });

        let comm = commission_config.compute(price, size.abs());
        state.commissions += comm;
        state.equity -= comm;

        if state.position == 0.0 {
            // Open new position
            state.position = if matches!(side, Signal::Buy) { size } else { -size };
            state.entry_price = price;
            // Store SL/TP and trailing stop for the new position
            state.sl_price = sl_price;
            state.tp_price = tp_price;
            state.trailing_stop = trailing_stop;
        } else {
            // Add to existing position (average in)
            let is_same_side = (state.position > 0.0 && matches!(side, Signal::Buy))
                || (state.position < 0.0 && matches!(side, Signal::Sell));
            if is_same_side {
                let old_pos = state.position.abs();
                let new_fill_pos = size.abs();
                state.entry_price =
                    (state.entry_price * old_pos + price * new_fill_pos) / (old_pos + new_fill_pos);
                state.position = if state.position > 0.0 {
                    old_pos + new_fill_pos
                } else {
                    -(old_pos + new_fill_pos)
                };
            } else {
                // Reversing — close then open
                let pnl = if state.position > 0.0 {
                    (price - state.entry_price) * state.position.abs()
                } else {
                    (state.entry_price - price) * state.position.abs()
                };
                state.realized_pnl += pnl;
                state.equity += pnl;
                state.position = if matches!(side, Signal::Buy) { size } else { -size };
                state.entry_price = price;
            }
        }
        state.num_trades += 1;
    }

    /// Close a position for the given instrument and return the realized PnL.
    pub fn close_position(
        &mut self,
        instrument_id: &InstrumentId,
        price: f64,
        commission_config: &CommissionConfig,
        ts_init: u64,
    ) -> f64 {
        let state = match self.states.get_mut(instrument_id) {
            Some(s) => s,
            None => return 0.0,
        };
        if state.position == 0.0 {
            return 0.0;
        }

        let comm = commission_config.compute(price, state.position.abs());
        state.commissions += comm;

        let pnl = if state.position > 0.0 {
            (price - state.entry_price) * state.position.abs()
        } else {
            (state.entry_price - price) * state.position.abs()
        };

        state.realized_pnl += pnl;
        state.equity += pnl;
        state.equity -= comm;
        state.position = 0.0;
        state.entry_price = 0.0;
        state.sl_price = None;
        state.tp_price = None;
        state.trailing_stop = None;
        state.last_close_ns = ts_init;
        state.last_close_price = Some(price);
        if pnl > 0.0 {
            state.num_wins += 1;
        } else if pnl < 0.0 {
            state.num_losses += 1;
        }
        state.num_trades += 1;

        pnl
    }

    /// Close a position with VPIN-aware slippage adjustment.
    ///
    /// `base_price` — current market mid price.
    /// `vpin` — current Volume-synchronized Probability of Informed Trading (0..1).
    /// `order_size_ticks` — order size in tick units (1.0 for our default).
    /// `avg_tick_duration_ns` — average inter-tick gap in nanoseconds.
    ///
    /// Returns the realized PnL **after slippage adjustment**, the delay applied,
    /// and the impact in basis points. The base `close_position()` is called with
    /// the slippage-adjusted price, so internal state is consistent.
    pub fn close_position_with_slippage(
        &mut self,
        instrument_id: &InstrumentId,
        base_price: f64,
        commission_config: &CommissionConfig,
        ts_init: u64,
        vpin: f64,
        order_size_ticks: f64,
        avg_tick_duration_ns: u64,
    ) -> (f64, u64, f64) {
        // Compute slippage from the current state of the world.
        let (fill_price, delay_ns, impact_bps) = crate::slippage::compute_fill_price(
            order_size_ticks,
            vpin,
            avg_tick_duration_ns,
            base_price,
        );
        let pnl = self.close_position(instrument_id, fill_price, commission_config, ts_init);
        (pnl, delay_ns, impact_bps)
    }

    /// Total realized PnL across all instruments.
    pub fn total_realized_pnl(&self) -> f64 {
        self.states.values().map(|s| s.realized_pnl).sum()
    }

    /// Returns true if the circuit breaker is still active (preventing re-entry after SL/TP close).
    /// Circuit breaker blocks re-entry for `cooldown_ticks` ticks OR until price moves `min_distance_pct`% away.
    pub fn is_circuit_broken(
        &self,
        instrument_id: &InstrumentId,
        current_price: f64,
        cooldown_ticks: u64,
        min_distance_pct: f64,
    ) -> bool {
        let state = match self.states.get(instrument_id) {
            Some(s) => s,
            None => return false,
        };

        // No recent close → no circuit breaker
        let last_close_price = match state.last_close_price {
            Some(p) => p,
            None => return false,
        };

        // Check time-based circuit breaker
        let ticks_since_close = state.last_close_ns;
        if ticks_since_close > 0 && ticks_since_close < cooldown_ticks {
            // Within cooldown window — check if price has moved enough to cancel the breaker
            let distance_pct = ((current_price - last_close_price) / last_close_price).abs();
            if distance_pct < min_distance_pct {
                return true; // Circuit breaker active
            }
        }

        false
    }

    /// Arm the circuit breaker after an SL/TP-triggered close.
    /// Sets last_close_ns and last_close_price from the close event.
    pub fn arm_circuit_breaker(&mut self, instrument_id: &InstrumentId, price: f64, ts_init: u64) {
        if let Some(state) = self.states.get_mut(instrument_id) {
            state.last_close_ns = ts_init;
            state.last_close_price = Some(price);
        }
    }

    /// Total commissions paid across all instruments.
    pub fn total_commissions(&self) -> f64 {
        self.states.values().map(|s| s.commissions).sum()
    }

    /// Portfolio-level peak equity and max drawdown.
    pub fn portfolio_peak_equity(&self) -> f64 {
        self.states.values().map(|s| s.peak_equity).sum()
    }

    /// Portfolio-level max drawdown (maximum of all instruments' max drawdowns).
    pub fn portfolio_max_drawdown(&self) -> f64 {
        self.states
            .values()
            .map(|s| s.max_drawdown)
            .fold(0.0, f64::max)
    }

    /// Total number of trades across all instruments.
    pub fn total_trades(&self) -> usize {
        self.states.values().map(|s| s.num_trades).sum()
    }

    /// Win rate across all instruments: wins / (wins + losses).
    /// Returns 0.0 if no closed trades yet.
    pub fn win_rate(&self) -> f64 {
        let total_wins: usize = self.states.values().map(|s| s.num_wins).sum();
        let total_losses: usize = self.states.values().map(|s| s.num_losses).sum();
        let total_closed = total_wins + total_losses;
        if total_closed == 0 {
            return 0.0;
        }
        total_wins as f64 / total_closed as f64
    }

    /// Total number of winning trades across all instruments.
    pub fn total_wins(&self) -> usize {
        self.states.values().map(|s| s.num_wins).sum()
    }

    /// Total number of losing trades across all instruments.
    pub fn total_losses(&self) -> usize {
        self.states.values().map(|s| s.num_losses).sum()
    }

    /// Daily returns derived from equity_curve.
    /// Each return is (equity[i] - equity[i-1]) / equity[i-1].
    /// Returns empty if < 2 points recorded.
    pub fn returns(&self) -> Vec<f64> {
        if self.equity_curve.len() < 2 {
            return Vec::new();
        }
        let mut rets = Vec::with_capacity(self.equity_curve.len() - 1);
        for i in 1..self.equity_curve.len() {
            let prev = self.equity_curve[i - 1];
            let curr = self.equity_curve[i];
            if prev > 0.0 {
                rets.push((curr - prev) / prev);
            }
        }
        rets
    }

    /// Record current portfolio equity into the equity curve.
    /// Call this at each tick or bar boundary.
    pub fn record_equity(&mut self) {
        let total = self.portfolio_equity();
        self.last_equity_record = total;
        self.equity_curve.push(total);
    }

    /// Returns a view into the equity curve for iteration.
    pub fn equity_curve(&self) -> &[f64] {
        &self.equity_curve
    }

    /// Set stop-loss and take-profit prices for an existing position.
    /// Also sets trailing stop if provided.
    pub fn set_sl_tp(
        &mut self,
        instrument_id: &InstrumentId,
        sl_price: Option<f64>,
        tp_price: Option<f64>,
        trailing_stop: Option<TrailingStopState>,
    ) {
        if let Some(state) = self.states.get_mut(instrument_id) {
            state.sl_price = sl_price;
            state.tp_price = tp_price;
            state.trailing_stop = trailing_stop;
        }
    }

    /// Get current running high for an instrument (for trailing stop updates).
    pub fn market_high(&self, instrument_id: &InstrumentId) -> f64 {
        self.states
            .get(instrument_id)
            .map(|s| s.trailing_stop.as_ref().map(|ts| ts.last_high).unwrap_or(0.0))
            .unwrap_or(0.0)
    }

    /// Get current running low for an instrument (for trailing stop updates).
    pub fn market_low(&self, instrument_id: &InstrumentId) -> f64 {
        self.states
            .get(instrument_id)
            .map(|s| s.trailing_stop.as_ref().map(|ts| ts.last_low).unwrap_or(f64::MAX))
            .unwrap_or(f64::MAX)
    }

    /// Update peak tracking for all instruments after a tick.
    pub fn update_peaks(&mut self, prices: &HashMap<InstrumentId, f64>) {
        for (id, state) in self.states.iter_mut() {
            if let Some(&price) = prices.get(id) {
                state.update_peak(price);
            }
        }
    }

    /// Run a portfolio backtest over a merged cursor.
    ///
    /// Calls `strategy.on_trade()` per tick. Strategy returns `Signal::Buy/Sell/Close`.
    /// Position opens/closes are managed via `open_position`/`close_position`.
    /// Commission is charged on each fill. Peak equity and max drawdown are tracked.
    pub fn run_portfolio<S: PortfolioStrategy + Clone>(
        &mut self,
        cursor: &mut MergeCursor<'_>,
        config: &PortfolioConfig,
        strategy_factory: impl Fn() -> S,
    ) {
        let mut strategy = strategy_factory();

        // Allow strategy to subscribe to signals before the backtest loop.
        // Strategies that want signal notifications override `subscribe_signal`.
        if let Some(ref sb) = self.signal_bus {
            strategy.subscribe_signal(Arc::clone(sb));
        }

        let mut prices: HashMap<InstrumentId, f64> = HashMap::new();
        let mut last_signal: HashMap<InstrumentId, Signal> = HashMap::new();
        // Track running high/low per instrument for trailing stops
        let mut market_highs: HashMap<InstrumentId, f64> = HashMap::new();
        let mut market_lows: HashMap<InstrumentId, f64> = HashMap::new();

        // Initialize last_signal for all registered instruments
        for id in self.states.keys() {
            last_signal.insert(id.clone(), Signal::Close);
        }

        // Initialize order books for all registered instruments
        for id in self.states.keys() {
            self.order_books.entry(id.clone()).or_insert_with(OrderBook::new);
        }

        while let Some(event) = cursor.advance() {
            let instrument_id = event.instrument_id.clone();
            let price = event.tick.price_int as f64 / 1_000_000_000.0;
            let size = event.tick.size_int as f64 / 1_000_000_000.0;

            // Ensure instrument is registered
            if !self.states.contains_key(&instrument_id) {
                self.register_instrument(instrument_id.clone());
                last_signal.insert(instrument_id.clone(), Signal::Close);
            }

            // Ensure order book exists for this instrument
            let book = self.order_books.entry(instrument_id.clone()).or_insert_with(OrderBook::new);
            // Update order book from trade (builds synthetic L2 book from tick flow)
            book.update_from_trade(event.tick);

            // ── Conditional fill engine: MatchingCore (FIFO) vs OrderEmulator (probabilistic) ──
            // MatchingCore requires L2 book data — use when running live or with synthetic book.
            // OrderEmulator uses VPIN-based probabilistic fill modeling — use for tick-based backtest.
            let fills = if config.use_matching_core {
                // Get or create MatchingCore for this instrument
                let core = self.matching_cores
                    .entry(instrument_id.clone())
                    .or_insert_with(|| {
                        MatchingCore::new(
                            instrument_id.clone(),
                            config.commission.maker_rate,
                            config.commission.rate,
                        )
                    });
                // Update market state (price, VPIN, spread estimate)
                // Default 1.0 bps spread when no quote data available in tick
                core.update_market(price, event.tick.vpin, 1.0);
                Vec::new() // MatchingCore fills are returned via submit_limit, not here
            } else {
                // Update market volume estimate for emulator's fill modeling
                self.order_emulator.update_market_volume(size);
                // Process pending limit orders — get fills at current price
                // This must happen before the signal-based position logic so fills
                // update positions before the strategy sees the new state.
                self.order_emulator.process_fills(
                    price,
                    event.tick.vpin,
                    event.tick.timestamp_ns,
                    config.commission.maker_rate,
                    book,
                )
            };

            for fill in fills {
                // Handle fill: open or close position based on fill side
                if fill.side == Side::Buy {
                    // Buy fill — if flat open long, if short close first then open long
                    let cur_pos = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);
                    if cur_pos < 0.0 {
                        self.close_position(&instrument_id, fill.fill_price, &config.commission, fill.timestamp_ns);
                    }
                    let pos_after = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);
                    if pos_after <= 0.0 {
                        self.open_position(&instrument_id, fill.fill_price, fill.fill_size, Signal::Buy, &config.commission, None, None, None);
                    }
                } else {
                    // Sell fill — if long close first, if flat open short
                    let cur_pos = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);
                    if cur_pos > 0.0 {
                        self.close_position(&instrument_id, fill.fill_price, &config.commission, fill.timestamp_ns);
                    }
                    let pos_after = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);
                    if pos_after >= 0.0 {
                        self.open_position(&instrument_id, fill.fill_price, fill.fill_size, Signal::Sell, &config.commission, None, None, None);
                    }
                }
            }

            // Update running high/low for this instrument
            let high = market_highs.entry(instrument_id.clone()).or_insert(price);
            *high = high.max(price);
            let low = market_lows.entry(instrument_id.clone()).or_insert(price);
            *low = low.min(price);

            prices.insert(instrument_id.clone(), price);

            // Get current position for this instrument
            let current_position = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);

            // Strategy signal
            let signal = strategy.on_trade(
                instrument_id.clone(),
                event.tick.timestamp_ns,
                price,
                size,
                self,
            );

            // Publish market signal to SignalBus so strategies can subscribe to them
            if let Some(ref signal_bus) = self.signal_bus {
                let signal_value = match signal {
                    Signal::Buy => 1.0,
                    Signal::Sell => -1.0,
                    Signal::Close => 0.0,
                };
                // Publish with instrument-specific name for targeted subscriptions
                let signal_name = format!("{}.{}", instrument_id.symbol, instrument_id.exchange);
                signal_bus.publish(&signal_name, signal_value, event.tick.timestamp_ns);
                // Also publish a generic "market_signal" for any subscribers interested in all market signals
                signal_bus.publish("market_signal", signal_value, event.tick.timestamp_ns);
            }

            // SL/TP circuit breaker — close on significant adverse move
            // Also update trailing stop trigger prices
            let market_high = *market_highs.get(&instrument_id).unwrap_or(&price);
            let market_low = *market_lows.get(&instrument_id).unwrap_or(&price);
            let sl_tp_signal = self.check_sl_tp(&instrument_id, price, market_high, market_low, config);
            let final_signal = if sl_tp_signal == Some(Signal::Close) {
                Signal::Close
            } else {
                signal
            };

            let last_sig = last_signal.get(&instrument_id).cloned().unwrap_or(Signal::Close);

            // ── Conditional fill engine: MatchingCore (FIFO) vs OrderEmulator (probabilistic) ──
            // When MatchingCore is enabled, submit to matching core (returns immediate fills).
            // When using OrderEmulator, queue in emulator for batch fill processing.
            if final_signal != last_sig && config.use_matching_core {
                // MatchingCore: submit_limit returns fills immediately if price crosses
                let side = match final_signal {
                    Signal::Buy if current_position <= 0.0 => Some(OrderSide::Buy),
                    Signal::Sell if current_position >= 0.0 => Some(OrderSide::Sell),
                    _ => None,
                };
                if let Some(side) = side {
                    let core = self.matching_cores.get_mut(&instrument_id).unwrap();
                    let client_order_id = format!("o_{}", event.tick.timestamp_ns);
                    let fills = core.submit_limit(
                        client_order_id,
                        side,
                        price,
                        size,
                        event.tick.timestamp_ns,
                        false, // post_only — not set for signal-based orders
                    );
                    // Process fills returned by MatchingCore
                    for fill in fills {
                        let fill_side = if fill.side == OrderSide::Buy { Side::Buy } else { Side::Sell };
                        if fill_side == Side::Buy {
                            let cur_pos = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);
                            if cur_pos < 0.0 {
                                self.close_position(&instrument_id, fill.fill_price, &config.commission, fill.ts_event);
                            }
                            let pos_after = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);
                            if pos_after <= 0.0 {
                                self.open_position(&instrument_id, fill.fill_price, fill.fill_size, Signal::Buy, &config.commission, None, None, None);
                            }
                        } else {
                            let cur_pos = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);
                            if cur_pos > 0.0 {
                                self.close_position(&instrument_id, fill.fill_price, &config.commission, fill.ts_event);
                            }
                            let pos_after = self.state(&instrument_id).map(|s| s.position).unwrap_or(0.0);
                            if pos_after >= 0.0 {
                                self.open_position(&instrument_id, fill.fill_price, fill.fill_size, Signal::Sell, &config.commission, None, None, None);
                            }
                        }
                    }
                }
            } else if final_signal != last_sig {
                // OrderEmulator: queue limit order for batch fill processing
                match final_signal {
                    Signal::Buy => {
                        if current_position <= 0.0 {
                            self.order_emulator.submit_limit(
                                price,
                                size,
                                Side::Buy,
                                event.tick.timestamp_ns,
                            );
                        }
                    }
                    Signal::Sell => {
                        if current_position >= 0.0 {
                            self.order_emulator.submit_limit(
                                price,
                                size,
                                Side::Sell,
                                event.tick.timestamp_ns,
                            );
                        }
                    }
                    Signal::Close => {
                        // Cancel any pending limit orders when closing
                        // (cancel_order removes from pending; no-op if already empty)
                    }
                }
            }

            // Market-order position execution (gated on signal change to avoid over-trading)
            // ONLY fires when use_fill_engine = false — i.e. neither MatchingCore nor
            // OrderEmulator is the intended execution path. When a fill engine is active,
            // position updates are handled exclusively by the fill engine path above
            // (MatchingCore or OrderEmulator), so this path must be suppressed to avoid
            // double position updates on the same signal change.
            let position_size = strategy.position_size();
            if final_signal != last_sig && !config.use_fill_engine {
                match final_signal {
                    Signal::Buy => {
                        if current_position <= 0.0 {
                            if current_position < 0.0 {
                                self.close_position(&instrument_id, price, &config.commission, event.tick.timestamp_ns);
                            }
                            self.open_position(&instrument_id, price, position_size, Signal::Buy, &config.commission, None, None, None);
                        }
                    }
                    Signal::Sell => {
                        if current_position >= 0.0 {
                            if current_position > 0.0 {
                                self.close_position(&instrument_id, price, &config.commission, event.tick.timestamp_ns);
                            }
                            self.open_position(&instrument_id, price, position_size, Signal::Sell, &config.commission, None, None, None);
                        }
                    }
                    Signal::Close => {
                        if current_position != 0.0 {
                            self.close_position(&instrument_id, price, &config.commission, event.tick.timestamp_ns);
                        }
                    }
                }
            }
            last_signal.insert(instrument_id, final_signal);

            // Update peaks after each tick
            self.update_peaks(&prices);
        }
    }

    /// Check SL/TP and trailing stop for a given instrument.
    /// Returns `Some(Signal::Close)` if stop-loss, take-profit, or trailing stop is triggered.
    /// Also updates trailing stop trigger price based on current market movement.
    fn check_sl_tp(
        &mut self,
        instrument_id: &InstrumentId,
        current_price: f64,
        market_high: f64,
        market_low: f64,
        config: &PortfolioConfig,
    ) -> Option<Signal> {
        let state = self.state_mut(instrument_id)?;
        if state.position == 0.0 || state.entry_price == 0.0 {
            return None;
        }

        let position_is_long = state.position > 0.0;

        // Check fixed SL price
        if let Some(sl_price) = state.sl_price {
            let sl_triggered = if position_is_long {
                current_price <= sl_price
            } else {
                current_price >= sl_price
            };
            if sl_triggered {
                return Some(Signal::Close);
            }
        }

        // Check fixed TP price
        if let Some(tp_price) = state.tp_price {
            let tp_triggered = if position_is_long {
                current_price >= tp_price
            } else {
                current_price <= tp_price
            };
            if tp_triggered {
                return Some(Signal::Close);
            }
        }

        // Check trailing stop and update state
        if let Some(ref mut ts) = state.trailing_stop {
            // Check activation price
            if let Some(activation) = ts.activation_price {
                if !ts.is_activated {
                    if position_is_long {
                        if current_price >= activation {
                            ts.is_activated = true;
                        }
                    } else {
                        if current_price <= activation {
                            ts.is_activated = true;
                        }
                    }
                }
            } else {
                ts.is_activated = true;
            }

            // Update last high/low for trailing stop calculations
            if position_is_long {
                if market_low < ts.last_low {
                    ts.last_low = market_low;
                }
            } else {
                if market_high > ts.last_high {
                    ts.last_high = market_high;
                }
            }

            // Only process trailing if activated
            if ts.is_activated {
                let offset_value = match ts.trailing_offset {
                    Some(offset) => match ts.offset_type {
                        TrailingOffsetType::Percentage => current_price * offset,
                        TrailingOffsetType::Absolute => offset,
                    },
                    None => return None,
                };

                // Calculate new trigger price
                let new_trigger = if position_is_long {
                    // BUY trailing stop: trigger moves UP as price rises
                    // trigger = last_low - offset
                    let calculated = ts.last_low - offset_value;
                    match ts.trigger_price {
                        Some(current) if calculated > current => calculated,
                        Some(current) => current,
                        None => calculated,
                    }
                } else {
                    // SELL trailing stop: trigger moves DOWN as price falls
                    // trigger = last_high + offset
                    let calculated = ts.last_high + offset_value;
                    match ts.trigger_price {
                        Some(current) if calculated < current => calculated,
                        Some(current) => current,
                        None => calculated,
                    }
                };

                ts.trigger_price = Some(new_trigger);

                // Check if trailing stop triggers
                let ts_triggered = if position_is_long {
                    current_price <= new_trigger
                } else {
                    current_price >= new_trigger
                };

                if ts_triggered {
                    return Some(Signal::Close);
                }
            }
        }

        // Fallback to percentage-based SL/TP if no fixed prices set
        let pnl_pct = if position_is_long {
            (current_price - state.entry_price) / state.entry_price * 100.0
        } else {
            (state.entry_price - current_price) / state.entry_price * 100.0
        };

        // Stop-loss percentage fallback
        if state.sl_price.is_none() && pnl_pct <= -config.stop_loss_pct {
            return Some(Signal::Close);
        }
        // Take-profit percentage fallback
        if state.tp_price.is_none() && pnl_pct >= config.take_profit_pct {
            return Some(Signal::Close);
        }

        None
    }

    /// Final equity after unrealized PnL is settled at the given prices.
    pub fn final_equity(&self, prices: &HashMap<InstrumentId, f64>) -> f64 {
        let mut total = 0.0;
        for (id, state) in &self.states {
            let price = prices.get(id).copied().unwrap_or(state.entry_price);
            let unrealized = if state.position > 0.0 {
                (price - state.entry_price) * state.position.abs()
            } else if state.position < 0.0 {
                (state.entry_price - price) * state.position.abs()
            } else {
                0.0
            };
            total += state.equity + unrealized - state.commissions;
        }
        total
    }
}

// =============================================================================
// `PortfolioStrategy` trait — LEGACY INTERFACE for direct-portfolio strategies.
//
// This trait is kept for backward compatibility with the 3 existing test
// strategies (MeanRevStrategy, MomentumStrategy, NoOpStrategy). New strategies
// should implement `nexus_strategy::Strategy` instead.
//
// The duplication between `nexus_strategy::Strategy` and `PortfolioStrategy` is
// documented as a known design choice. Consolidation is a multi-layer refactor
// (requires StrategyCtx impls in upper layers — out of scope for current work).
// =============================================================================

pub trait PortfolioStrategy {
    fn on_trade(
        &mut self,
        instrument_id: InstrumentId,
        timestamp_ns: u64,
        price: f64,
        size: f64,
        portfolio: &mut Portfolio,
    ) -> Signal;

    /// Subscribe to a named signal on the SignalBus.
    /// Called once by `run_portfolio` before the backtest loop starts,
    /// allowing strategies to set up signal subscriptions.
    /// The default implementation is a no-op.
    fn subscribe_signal(&mut self, _signal_bus: Arc<SignalBus>) {}

    /// Returns the position size for this strategy.
    /// Subclasses override to specify their configured size.
    fn position_size(&self) -> f64 {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::CommissionConfig;

    #[test]
    fn test_portfolio_new() {
        let portfolio = Portfolio::new(10000.0);
        assert_eq!(portfolio.num_instruments(), 0);
        assert_eq!(portfolio.portfolio_equity(), 0.0);
    }

    #[test]
    fn test_portfolio_register_instrument() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        portfolio.register_instrument(btc_id.clone());
        assert_eq!(portfolio.num_instruments(), 1);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.equity, 10000.0);
        assert_eq!(state.position, 0.0);
    }

    #[test]
    fn test_portfolio_multiple_instruments() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let eth_id = InstrumentId::new("ETHUSDT", "BINANCE");
        portfolio.register_instruments(&[btc_id, eth_id]);
        assert_eq!(portfolio.num_instruments(), 2);
        assert_eq!(portfolio.portfolio_equity(), 20000.0);
    }

    #[test]
    fn test_instrument_state_unrealized_pnl_long() {
        let mut state = InstrumentState::new(10000.0);
        state.position = 1.0;
        state.entry_price = 100.0;
        state.update_unrealized_pnl(110.0);
        assert!((state.unrealized_pnl - 10.0).abs() < 0.001);
        assert!((state.total_equity() - 10010.0).abs() < 0.001);
    }

    #[test]
    fn test_instrument_state_unrealized_pnl_short() {
        let mut state = InstrumentState::new(10000.0);
        state.position = -1.0;
        state.entry_price = 100.0;
        state.update_unrealized_pnl(90.0);
        assert!((state.unrealized_pnl - 10.0).abs() < 0.001);
        assert!((state.total_equity() - 10010.0).abs() < 0.001);
    }

    #[test]
    fn test_portfolio_total_equity() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let eth_id = InstrumentId::new("ETHUSDT", "BINANCE");
        portfolio.register_instruments(&[btc_id.clone(), eth_id.clone()]);

        {
            let btc = portfolio.state_mut(&btc_id).unwrap();
            btc.position = 1.0;
            btc.entry_price = 50000.0;
            btc.update_unrealized_pnl(51000.0);
        }

        {
            let eth = portfolio.state_mut(&eth_id).unwrap();
            eth.position = -1.0;
            eth.entry_price = 3000.0;
            eth.update_unrealized_pnl(2900.0);
        }

        let btc_total = 10000.0 + 1000.0;
        let eth_total = 10000.0 + 100.0;
        assert!((portfolio.portfolio_equity() - (btc_total + eth_total)).abs() < 0.001);
    }

    #[test]
    fn test_open_position_long() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Buy, &comm, None, None, None);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 1.0);
        assert_eq!(state.entry_price, 100.0);
        // commission = 100 * 1 * 0.001 = 0.1
        assert_eq!(state.equity, 10000.0 - 0.1); // commission charged
        assert_eq!(state.commissions, 0.1);
    }

    #[test]
    fn test_open_position_short() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Sell, &comm, None, None, None);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, -1.0);
        assert_eq!(state.entry_price, 100.0);
    }

    #[test]
    fn test_open_position_add_to_long() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        // Open long 1 @ 100
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Buy, &comm, None, None, None);
        // Add 1 more @ 110 (average entry = (100*1 + 110*1) / 2 = 105)
        portfolio.open_position(&btc_id, 110.0, 1.0, Signal::Buy, &comm, None, None, None);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 2.0);
        assert!((state.entry_price - 105.0).abs() < 0.001);
        // commission 1: 100 * 1 * 0.001 = 0.1, commission 2: 110 * 1 * 0.001 = 0.11
        assert!((state.commissions - 0.21).abs() < 0.0001);
    }

    #[test]
    fn test_open_position_reverse_short_to_long() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        // Open short 1 @ 100
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Sell, &comm, None, None, None);
        // Buy to close short @ 95 → pnl = 100 - 95 = 5
        let pnl = portfolio.close_position(&btc_id, 95.0, &comm, 0);
        assert!((pnl - 5.0).abs() < 0.001);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 0.0);
        // Now open long 1 @ 95
        portfolio.open_position(&btc_id, 95.0, 1.0, Signal::Buy, &comm, None, None, None);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 1.0);
        assert_eq!(state.entry_price, 95.0);
    }

    #[test]
    fn test_close_position_long() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        // Open long 1 @ 100
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Buy, &comm, None, None, None);
        // Close @ 110 → pnl = (110 - 100) * 1 = 10
        let pnl = portfolio.close_position(&btc_id, 110.0, &comm, 0);
        assert!((pnl - 10.0).abs() < 0.001);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 0.0);
        // commission = 100 * 1 * 0.001 = 0.1, exit commission = 110 * 1 * 0.001 = 0.11
        // equity = 10000 - 0.1(entry) - 0.11(exit) + 10
        assert!((state.equity - (10000.0 - 0.21 + 10.0)).abs() < 0.001);
        assert_eq!(state.realized_pnl, 10.0);
    }

    #[test]
    fn test_close_position_short() {
        let mut portfolio = Portfolio::new(10000.0);
        let btc_id = InstrumentId::new("BTCUSDT", "BINANCE");
        let comm = CommissionConfig::new(0.001);
        portfolio.register_instrument(btc_id.clone());
        // Open short 1 @ 100
        portfolio.open_position(&btc_id, 100.0, 1.0, Signal::Sell, &comm, None, None, None);
        // Close @ 90 → pnl = (100 - 90) * 1 = 10
        let pnl = portfolio.close_position(&btc_id, 90.0, &comm, 0);
        assert!((pnl - 10.0).abs() < 0.001);
        let state = portfolio.state(&btc_id).unwrap();
        assert_eq!(state.position, 0.0);
        assert_eq!(state.realized_pnl, 10.0);
    }

    #[test]
    fn test_update_peak_tracking() {
        let mut state = InstrumentState::new(10000.0);
        state.position = 1.0;
        state.entry_price = 100.0;
        // equity = 10000, unrealized = 0
        state.update_peak(100.0);
        assert_eq!(state.peak_equity, 10000.0);
        assert_eq!(state.max_drawdown, 0.0);
        // price rises to 110 → unrealized = 10, equity = 10010
        state.update_peak(110.0);
        assert_eq!(state.peak_equity, 10010.0);
        assert_eq!(state.max_drawdown, 0.0);
        // price drops to 95 → unrealized = -5, equity = 9995
        state.update_peak(95.0);
        assert_eq!(state.peak_equity, 10010.0);
        // drawdown = (10010 - 9995) / 10010 ≈ 0.15%
        assert!(state.max_drawdown > 0.0);
        assert!(state.max_drawdown < 0.5);
    }

    #[test]
    fn test_instrument_state_new_has_all_fields() {
        let state = InstrumentState::new(10000.0);
        assert_eq!(state.position, 0.0);
        assert_eq!(state.entry_price, 0.0);
        assert_eq!(state.equity, 10000.0);
        assert_eq!(state.realized_pnl, 0.0);
        assert_eq!(state.commissions, 0.0);
        assert_eq!(state.num_trades, 0);
        assert_eq!(state.peak_equity, 10000.0);
        assert_eq!(state.max_drawdown, 0.0);
    }

    #[test]
    fn test_portfolio_signal_bus_getter() {
        // Without signal bus
        let portfolio = Portfolio::new(10000.0);
        assert!(portfolio.signal_bus().is_none());

        // With signal bus
        let sb = Arc::new(SignalBus::new());
        let portfolio = Portfolio::new(10000.0).with_signal_bus(sb.clone());
        let retrieved = portfolio.signal_bus();
        assert!(retrieved.is_some());
        // Should be the same bus (same Arc pointer)
        assert!(Arc::ptr_eq(retrieved.as_ref().unwrap(), &sb));
    }

    #[test]
    fn test_portfolio_strategy_subscribe_signal_default() {
        // Test that the default subscribe_signal is a no-op and doesn't panic.
        struct NoOpStrategy;
        impl PortfolioStrategy for NoOpStrategy {
            fn on_trade(&mut self, _: InstrumentId, _: u64, _: f64, _: f64, _: &mut Portfolio) -> Signal {
                Signal::Close
            }
            // Default subscribe_signal should be inherited (no override)
        }

        let sb = Arc::new(SignalBus::new());
        let mut strategy = NoOpStrategy;
        // Should not panic — default is no-op
        strategy.subscribe_signal(sb);
    }
}
