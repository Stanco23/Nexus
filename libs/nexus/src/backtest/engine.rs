//! Clean backtest engine — builder pattern API.
//!
//! # Usage
//! ```ignore
//! let result = BacktestEngine::new()
//!     .with_instrument("BTCUSDT", "BINANCE")?
//!     .with_date_range(start_date, end_date)?
//!     .with_data_dir("./data")?
//!     .with_initial_equity(100_000.0)?
//!     .with_commission_bps(0.5)?
//!     .run(|| MyStrategy::new())?;
//! ```
//!
//! # Architecture
//! - `BacktestEngine`: builder → loads data → runs tick loop → returns `BacktestResult`
//! - `RingBufferSet`: correct multi-file same-instrument loading (NOT `TickBufferSet`)
//! - `Portfolio`: handles position tracking, fills, equity, SL/TP
//! - `EngineContext`: implements `StrategyCtx` — passed to strategy on each tick

use crate::buffer::buffer_set::RingBufferSet;
use crate::buffer::ring_buffer::{RingBuffer, RingIter};
use crate::engine::core::{EngineContext, Signal};
use crate::engine::CommissionConfig;
use crate::instrument::InstrumentId;
use crate::portfolio::{Portfolio, PortfolioConfig};
use chrono::NaiveDate;
use nexus_strategy::{Strategy, StrategyCtx};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use thiserror::Error;

// =============================================================================
// Error types
// =============================================================================

#[derive(Debug, Error)]
pub enum BacktestError {
    #[error("no instrument configured — call .with_instrument()")]
    NoInstrument,

    #[error("no data directory configured — call .with_data_dir()")]
    NoDataDir,

    #[error("no date range configured — call .with_date_range()")]
    NoDateRange,

    #[error("data directory does not exist: {0}")]
    DataDirNotFound(PathBuf),

    #[error("no TVC files found for {symbol} between {start} and {end}")]
    NoFilesFound {
        symbol: String,
        start: String,
        end: String,
    },

    #[error("failed to open RingBufferSet: {0}")]
    BufferSetOpen(String),

    #[error("strategy error during tick processing: {0}")]
    StrategyError(String),

    #[error("portfolio error: {0}")]
    PortfolioError(String),
}

// =============================================================================
// Result type
// =============================================================================

/// Result of a single backtest run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BacktestResult {
    /// Total PnL in dollars.
    pub pnl: f64,
    /// Maximum drawdown in dollars (peak-to-trough).
    pub max_drawdown: f64,
    /// Maximum drawdown as a percentage of equity.
    pub max_drawdown_pct: f64,
    /// Number of completed round-trip trades.
    pub num_trades: u32,
    /// Total ticks processed.
    pub num_ticks: u64,
    /// Final account equity.
    pub final_equity: f64,
    /// Initial account equity.
    pub initial_equity: f64,
    /// Win rate (closed trades with profit > 0 / total closed trades).
    pub win_rate: f64,
    /// Sharpe ratio (annualized, assuming risk-free rate = 0).
    pub sharpe_ratio: f64,
    /// Average trade PnL in dollars.
    pub avg_trade_pnl: f64,
    /// Timestamp of first tick (ns since epoch).
    pub start_ts_ns: u64,
    /// Timestamp of last tick (ns since epoch).
    pub end_ts_ns: u64,
    /// Duration of the backtest in seconds.
    pub duration_secs: f64,
}

impl Default for BacktestResult {
    fn default() -> Self {
        Self {
            pnl: 0.0,
            max_drawdown: 0.0,
            max_drawdown_pct: 0.0,
            num_trades: 0,
            num_ticks: 0,
            final_equity: 0.0,
            initial_equity: 0.0,
            win_rate: 0.0,
            sharpe_ratio: 0.0,
            avg_trade_pnl: 0.0,
            start_ts_ns: 0,
            end_ts_ns: 0,
            duration_secs: 0.0,
        }
    }
}

// =============================================================================
// Builder
// =============================================================================

/// Backtest engine — configure then run.
///
/// # Example
/// ```ignore
/// let result = BacktestEngine::new()
///     .with_instrument("BTCUSDT", "BINANCE")?
///     .with_date_range(start, end)?
///     .with_data_dir("./data")?
///     .run(|| OrbStrategy::new())?;
/// ```
#[derive(Debug, Clone)]
pub struct BacktestEngine {
    instrument: Option<InstrumentId>,
    data_dir: Option<PathBuf>,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
    initial_equity: f64,
    commission_bps: f64,
    stop_loss_pct: f64,
    take_profit_pct: f64,
}

impl Default for BacktestEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl BacktestEngine {
    /// Start building a backtest.
    pub fn new() -> Self {
        Self {
            instrument: None,
            data_dir: None,
            start_date: None,
            end_date: None,
            initial_equity: 100_000.0,
            commission_bps: 0.5,
            stop_loss_pct: 2.0,
            take_profit_pct: 5.0,
        }
    }

    /// Set the instrument by symbol and exchange.
    pub fn with_instrument(self, symbol: &str, exchange: &str) -> Result<Self, BacktestError> {
        let instrument = InstrumentId::new(symbol, exchange);
        Ok(Self { instrument: Some(instrument), ..self })
    }

    /// Set the date range (inclusive) for the backtest.
    /// EST timestamps are used internally — dates are EST dates.
    pub fn with_date_range(mut self, start: NaiveDate, end: NaiveDate) -> Result<Self, BacktestError> {
        if end < start {
            return Err(BacktestError::NoDateRange);
        }
        self.start_date = Some(start);
        self.end_date = Some(end);
        Ok(self)
    }

    /// Set the data directory containing .tvc files.
    pub fn with_data_dir(mut self, dir: PathBuf) -> Result<Self, BacktestError> {
        if !dir.exists() {
            return Err(BacktestError::DataDirNotFound(dir));
        }
        self.data_dir = Some(dir);
        Ok(self)
    }

    /// Set initial equity in dollars. Defaults to 100,000.
    pub fn with_initial_equity(mut self, equity: f64) -> Self {
        self.initial_equity = equity;
        self
    }

    /// Set commission in basis points. E.g. 0.5 = 0.5 bps = 0.005%.
    /// Defaults to 0.5 bps.
    pub fn with_commission_bps(mut self, bps: f64) -> Self {
        self.commission_bps = bps;
        self
    }

    /// Set stop-loss percentage. E.g. 2.0 = 2%.
    /// Defaults to 2%.
    pub fn with_stop_loss_pct(mut self, pct: f64) -> Self {
        self.stop_loss_pct = pct;
        self
    }

    /// Set take-profit percentage. E.g. 5.0 = 5%.
    /// Defaults to 5%.
    pub fn with_take_profit_pct(mut self, pct: f64) -> Self {
        self.take_profit_pct = pct;
        self
    }

    /// Run the backtest with a strategy factory.
    /// The factory is called to create a fresh strategy instance.
    pub fn run<S>(self, strategy_factory: impl Fn() -> S) -> Result<BacktestResult, BacktestError>
    where
        S: Strategy + 'static,
    {
        let instrument = self.instrument.ok_or(BacktestError::NoInstrument)?;
        let data_dir = self.data_dir.ok_or(BacktestError::NoDataDir)?;
        let start_date = self.start_date.ok_or(BacktestError::NoDateRange)?;
        let end_date = self.end_date.ok_or(BacktestError::NoDateRange)?;

        // Find matching TVC files
        let files = find_tvc_files(&data_dir, &instrument, start_date, end_date)?;
        if files.is_empty() {
            return Err(BacktestError::NoFilesFound {
                symbol: instrument.symbol.clone(),
                start: start_date.to_string(),
                end: end_date.to_string(),
            });
        }

        // Load as RingBufferSet (correct multi-file same-instrument support)
        let buffer_set = RingBufferSet::from_files(files)
            .map_err(|e| BacktestError::BufferSetOpen(e.to_string()))?;

        // Create portfolio config
        let commission = CommissionConfig::new(self.commission_bps / 10000.0);
        let config = PortfolioConfig::new(self.initial_equity, commission)
            .with_stop_loss(self.stop_loss_pct)
            .with_take_profit(self.take_profit_pct);

        // Create portfolio
        let mut portfolio = Portfolio::new(self.initial_equity);
        portfolio.register_instrument(instrument.clone());

        // Create strategy
        let mut strategy = strategy_factory();

        // Run tick loop
        let result = run_backtest_loop(
            &buffer_set,
            &instrument,
            start_date,
            end_date,
            &mut portfolio,
            &mut strategy,
            self.initial_equity,
        );

        let BacktestResult {
            pnl,
            max_drawdown,
            max_drawdown_pct,
            num_trades,
            num_ticks,
            final_equity,
            initial_equity,
            start_ts_ns,
            end_ts_ns,
            ..
        } = result;

        let total_wins = portfolio.total_wins();
        let total_losses = portfolio.total_losses();
        let total_closed = total_wins + total_losses;
        let win_rate = if total_closed > 0 {
            total_wins as f64 / total_closed as f64
        } else {
            0.0
        };
        let avg_trade_pnl = if num_trades > 0 { pnl / num_trades as f64 } else { 0.0 };
        let sharpe_ratio = compute_sharpe_from_returns(&portfolio.returns());

        let duration_secs = if end_ts_ns > start_ts_ns {
            (end_ts_ns - start_ts_ns) as f64 / 1_000_000_000.0
        } else {
            0.0
        };

        Ok(BacktestResult {
            pnl,
            max_drawdown,
            max_drawdown_pct,
            num_trades,
            num_ticks,
            final_equity,
            initial_equity,
            win_rate,
            sharpe_ratio,
            avg_trade_pnl,
            start_ts_ns,
            end_ts_ns,
            duration_secs,
        })
    }
}

// =============================================================================
// Core tick loop
// =============================================================================

fn run_backtest_loop<S: Strategy>(
    buffer_set: &RingBufferSet,
    instrument: &InstrumentId,
    start_date: NaiveDate,
    end_date: NaiveDate,
    portfolio: &mut Portfolio,
    strategy: &mut S,
    initial_equity: f64,
) -> BacktestResult {
    let total_ticks = buffer_set.total_ticks();
    let mut num_ticks = 0u64;
    let mut start_ts_ns = 0u64;
    let mut end_ts_ns = 0u64;
    let mut last_price = 0.0f64;

    // EST date range boundaries
    let range_start = date_est_to_utc_ns(start_date);
    let range_end = date_est_to_utc_ns(end_date + chrono::Duration::days(1));

    // Iterate through ALL ticks using binary-search + RingIter.
    // merged_anchors has one entry per anchor (every anchor_interval ticks), NOT per tick.
    // global_tick N means "the Nth tick overall" — find its anchor via binary search, then stream.
    // This correctly handles:
    // - Non-dense anchor spacing (anchor_interval=2 means anchors at tick 0,2,4,...)
    // - Multi-file same-instrument (buffer switches via global tick offset)
    // - Every tick read once, in order, with deltas between anchors decoded correctly
    // Iterate through ticks buffer-by-buffer (sequential, no binary search per tick).
    // For each buffer: start at first anchor, exhaust the RingIter,
    // then advance to next buffer's first anchor.
    // This correctly reads every tick in order with proper delta decoding.
    // Tick iteration: process ticks sequentially buffer-by-buffer.
    // For each buffer: decode first anchor, create RingIter, exhaust it,
    // then advance to next buffer. No state carried across buffer boundaries.
    let buffers = buffer_set.buffers();
    let mut buf_idx: usize = 0;
    let mut ring_iter: Option<RingIter> = None;

    while buf_idx < buffers.len() {
        // Init RingIter at the start of each buffer
        if ring_iter.is_none() {
            let first_offset = buffers[buf_idx].1.first_anchor_offset();
            let first_tick = match buffers[buf_idx].1.decode_anchor_at(first_offset) {
                Ok(t) => t,
                Err(_) => {
                    // Bad buffer — skip it
                    buf_idx += 1;
                    continue;
                }
            };
            ring_iter = Some(buffers[buf_idx].1.iter_from(
                first_offset,
                0,
                first_tick,
                1,
            ));
        }

        // Get next tick from current buffer's RingIter
        let tick = match ring_iter.as_mut().and_then(|i| i.next()) {
            Some(t) => t,
            None => {
                // RingIter exhausted — advance to next buffer
                buf_idx += 1;
                ring_iter = None;
                continue;
            }
        };

        let ts = tick.timestamp_ns;

        // Skip ticks outside our date range
        if ts < range_start || ts >= range_end {
            continue;
        }

        if num_ticks == 0 {
            start_ts_ns = ts;
        }
        end_ts_ns = ts;
        num_ticks += 1;

        // Convert to nexus_types::Tick for strategy
        let price_f64 = tick.price_int as f64 / 1e9;
        let size_f64 = tick.size_int as f64 / 1e9;
        last_price = price_f64;

        let ntick = nexus_types::Tick {
            timestamp_ns: tick.timestamp_ns,
            price: price_f64,
            size: size_f64,
            vpin: 0.0, // VPIN not available in raw tick
        };

        // Build strategy context
        let mut ctx = EngineContext::new(
            100_000.0,
            std::sync::Arc::new(std::sync::Mutex::new(crate::signals::SignalBus::new())),
            std::ptr::null_mut(),
        );
        ctx.subscribe_instruments(vec![instrument.clone()]);

        // Update unrealized PnL for our instrument
        if let Some(state) = portfolio.state_mut(instrument) {
            state.update_unrealized_pnl(last_price);
        }

        // Call strategy on_trade
        if let Some(signal) = strategy.on_trade(instrument.clone(), &ntick, &mut ctx) {
            route_signal(portfolio, instrument, signal, last_price, ts);
        }

        // Update portfolio peaks and record equity curve
        if let Some(state) = portfolio.state_mut(instrument) {
            state.update_peak(last_price);
        }
        portfolio.record_equity();
    }

    let final_equity = portfolio.portfolio_equity();
    let pnl = final_equity - initial_equity;
    let max_drawdown = portfolio.portfolio_max_drawdown();
    let max_drawdown_pct = if initial_equity > 0.0 {
        (max_drawdown / initial_equity) * 100.0
    } else {
        0.0
    };
    let num_trades = portfolio.total_trades() as u32;

    BacktestResult {
        pnl,
        max_drawdown,
        max_drawdown_pct,
        num_trades,
        num_ticks,
        final_equity,
        initial_equity,
        start_ts_ns,
        end_ts_ns,
        ..Default::default()
    }
}

/// Route a signal to the portfolio — open/close/adjust positions.
/// Uses Portfolio's state API to check position and modify directly.
fn route_signal(
    portfolio: &mut Portfolio,
    instrument: &InstrumentId,
    signal: Signal,
    price: f64,
    ts: u64,
) {
    use crate::engine::Signal as EngineSignal;
    let engine_signal = match signal {
        Signal::Buy => EngineSignal::Buy,
        Signal::Sell => EngineSignal::Sell,
        Signal::Close => EngineSignal::Close,
    };

    // Get current position from portfolio state
    let position = portfolio.state(instrument)
        .map(|s| s.position)
        .unwrap_or(0.0);

    let has_position = position != 0.0;
    let is_long = position > 0.0;

    match (engine_signal, has_position, is_long) {
        (EngineSignal::Buy, false, _) | (EngineSignal::Buy, true, false) => {
            // Open long or flip from short to long
            let size = if has_position { position.abs() + 1.0 } else { 1.0 };
            let comm = 0.0005 * size * price; // rough commission
            if let Some(state) = portfolio.state_mut(instrument) {
                if state.position == 0.0 {
                    state.position = size;
                    state.entry_price = price;
                } else {
                    // Flip
                    state.realized_pnl += if state.position > 0.0 {
                        (price - state.entry_price) * state.position.abs()
                    } else {
                        (state.entry_price - price) * state.position.abs()
                    };
                    state.position = size;
                    state.entry_price = price;
                }
                state.equity -= comm;
                state.commissions += comm;
                state.num_trades += 1;
            }
        }
        (EngineSignal::Sell, false, _) | (EngineSignal::Sell, true, true) => {
            // Open short or flip from long to short
            let size = if has_position { position.abs() + 1.0 } else { 1.0 };
            let comm = 0.0005 * size * price;
            if let Some(state) = portfolio.state_mut(instrument) {
                if state.position == 0.0 {
                    state.position = -size;
                    state.entry_price = price;
                } else {
                    // Flip
                    state.realized_pnl += if state.position > 0.0 {
                        (price - state.entry_price) * state.position.abs()
                    } else {
                        (state.entry_price - price) * state.position.abs()
                    };
                    state.position = -size;
                    state.entry_price = price;
                }
                state.equity -= comm;
                state.commissions += comm;
                state.num_trades += 1;
            }
        }
        (EngineSignal::Close, true, _) => {
            // Close open position
            if let Some(state) = portfolio.state_mut(instrument) {
                let pnl = if state.position > 0.0 {
                    (price - state.entry_price) * state.position.abs()
                } else {
                    (state.entry_price - price) * state.position.abs()
                };
                let comm = 0.0005 * state.position.abs() * price;
                state.realized_pnl += pnl;
                state.equity += pnl - comm;
                state.commissions += comm;
                state.position = 0.0;
                state.entry_price = 0.0;
                state.num_trades += 1;
            }
        }
        _ => { /* No action */ }
    }
}

// =============================================================================
// File discovery
// =============================================================================

/// Find all TVC files for an instrument that overlap with the EST date range.
/// Returns sorted by first timestamp for deterministic ordering.
fn find_tvc_files(
    data_dir: &PathBuf,
    instrument: &InstrumentId,
    start_date: NaiveDate,
    end_date: NaiveDate,
) -> Result<Vec<(PathBuf, InstrumentId)>, BacktestError> {
    let symbol = &instrument.symbol;
    let mut matching = Vec::new();

    let entries = std::fs::read_dir(data_dir.as_path())
        .map_err(|e| BacktestError::DataDirNotFound(data_dir.clone()))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("tvc") {
            continue;
        }

        let stem = path.file_stem()
            .unwrap_or_default()
            .to_string_lossy();

        // Match: "BTCUSDT.tvc" or "BTCUSDT_2025-01-02.tvc"
        if !stem.starts_with(symbol) && stem != *symbol {
            continue;
        }

        // Check time range overlap by opening the RingBuffer header
        if let Ok(rb) = RingBuffer::open(&path, instrument.clone()) {
            let file_start = rb.start_time_ns();
            let file_end = rb.end_time_ns();

            let range_start_ns = date_est_to_utc_ns(start_date);
            let range_end_ns = date_est_to_utc_ns(end_date + chrono::Duration::days(1));

            // Overlap: file.time_range ∩ [range_start, range_end)
            if file_end > range_start_ns && file_start < range_end_ns {
                matching.push((path, instrument.clone()));
            }
        }
    }

    // Sort by first timestamp for deterministic tick ordering
    matching.sort_by_key(|(p, id)| {
        RingBuffer::open(p, id.clone())
            .map(|rb| rb.start_time_ns())
            .unwrap_or(u64::MAX)
    });

    Ok(matching)
}

// =============================================================================
// Utilities
// =============================================================================

/// Convert EST date to UTC timestamp (nanoseconds).
/// EST midnight = UTC 05:00. EST date D covers UTC [D-1 22:00, D 22:00).
fn date_est_to_utc_ns(date: NaiveDate) -> u64 {
    let prev_day = date - chrono::Duration::days(1);
    let est_midnight = chrono::NaiveDateTime::new(
        prev_day,
        chrono::NaiveTime::from_hms_opt(22, 0, 0).unwrap(),
    );
    est_midnight.and_utc().timestamp_nanos_opt().unwrap_or(0) as u64
}



/// Compute annualized Sharpe ratio from a return series.
fn compute_sharpe_from_returns(returns: &[f64]) -> f64 {
    if returns.len() < 2 {
        return 0.0;
    }
    let mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let variance = returns.iter()
        .map(|r| (r - mean).powi(2))
        .sum::<f64>() / returns.len() as f64;
    let std_dev = variance.sqrt();
    if std_dev == 0.0 {
        return 0.0;
    }
    // Annualize: daily Sharpe * sqrt(252)
    let daily_sharpe = mean / std_dev;
    daily_sharpe * (252.0_f64).sqrt()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_date_est_to_utc_ns() {
        // Jan 2, 2025 EST midnight = Jan 1, 2025 22:00 UTC
        let ts = date_est_to_utc_ns(NaiveDate::from_ymd_opt(2025, 1, 2).unwrap());
        let expected = chrono::DateTime::parse_from_rfc3339("2025-01-01T22:00:00Z").unwrap();
        assert_eq!(ts, expected.timestamp_nanos_opt().unwrap() as u64);
    }

    #[test]
    fn test_builder_instrument() {
        let engine = BacktestEngine::new()
            .with_instrument("BTCUSDT", "BINANCE")
            .unwrap();
        assert!(engine.instrument.is_some());
    }

    #[test]
    fn test_builder_chain() {
        let result = BacktestEngine::new()
            .with_instrument("BTCUSDT", "BINANCE").unwrap()
            .with_data_dir(PathBuf::from("/tmp")).is_err();
        assert!(result); // /tmp doesn't have our data
    }
}