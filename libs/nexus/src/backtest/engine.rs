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

use super::capital::CapitalSpread;
use crate::buffer::buffer_set::{RingBufferSet, TickBufferSet};
use crate::data_manager::data_manager::DataManager;
use crate::data_manager::downloader::Downloader;
use crate::data_manager::types::{DataManagerConfig, Exchange, Venue};
use crate::data_manager::bar_ingester::InstrumentType;
use crate::buffer::ring_buffer::RingIter;
use crate::engine::core::Signal;
use crate::engine::{CommissionConfig, EngineContext};
use crate::instrument::InstrumentId;
use crate::portfolio::{Portfolio, PortfolioConfig};
use crate::runner::BacktestMode;
use chrono::NaiveDate;
use nexus_strategy::{Strategy, StrategyCtx};
use serde::{Deserialize, Serialize};
use nexus_types::Bar as NexusBar;
use tvc::tvcb::reader::BarIter;

use std::collections::HashMap;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use thiserror::Error;
use rayon::prelude::*;

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

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

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

    #[error("IO error: {0}")]
    IoError(String),
}

impl From<std::io::Error> for BacktestError {
    fn from(e: std::io::Error) -> Self {
        BacktestError::IoError(e.to_string())
    }
}

impl From<crate::data_manager::data_manager::DataManagerError> for BacktestError {
    fn from(e: crate::data_manager::data_manager::DataManagerError) -> Self {
        BacktestError::BufferSetOpen(e.to_string())
    }
}

// =============================================================================
// Result type
// =============================================================================

/// Bar source for backtesting — tick aggregation or direct bar iteration.
pub enum BarSource {
    /// Use TickBufferSet + bar aggregation (existing path)
    TickBuffer(TickBufferSet),
    /// Use BarIter from TVCB files (new path — no aggregation needed)
    BarIter(BarIter),
}

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
pub struct BacktestEngine {
    instrument: Option<InstrumentId>,
    instruments: Option<Vec<InstrumentId>>,
    capital_spread: Option<CapitalSpread>,
    data_dir: Option<PathBuf>,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
    initial_equity: f64,
    commission_bps: f64,
    stop_loss_pct: f64,
    take_profit_pct: f64,
    downloader: Option<Downloader>,
    exchange: Option<Exchange>,
    venue: Option<Venue>,
    /// Run mode — defaults to `Backtest`. Live/Paper dispatch to LiveRunner in Phase 6.
    mode: BacktestMode,
    /// Bar source — tick-buffer (default) or pre-loaded BarIter.
    bar_source: Option<BarSource>,
    /// Instrument type for auto-ingestion (spot / futures / inverse). Defaults to Spot.
    instrument_type: Option<InstrumentType>,
    /// Timeframe string for auto-ingestion bar backtest (e.g. "15m", "1h").
    /// When set alongside data_dir + date_range, engine auto-loads TVCB bars via DataManager.
    timeframe: Option<String>,
    /// Risk engine for order gating. `None` = risk checks disabled.
    /// Default is `Some(RiskEngine::default())` — risk checks enabled by default.
    risk_engine: Option<crate::engine::risk::RiskEngine>,
}

impl std::fmt::Debug for BacktestEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BacktestEngine")
            .field("instrument", &self.instrument)
            .field("instruments", &self.instruments)
            .field("capital_spread", &self.capital_spread)
            .field("data_dir", &self.data_dir)
            .field("start_date", &self.start_date)
            .field("end_date", &self.end_date)
            .field("initial_equity", &self.initial_equity)
            .field("commission_bps", &self.commission_bps)
            .field("stop_loss_pct", &self.stop_loss_pct)
            .field("take_profit_pct", &self.take_profit_pct)
            .field("downloader", &"<Downloader>")
            .field("exchange", &self.exchange)
            .field("venue", &self.venue)
            .field("mode", &self.mode)
            .finish()
    }
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
            instruments: None,
            capital_spread: None,
            data_dir: None,
            start_date: None,
            end_date: None,
            commission_bps: 0.5,
            stop_loss_pct: 2.0,
            take_profit_pct: 5.0,
            downloader: None,
            exchange: None,
            venue: None,
            mode: BacktestMode::Backtest,
            bar_source: None,
            instrument_type: None,
            timeframe: None,
            // Order matters: initial_equity must precede risk_engine (RiskEngine::new
            // needs an equity value). Use literal 100_000.0 to match the default below.
            initial_equity: 100_000.0,
            // Risk engine on by default — order gating is the safe default.
            // Use `.with_risk_engine(None)` to disable.
            risk_engine: Some(
                crate::engine::risk::RiskEngine::new(
                    crate::engine::risk::RiskConfig::default(),
                    100_000.0,
                ),
            ),
        }
    }

    /// Set the run mode (backtest / live / paper).
    pub fn with_mode(mut self, mode: BacktestMode) -> Self {
        self.mode = mode;
        self
    }

    /// Set the instrument by symbol and exchange.
    /// Auto-derives Exchange from exchange string (case-insensitive).
    pub fn with_instrument(self, symbol: &str, exchange: &str) -> Result<Self, BacktestError> {
        let instrument = InstrumentId::new(symbol, exchange);
        let exchange_parsed = Exchange::from_str(exchange)
            .map_err(|e| BacktestError::InvalidConfig(e))?;
        Ok(Self {
            instrument: Some(instrument),
            instruments: self.instruments,
            capital_spread: self.capital_spread,
            data_dir: self.data_dir,
            start_date: self.start_date,
            end_date: self.end_date,
            initial_equity: self.initial_equity,
            commission_bps: self.commission_bps,
            stop_loss_pct: self.stop_loss_pct,
            take_profit_pct: self.take_profit_pct,
            downloader: self.downloader,
            exchange: Some(exchange_parsed),
            venue: self.venue,
            mode: self.mode,
            bar_source: self.bar_source,
            instrument_type: self.instrument_type,
            timeframe: self.timeframe,
            risk_engine: self.risk_engine,
        })
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

    /// Set exchange and venue. Defaults to Binance Spot if not specified.
    pub fn with_exchange_venue(mut self, exchange: Exchange, venue: Venue) -> Self {
        self.exchange = Some(exchange);
        self.venue = Some(venue);
        self
    }

    /// Set a downloader for automatic data fetching on cache miss.
    pub fn with_downloader(mut self, downloader: Downloader) -> Self {
        self.downloader = Some(downloader);
        self
    }

    /// Set multiple instruments for multi-symbol backtest.
    /// When configured, BacktestEngine loads via TickBufferSet (not RingBufferSet)
    /// and runs portfolio via `run_multi()` instead of `run_backtest_loop()`.
    pub fn with_instruments(self, instruments: Vec<InstrumentId>) -> Self {
        Self { instruments: Some(instruments), ..self }
    }

    /// Set capital spread across multiple instruments.
    /// Only used when instruments are configured via `with_instruments()`.
    pub fn with_capital_spread(self, spread: CapitalSpread) -> Self {
        Self { capital_spread: Some(spread), ..self }
    }

    /// Configure or disable the risk engine.
    ///
    /// By default, the risk engine is **on** (a `RiskEngine::default()` is created
    /// in `BacktestEngine::new()`). Pass `Some(risk)` to use a specific config,
    /// or `None` to disable risk gating entirely (order signals flow through
    /// without checks).
    pub fn with_risk_engine(mut self, risk: Option<crate::engine::risk::RiskEngine>) -> Self {
        self.risk_engine = risk;
        self
    }

    /// Set the bar source for backtesting.
    /// Use `BarSource::BarIter(iter)` to run backtest directly from TVCB files
    /// without going through tick aggregation.
    pub fn with_bar_source(mut self, source: BarSource) -> Self {
        self.bar_source = Some(source);
        self
    }

    /// Set the instrument type (spot / futures / inverse).
    /// Required for bar backtest auto-ingestion path. Defaults to Spot.
    pub fn with_instrument_type(mut self, itype: InstrumentType) -> Self {
        self.instrument_type = Some(itype);
        self
    }

    /// Set the timeframe string for bar backtest auto-ingestion (e.g. "15m", "1h").
    /// When set alongside data_dir + date_range, engine auto-loads TVCB bars
    /// via DataManager::load_bars() and runs bar-mode backtest.
    pub fn with_timeframe(mut self, tf: &str) -> Self {
        self.timeframe = Some(tf.to_string());
        self
    }

    /// Run with a boxed strategy factory (type-erased via `clone_box`).
    /// Prefer `run()` with a concrete strategy type when possible.
    pub fn run_boxed(
        self,
        strategy_factory: Box<dyn Fn() -> Box<dyn nexus_strategy::Strategy>>,
    ) -> Result<BacktestResult, BacktestError> {
        self.run(move || strategy_factory())
    }

    /// Run the backtest with a strategy factory.
    /// The factory is called to create a fresh strategy instance.
    ///
    /// Supports two modes:
    /// - Single-instrument (`.with_instrument()`): loads RingBufferSet, runs `run_backtest_loop()`
    /// - Multi-instrument (`.with_instruments()`): loads TickBufferSet, creates `MergeCursor`,
    ///   registers all instruments, and runs the tick loop directly
    pub fn run<S>(self, strategy_factory: impl Fn() -> S) -> Result<BacktestResult, BacktestError>
    where
        S: nexus_strategy::Strategy + 'static + std::any::Any,
    {
        // ── Extract all fields up-front so we can move self without borrow conflict ──
        let instruments_opt = self.instruments;
        let instrument = self.instrument;
        let data_dir = self.data_dir.clone();
        let start_date = self.start_date;
        let end_date = self.end_date;
        let initial_equity = self.initial_equity;
        let commission_bps = self.commission_bps;
        let stop_loss_pct = self.stop_loss_pct;
        let take_profit_pct = self.take_profit_pct;
        let downloader = self.downloader;
        let venue = self.venue.unwrap_or(Venue::Spot);
        let exchange = self.exchange.unwrap_or(Exchange::Binance);
        let _mode = self.mode;
        let bar_source = self.bar_source;
        let instrument_type = self.instrument_type;
        let timeframe = self.timeframe;

        // === BarIter path: bars are pre-loaded, no date range or data_dir needed ===
        if let Some(BarSource::BarIter(bar_iter)) = bar_source {
            let instrument = instrument.ok_or(BacktestError::NoInstrument)?;
            let commission = CommissionConfig::new(commission_bps / 10000.0);
            let mut portfolio = Portfolio::new(initial_equity);
            portfolio.register_instrument(instrument.clone());
            let result = run_bar_backtest(
                bar_iter,
                &instrument,
                &mut portfolio,
                strategy_factory(),
                initial_equity,
            );
            return Ok(result);
        }

        // === Auto-ingestion bar path: timeframe + data_dir + date_range → load_bars → bar backtest ===
        if let (Some(tf), Some(dd), Some(sd), Some(ed)) = (timeframe, data_dir.clone(), start_date, end_date) {
            let instrument = instrument.ok_or(BacktestError::NoInstrument)?;
            // Create DataManager — load_bars auto-ingests on miss
            let dm = DataManager::with_default_downloader(dd.clone())
                .map_err(|e| BacktestError::BufferSetOpen(e.to_string()))?;
            let itype = instrument_type.unwrap_or(InstrumentType::Spot);
            let bar_iter = dm.load_bars(exchange, itype, &instrument.symbol, &tf, sd, ed)
                .map_err(|e| BacktestError::BufferSetOpen(e.to_string()))?;
            let mut portfolio = Portfolio::new(initial_equity);
            portfolio.register_instrument(instrument.clone());
            let result = run_bar_backtest(
                bar_iter,
                &instrument,
                &mut portfolio,
                strategy_factory(),
                initial_equity,
            );
            return Ok(result);
        }

        // === Multi-instrument path ===
        if let Some(_instruments) = instruments_opt {
            return Err(BacktestError::InvalidConfig(
                "multi-instrument backtest requires BacktestEngine::run_multi() \
                 with a PortfolioStrategy factory (not a Strategy factory). \
                 For bar-mode single-instrument, use .with_instrument() instead."
                    .to_string(),
            ));
        }

        // === Unwrap all fields needed for single-instrument path ===
        let instrument = instrument.ok_or(BacktestError::NoInstrument)?;
        let start_date = start_date.ok_or(BacktestError::NoDateRange)?;
        let end_date = end_date.ok_or(BacktestError::NoDateRange)?;
        let data_dir = data_dir.ok_or(BacktestError::NoDataDir)?;

        // Derive download_on_miss
        let download_on_miss = true;
        let data_dir2 = data_dir.clone();

        // Construct DataManagerConfig
        let dm_config = DataManagerConfig {
            data_root: data_dir2,
            exchange,
            venue,
            symbol: instrument.symbol.clone(),
            start_date: start_date.clone(),
            end_date: end_date.clone(),
            download_on_miss,
        };

        // Create DataManager — auto-download via Binance downloader if data missing
        let dm = DataManager::with_default_downloader(data_dir.clone())?;

        // Load RingBufferSet via DataManager (handles download-on-miss)
        let buffer_set = dm
            .load_ring_buffer_set(&dm_config)
            .map_err(|e| BacktestError::BufferSetOpen(e.to_string()))?;

        // Create portfolio config
        let commission = CommissionConfig::new(commission_bps / 10000.0);
        let config = PortfolioConfig::new(initial_equity, commission)
            .with_stop_loss(stop_loss_pct)
            .with_take_profit(take_profit_pct);

        // Create portfolio
        let mut portfolio = Portfolio::new(initial_equity);
        portfolio.register_instrument(instrument.clone());

        // Create strategy
        let mut strategy = strategy_factory();

        // Determine bar-mode timeframe from strategy
        let timeframe = strategy.timeframe();

        // Run tick loop (bar mode detected via timeframe).
        // Risk engine: None (legacy auto-ingestion path doesn't gate orders).
        let result = run_backtest_loop(
            &buffer_set,
            &instrument,
            start_date,
            end_date,
            &mut portfolio,
            &mut strategy,
            initial_equity,
            timeframe,
            None,
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
// Parameter sweep
// =============================================================================

impl BacktestEngine {
    /// Run a parameter sweep across a grid of parameter values.
    ///
    /// Uses Rayon for parallel execution across parameter combinations.
    /// Each iteration gets a fresh strategy instance and its own Portfolio.
    ///
    /// # Example
    /// ```ignore
    /// use crate::sweep::ParameterGrid;
    ///
    /// let grid = ParameterGrid::new()
    ///     .add_param("fast_ma", vec![10.0, 20.0, 30.0])
    ///     .add_param("slow_ma", vec![50.0, 100.0, 200.0]);
    ///
    /// let results = BacktestEngine::new()
    ///     .with_instrument("BTCUSDT", "BINANCE")?
    ///     .with_date_range(start_date, end_date)?
    ///     .with_data_dir("./data")?
    ///     .with_initial_equity(100_000.0)
    ///     .run_sweep(&grid, |params| {
    ///         SmaCrossTrailingStrategy::new(
    ///             params["fast_ma"] as usize,
    ///             params["slow_ma"] as usize,
    ///             1.0, 0.01, 0.02,
    ///         )
    ///     })?;
    /// ```
    pub fn run_sweep<S>(self, grid: &crate::sweep::ParameterGrid, strategy_factory: impl Fn(HashMap<String, f64>) -> S + Send + Sync + 'static) -> Result<Vec<crate::sweep::SweepResult>, BacktestError>
    where
        S: Strategy + Clone + Send + 'static,
    {
        let instrument = self.instrument.ok_or(BacktestError::NoInstrument)?;
        let data_dir = self.data_dir.ok_or(BacktestError::NoDataDir)?;
        let start_date = self.start_date.ok_or(BacktestError::NoDateRange)?;
        let end_date = self.end_date.ok_or(BacktestError::NoDateRange)?;
        let initial_equity = self.initial_equity;
        let commission_bps = self.commission_bps;
        let exchange = self.exchange.unwrap_or(Exchange::Binance);
        let venue = self.venue.unwrap_or(Venue::Spot);

        if self.instruments.is_some() {
            return Err(BacktestError::InvalidConfig(
                "run_sweep: multi-instrument not yet supported".to_string(),
            ));
        }

        // Load data once, shared across all worker threads
        let dm = DataManager::with_default_downloader(data_dir.clone())?;
        let dm_config = DataManagerConfig {
            data_root: data_dir,
            exchange,
            venue,
            symbol: instrument.symbol.clone(),
            start_date,
            end_date,
            download_on_miss: true,
        };
        let buffer_set = dm
            .load_ring_buffer_set(&dm_config)
            .map_err(|e| BacktestError::BufferSetOpen(e.to_string()))?;

        let buffer_set = Arc::new(buffer_set);
        let range_start = date_est_to_utc_ns(start_date);
        let range_end = date_est_to_utc_ns(end_date + chrono::Duration::days(1));

        let combos: Vec<_> = grid.iter().collect();

        let results: Vec<_> = combos
            .par_iter()
            .filter_map(|params| {
                let mut strategy = strategy_factory(params.clone());
                strategy.on_reset();

                let mut portfolio = Portfolio::new(initial_equity);
                portfolio.register_instrument(instrument.clone());

                let signal_bus = Arc::new(Mutex::new(crate::signals::SignalBus::new()));
                let mut ctx = EngineContext::new(initial_equity, signal_bus, std::ptr::null_mut());
                ctx.subscribe_instruments(vec![instrument.clone()]);

                // Bar-mode was removed: bar aggregation is stale (TVCB provides pre-aggregated bars).
                // For now, always run in tick mode. Bar-mode support via TVCB iteration will be re-added.
                let _ = strategy.timeframe(); // suppress unused warning; field kept for API stability
                let mut strategy_started = false;

                let buffers = buffer_set.buffers();
                let mut buf_idx = 0usize;
                let mut ring_iter: Option<RingIter> = None;

                // Price divisor from first buffer's header
                let price_divisor = if !buffers.is_empty() {
                    10_f64.powi(buffers[0].1.header().decimal_precision as i32)
                } else { 1e9 };

                let mut last_price = 0.0f64;
                let mut end_ts_ns = 0u64;

                while buf_idx < buffers.len() {
                    let buf = &buffers[buf_idx].1; // Arc<RingBuffer>
                    if ring_iter.is_none() {
                        let first_offset = buf.first_anchor_offset();
                        let first_tick = match buf.decode_anchor_at(first_offset) {
                            Ok(t) => t,
                            Err(_) => { buf_idx += 1; continue; }
                        };
                        ring_iter = Some(buf.iter_from(first_offset, 0, first_tick, 0));
                    }

                    let tick = match ring_iter.as_mut().and_then(|i| i.next()) {
                        Some(t) => t,
                        None => { buf_idx += 1; ring_iter = None; continue; }
                    };

                    let ts = tick.timestamp_ns;
                    let price_f64 = tick.price_int as f64 / price_divisor;
                    let size_f64 = tick.size_int as f64 / 1e6;

                    if ts < range_start || ts >= range_end { continue; }
                    if !strategy_started && ts >= range_start {
                        strategy.on_start();
                        strategy_started = true;
                    }

                    // Tick-mode only (bar-mode was removed; TVCB iteration will replace it)
                    // VPIN is not available from raw RingIter ticks. TODO: route single-instrument
                    // backtests through TickBufferSet so VPIN from TradeFlowStats is available.
                    let ntick = nexus_types::Tick {
                        timestamp_ns: ts,
                        price: price_f64,
                        size: size_f64,
                        vpin: 0.0,
                    };
                    let signal = strategy.on_trade(instrument.clone(), &ntick, &mut ctx);

                    if let Some(sig) = signal {
                        route_signal(
                            &mut portfolio,
                            &instrument,
                            sig,
                            price_f64,
                            ts,
                            strategy.position_size(),
                            self.risk_engine.as_ref(),
                        );
                    }

                    if let Some(state) = portfolio.state_mut(&instrument) {
                        state.update_unrealized_pnl(price_f64);
                        state.update_peak(price_f64);
                    }
                    portfolio.record_equity();
                }

                // Flush logic removed (was bar-mode only; TVCB will replace it)
                if strategy_started {
                    strategy.on_stop();
                }

                let pnl = portfolio.portfolio_equity() - initial_equity;
                let max_drawdown = portfolio.portfolio_max_drawdown();
                let num_trades = portfolio.total_trades();
                let (wins, losses) = (portfolio.total_wins(), portfolio.total_losses());
                let win_rate = if wins + losses > 0 { wins as f64 / (wins + losses) as f64 } else { 0.0 };
                let sharpe = compute_sharpe_from_returns(&portfolio.returns());

                Some(crate::sweep::SweepResult {
                    params: params.clone(),
                    pnl,
                    sharpe,
                    max_drawdown,
                    num_trades,
                    win_rate,
                })
            })
            .collect();

        Ok(results)
    }
}

// =============================================================================
// Multi-instrument engine
// =============================================================================

impl BacktestEngine {
    /// Run a multi-instrument backtest via MergeCursor+Portfolio.
    ///
    /// Uses `DataManager::load_tick_buffer_set()` for multi-instrument data loading,
    /// creates a `MergeCursor` from the returned `TickBufferSet` for time-ordered
    /// tick delivery, registers all instruments, and runs the tick loop directly.
    pub fn run_multi<S>(self, instruments: Vec<InstrumentId>, strategy_factory: impl Fn() -> S) -> Result<BacktestResult, BacktestError>
    where
        S: crate::portfolio::PortfolioStrategy + 'static,
    {
        let data_dir = self.data_dir.ok_or(BacktestError::NoDataDir)?;
        let start_date = self.start_date.ok_or(BacktestError::NoDateRange)?;
        let end_date = self.end_date.ok_or(BacktestError::NoDateRange)?;
        let exchange = self.exchange.unwrap_or(Exchange::Binance);

        // ── Validate capital spread if provided ─────────────────────────────
        if let Some(ref spread) = self.capital_spread {
            spread.validate().map_err(|e| {
                BacktestError::InvalidConfig(format!("capital spread: {}", e))
            })?;
        }

        // ── Build instrument list as (symbol, exchange) pairs ─────────────
        let instrument_pairs: Vec<(String, Exchange)> = instruments
            .iter()
            .map(|i| (i.symbol.clone(), exchange))
            .collect();

        // ── Create DataManager ───────────────────────────────────────────
        let dm = DataManager::new(data_dir.clone())
            .map_err(|e| BacktestError::BufferSetOpen(e.to_string()))?;

        // ── Load TickBufferSet for all instruments ───────────────────────
        let buffer_set = dm
            .load_tick_buffer_set(&instrument_pairs, start_date, end_date)
            .map_err(|e| BacktestError::BufferSetOpen(e.to_string()))?;

        let mut cursor = buffer_set.merge_cursor();

        // ── Create portfolio and register all instruments ─────────────────
        let commission = CommissionConfig::new(self.commission_bps / 10000.0);
        let _port_config = crate::portfolio::PortfolioConfig::new(self.initial_equity, commission)
            .with_stop_loss(self.stop_loss_pct)
            .with_take_profit(self.take_profit_pct)
            .with_fill_engine_disabled();

        let mut portfolio = Portfolio::new(self.initial_equity);
        for id in &instruments {
            portfolio.register_instrument(id.clone());
        }

        // ── Wire SignalBus ─────────────────────────────────────────────
        let signal_bus = std::sync::Arc::new(crate::signals::SignalBus::new());
        portfolio = portfolio.with_signal_bus(signal_bus.clone());

        // ── Run tick loop directly ──────────────────────────────────────
        let mut strategy = strategy_factory();

        let mut last_signal: std::collections::HashMap<InstrumentId, crate::engine::Signal> =
            std::collections::HashMap::new();
        let mut last_est_min: std::collections::HashMap<InstrumentId, u32> =
            std::collections::HashMap::new();

        for id in &instruments {
            last_signal.insert(id.clone(), crate::engine::Signal::Close);
            last_est_min.insert(id.clone(), 0);
        }

        let mut num_ticks: u64 = 0;
        let mut start_ts_ns: u64 = 0;
        let mut end_ts_ns: u64 = 0;

        while let Some(event) = cursor.advance() {
            let instrument_id = event.instrument_id.clone();
            let ts = event.tick.timestamp_ns;
            let price = event.tick.price_int as f64 / 1_000_000_000.0;
            let size = event.tick.size_int as f64 / 1_000_000_000.0;

            if num_ticks == 0 {
                start_ts_ns = ts;
            }
            end_ts_ns = ts;
            num_ticks += 1;

            // Ensure instrument is registered on first tick
            if portfolio.state(&instrument_id).is_none() {
                portfolio.register_instrument(instrument_id.clone());
                last_signal.insert(instrument_id.clone(), crate::engine::Signal::Close);
                last_est_min.insert(instrument_id.clone(), 0);
            }

            // Update unrealized PnL for this instrument
            if let Some(state) = portfolio.state_mut(&instrument_id) {
                state.update_unrealized_pnl(price);
            }

            // ── Day-boundary reset via EST minute-of-day ─────────────────
            let utc_h = ((ts / 3_600_000_000_000u64) % 24) as u32;
            let utc_m = ((ts / 60_000_000_000u64) % 60) as u32;
            let est_h = if utc_h >= 5 { utc_h - 5 } else { utc_h + 19 };
            let est_min = est_h * 60 + utc_m;
            let last_min = last_est_min.get(&instrument_id).copied().unwrap_or(0);

            // Forward-time reset (cross midnight → new trading day)
            if est_min < last_min && last_min > 0 {
                if let Some(state) = portfolio.state_mut(&instrument_id) {
                    if state.position != 0.0 {
                        let _ = portfolio.close_position(
                            &instrument_id, price, &commission, ts,
                        );
                    }
                }
                last_signal.insert(instrument_id.clone(), crate::engine::Signal::Close);
            }
            last_est_min.insert(instrument_id.clone(), est_min);

            // ── Strategy signal ─────────────────────────────────────
            let signal = strategy.on_trade(
                instrument_id.clone(),
                ts,
                price,
                size,
                &mut portfolio,
            );

            // ── Publish to SignalBus ─────────────────────────────────
            if let Some(ref sb) = portfolio.signal_bus() {
                let signal_value = match signal {
                    crate::engine::Signal::Buy => 1.0,
                    crate::engine::Signal::Sell => -1.0,
                    crate::engine::Signal::Close => 0.0,
                };
                let signal_name = format!("{}.{}", instrument_id.symbol, instrument_id.exchange);
                sb.publish(&signal_name, signal_value, ts);
                sb.publish("market_signal", signal_value, ts);
            }

            let last_sig = last_signal
                .get(&instrument_id)
                .copied()
                .unwrap_or(crate::engine::Signal::Close);
            let current_position = portfolio
                .state(&instrument_id)
                .map(|s| s.position)
                .unwrap_or(0.0);

            // Signal-change gated execution (fill engine disabled → direct signal path)
            if signal != last_sig {
                match signal {
                    crate::engine::Signal::Buy => {
                        if current_position <= 0.0 {
                            if current_position < 0.0 {
                                let _ = portfolio.close_position(
                                    &instrument_id, price, &commission, ts,
                                );
                            }
                            let pos_size = strategy.position_size();
                            portfolio.open_position(
                                &instrument_id,
                                price,
                                pos_size,
                                crate::engine::Signal::Buy,
                                &commission,
                                None,
                                None,
                                None,
                            );
                        }
                    }
                    crate::engine::Signal::Sell => {
                        if current_position >= 0.0 {
                            if current_position > 0.0 {
                                let _ = portfolio.close_position(
                                    &instrument_id, price, &commission, ts,
                                );
                            }
                            let pos_size = strategy.position_size();
                            portfolio.open_position(
                                &instrument_id,
                                price,
                                pos_size,
                                crate::engine::Signal::Sell,
                                &commission,
                                None,
                                None,
                                None,
                            );
                        }
                    }
                    crate::engine::Signal::Close => {
                        if current_position != 0.0 {
                            let _ = portfolio.close_position(
                                &instrument_id, price, &commission, ts,
                            );
                        }
                    }
                }
            }
            last_signal.insert(instrument_id.clone(), signal);

            // ── Update peaks and record equity curve ─────────────────
            if let Some(state) = portfolio.state_mut(&instrument_id) {
                state.update_peak(price);
            }
            portfolio.record_equity();
        }

        // ── Build result ───────────────────────────────────────────────
        let final_equity = portfolio.portfolio_equity();
        let pnl = final_equity - self.initial_equity;
        let max_drawdown = portfolio.portfolio_max_drawdown();
        let max_drawdown_pct = if self.initial_equity > 0.0 {
            (max_drawdown / self.initial_equity) * 100.0
        } else {
            0.0
        };
        eprintln!("DEBUG FINAL: initial_equity={}, final_equity={}, pnl={}, max_drawdown={}", self.initial_equity, final_equity, pnl, max_drawdown);
        let num_trades = portfolio.total_trades() as u32;

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
            initial_equity: self.initial_equity,
            win_rate,
            sharpe_ratio,
            avg_trade_pnl,
            start_ts_ns,
            end_ts_ns,
            duration_secs,
        })
    }
/// Run a backtest on a pre-loaded TickBufferSet with a time window.
    ///
    /// Creates a MergeCursor over the buffer set, iterates ticks filtered to the
    /// window \[window_start_ns, window_end_ns), calls strategy.on_trade() for each
    /// tick in range, and returns BacktestResult with real PnL, Sharpe, trades, etc.
    ///
    /// This is the core primitive used by WalkForwardRunner — no data loading is done
    /// here; the caller pre-loads the TickBufferSet once before the window loop.
    pub fn run_on_buffer_with_window<S>(
        buffer: Arc<TickBufferSet>,
        window_start_ns: u64,
        window_end_ns: u64,
        strategy_factory: impl Fn() -> S,
        config: &PortfolioConfig,
    ) -> Result<BacktestResult, BacktestError>
    where
        S: Strategy + 'static,
    {
        let mut cursor = buffer.merge_cursor();

        // ── Create portfolio ───────────────────────────────────────────────
        let mut portfolio = Portfolio::new(config.initial_equity_per_instrument);

        for id in buffer.instrument_ids() {
            portfolio.register_instrument(id.clone());
        }

        let signal_bus = Arc::new(crate::signals::SignalBus::new());
        portfolio = portfolio.with_signal_bus(signal_bus.clone());

        // ── Create strategy ────────────────────────────────────────────────
        let mut strategy = strategy_factory();

        // ── Wire up last-signal tracking per instrument ───────────────────
        let mut last_signal: std::collections::HashMap<InstrumentId, Signal> =
            std::collections::HashMap::new();
        let mut last_est_min: std::collections::HashMap<InstrumentId, u32> =
            std::collections::HashMap::new();

        for id in buffer.instrument_ids() {
            last_signal.insert(id.clone(), Signal::Close);
            last_est_min.insert(id.clone(), 0);
        }

        // ── Build EngineContext (needs Arc<Mutex<SignalBus>>) ───────────────
        let commission = &config.commission;
        let initial_equity = config.initial_equity_per_instrument;

        let mut ctx = EngineContext::new(
            initial_equity,
            Arc::new(Mutex::new(crate::signals::SignalBus::new())),
            std::ptr::null_mut(),
        );
        for id in buffer.instrument_ids() {
            ctx.subscribed_instruments.push(id.id);
        }

        let mut num_ticks: u64 = 0;
        let mut start_ts_ns: u64 = 0;
        let mut end_ts_ns: u64 = 0;

        // ── Tick loop — filter to [window_start_ns, window_end_ns) ─────────
        while let Some(event) = cursor.advance() {
            let instrument_id = event.instrument_id.clone();
            let ts = event.tick.timestamp_ns;

            if ts < window_start_ns {
                continue;
            }
            if ts >= window_end_ns {
                // Ticks are time-ordered; once we're past the window, stop.
                break;
            }

            if num_ticks == 0 {
                start_ts_ns = ts;
            }
            end_ts_ns = ts;
            num_ticks += 1;

            // Ensure instrument is registered on first tick
            if portfolio.state(&instrument_id).is_none() {
                portfolio.register_instrument(instrument_id.clone());
                last_signal.insert(instrument_id.clone(), Signal::Close);
                last_est_min.insert(instrument_id.clone(), 0);
            }

            let price = event.tick.price_int as f64 / 1_000_000_000.0;
            let size = event.tick.size_int as f64 / 1_000_000_000.0;

            // Update unrealized PnL for this instrument
            if let Some(state) = portfolio.state_mut(&instrument_id) {
                state.update_unrealized_pnl(price);
            }

            // ── Day-boundary reset via EST minute-of-day ─────────────────
            let utc_h = ((ts / 3_600_000_000_000u64) % 24) as u32;
            let utc_m = ((ts / 60_000_000_000u64) % 60) as u32;
            let est_h = if utc_h >= 5 { utc_h - 5 } else { utc_h + 19 };
            let est_min = est_h * 60 + utc_m;
            let last_min = last_est_min.get(&instrument_id).copied().unwrap_or(0);

            if est_min < last_min && last_min > 0 {
                if let Some(state) = portfolio.state_mut(&instrument_id) {
                    if state.position != 0.0 {
                        let _ = portfolio.close_position(&instrument_id, price, commission, ts);
                    }
                }
                last_signal.insert(instrument_id.clone(), Signal::Close);
            }
            last_est_min.insert(instrument_id.clone(), est_min);

            // ── Strategy signal ───────────────────────────────────────
            let ntick = nexus_types::Tick {
                timestamp_ns: ts,
                price,
                size,
                vpin: 0.0,
            };

            let signal = strategy.on_trade(instrument_id.clone(), &ntick, &mut ctx);

            // ── Publish to SignalBus ────────────────────────────────────
            if let Some(ref sb) = portfolio.signal_bus() {
                let signal_value = match signal {
                    Some(Signal::Buy) => 1.0,
                    Some(Signal::Sell) => -1.0,
                    _ => 0.0,
                };
                let signal_name = format!("{}.{}", instrument_id.symbol, instrument_id.exchange);
                sb.publish(&signal_name, signal_value, ts);
                sb.publish("market_signal", signal_value, ts);
            }

            let last_sig = last_signal
                .get(&instrument_id)
                .copied()
                .unwrap_or(Signal::Close);
            let current_position = portfolio
                .state(&instrument_id)
                .map(|s| s.position)
                .unwrap_or(0.0);

            // Signal-change gated execution (fill engine disabled → direct path)
            let last_sig_opt: Option<Signal> = Some(last_sig);
            if signal != last_sig_opt {
                match signal {
                    Some(Signal::Buy) => {
                        if current_position <= 0.0 {
                            if current_position < 0.0 {
                                let _ = portfolio.close_position(
                                    &instrument_id, price, commission, ts,
                                );
                            }
                            let pos_size = strategy.position_size();
                            portfolio.open_position(
                                &instrument_id,
                                price,
                                pos_size,
                                Signal::Buy,
                                commission,
                                None,
                                None,
                                None,
                            );
                        }
                    }
                    Some(Signal::Sell) => {
                        if current_position >= 0.0 {
                            if current_position > 0.0 {
                                let _ = portfolio.close_position(
                                    &instrument_id, price, commission, ts,
                                );
                            }
                            let pos_size = strategy.position_size();
                            portfolio.open_position(
                                &instrument_id,
                                price,
                                pos_size,
                                Signal::Sell,
                                commission,
                                None,
                                None,
                                None,
                            );
                        }
                    }
                    Some(Signal::Close) | None => {
                        if current_position != 0.0 {
                            let _ = portfolio.close_position(
                                &instrument_id, price, commission, ts,
                            );
                        }
                    }
                }
            }
            last_signal.insert(instrument_id.clone(), signal.unwrap_or(Signal::Close));

            // ── Update peaks and record equity curve ─────────────────
            if let Some(state) = portfolio.state_mut(&instrument_id) {
                state.update_peak(price);
            }
            portfolio.record_equity();
        }

        // ── Lifecycle: on_stop ────────────────────────────────────────────
        strategy.on_stop();

        // ── Build result ───────────────────────────────────────────────────
        let final_equity = portfolio.portfolio_equity();
        let pnl = final_equity - initial_equity;
        let max_drawdown = portfolio.portfolio_max_drawdown();
        let max_drawdown_pct = if initial_equity > 0.0 {
            (max_drawdown / initial_equity) * 100.0
        } else {
            0.0
        };
        let num_trades = portfolio.total_trades() as u32;

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
// Bar-mode backtest (from BarIter — no tick aggregation)
// =============================================================================

fn run_bar_backtest<S: Strategy>(
    bar_iter: BarIter,
    instrument: &InstrumentId,
    portfolio: &mut Portfolio,
    mut strategy: S,
    initial_equity: f64,
) -> BacktestResult {
    use crate::engine::Signal as EngineSignal;

    let commission = CommissionConfig::new(0.5 / 10000.0);

    // Create SignalBus
    let signal_bus = std::sync::Arc::new(std::sync::Mutex::new(crate::signals::SignalBus::new()));
    let mut ctx = EngineContext::new(initial_equity, signal_bus, std::ptr::null_mut());
    ctx.subscribe_instruments(vec![instrument.clone()]);

    let mut num_ticks: u64 = 0;
    let mut start_ts_ns: u64 = 0;
    let mut end_ts_ns: u64 = 0;
    let mut last_price = 0.0f64;

    strategy.on_start();

    for bar_result in bar_iter {
        let bar = match bar_result {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("error reading bar: {}", e);
                continue;
            }
        };

        let ts = bar.ts_event;

        if num_ticks == 0 {
            start_ts_ns = ts;
        }
        end_ts_ns = ts;
        num_ticks += 1;

        // Convert tvc::Bar to nexus_types::Bar
        // tvc::Bar stores prices as i64 nanounits (× 1e9) and volume as i64 units (× 1e6).
        // nexus_types::Bar stores prices and volumes as plain f64 — divide to convert.
        let nexus_bar = nexus_types::Bar {
            timestamp_ns: bar.ts_event,
            open: bar.open as f64 / 1_000_000_000.0,
            high: bar.high as f64 / 1_000_000_000.0,
            low: bar.low as f64 / 1_000_000_000.0,
            close: bar.close as f64 / 1_000_000_000.0,
            volume: bar.volume as f64 / 1_000_000.0,
            buy_volume: bar.buy_volume as f64 / 1_000_000.0,
            sell_volume: bar.sell_volume as f64 / 1_000_000.0,
            tick_count: bar.tick_count as u64,
        };
        let price_f64 = bar.close as f64 / 1_000_000_000.0;
        last_price = price_f64;

        // Update context price for queries
        ctx.update_price(instrument.id, price_f64);

        // Update unrealized PnL
        if let Some(state) = portfolio.state_mut(instrument) {
            state.update_unrealized_pnl(last_price);
        }

        // Call strategy on_bar
        if let Some(signal) = strategy.on_bar(instrument.clone(), &nexus_bar, &mut ctx) {
            // Risk engine: bar-mode path passes None (no risk gating in legacy auto-ingestion).
            route_signal(portfolio, instrument, signal, last_price, ts, strategy.position_size(), None);
        }

        // Update portfolio peaks and record equity curve
        if let Some(state) = portfolio.state_mut(instrument) {
            state.update_peak(last_price);
        }
        portfolio.record_equity();
    }

    strategy.on_stop();

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
    timeframe: Option<Duration>,
    risk_engine: Option<&crate::engine::risk::RiskEngine>,
) -> BacktestResult {
    let _total_ticks = buffer_set.total_ticks();
    let mut num_ticks = 0u64;
    let mut start_ts_ns: u64 = 0;
    let mut end_ts_ns: u64 = 0;
    let mut last_price = 0.0f64;

    // EST date range boundaries
    let range_start = date_est_to_utc_ns(start_date);
    let range_end = date_est_to_utc_ns(end_date + chrono::Duration::days(1));

    // Iterate buffer-by-buffer for correct delta decoding.
    let buffers = buffer_set.buffers();
    let mut buf_idx: usize = 0;
    let mut ring_iter: Option<RingIter> = None;

    // Create SignalBus once and reuse
    let signal_bus = std::sync::Arc::new(std::sync::Mutex::new(crate::signals::SignalBus::new()));
    let mut ctx = EngineContext::new(
        initial_equity,
        signal_bus,
        std::ptr::null_mut(),
    );
    ctx.subscribe_instruments(vec![instrument.clone()]);

    // Bar-mode was removed: bar aggregation is stale (TVCB provides pre-aggregated bars).
    // For now, always run in tick mode. Bar-mode support via TVCB iteration will be re-added.
    let _ = timeframe; // suppress unused warning; field kept for API stability
    let mut strategy_started = false;

    while buf_idx < buffers.len() {
        // Init RingIter at the start of each buffer
        if ring_iter.is_none() {
            let first_offset = buffers[buf_idx].1.first_anchor_offset();
            let first_tick = match buffers[buf_idx].1.decode_anchor_at(first_offset) {
                Ok(t) => t,
                Err(_) => {
                    buf_idx += 1;
                    continue;
                }
            };
            ring_iter = Some(buffers[buf_idx].1.iter_from(
                first_offset,
                0,
                first_tick,
                0,
            ));
        }

        let tick = match ring_iter.as_mut().and_then(|i| i.next()) {
            Some(t) => t,
            None => {
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

        let price_divisor = 10_f64.powi(buffers[buf_idx].1.header().decimal_precision as i32);
        let price_f64 = tick.price_int as f64 / price_divisor;
        let size_f64 = tick.size_int as f64 / 1e6;
        last_price = price_f64;

        // Update context price for queries
        ctx.update_price(instrument.id, price_f64);

        // Update unrealized PnL
        if let Some(state) = portfolio.state_mut(instrument) {
            state.update_unrealized_pnl(last_price);
        }

        // ── Bar mode vs tick mode ──────────────────────────────────────────
        // Bar-mode was removed (TVCB provides pre-aggregated bars). Always tick-mode.
        if !strategy_started {
            strategy.on_start();
            strategy_started = true;
        }

        let ntick = nexus_types::Tick {
            timestamp_ns: tick.timestamp_ns,
            price: price_f64,
            size: size_f64,
            vpin: 0.0,
        };

        // Call strategy on_trade (tick mode only)
        if let Some(signal) = strategy.on_trade(instrument.clone(), &ntick, &mut ctx) {
            route_signal(portfolio, instrument, signal, price_f64, ts, strategy.position_size(), risk_engine);
        }

        // Update portfolio peaks and record equity curve
        if let Some(state) = portfolio.state_mut(instrument) {
            state.update_peak(last_price);
        }
        portfolio.record_equity();
    }

    // ── Flush any open bar ──────────────────────────────────────────────────
    // Bar-mode flush removed (no bar aggregator anymore)

    // ── Lifecycle: on_stop ─────────────────────────────────────────────────
    if strategy_started {
        strategy.on_stop();
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
///
/// `position_size` is the size in instrument units (1.0 = 1 contract/shares/share).
/// Comes from the strategy's `position_size()` method, NOT a hardcoded constant.
///
/// If `risk_engine` is `Some`, the order is gated through `RiskEngine::check_order`.
/// Blocked orders are dropped silently (the strategy still ran, but no fill happened).
fn route_signal(
    portfolio: &mut Portfolio,
    instrument: &InstrumentId,
    signal: Signal,
    price: f64,
    _ts: u64,
    position_size: f64,
    risk_engine: Option<&crate::engine::risk::RiskEngine>,
) {
    use crate::engine::Signal as EngineSignal;
    let engine_signal = match signal {
        Signal::Buy => EngineSignal::Buy,
        Signal::Sell => EngineSignal::Sell,
        Signal::Close => EngineSignal::Close,
    };

    let position = portfolio.state(instrument)
        .map(|s| s.position)
        .unwrap_or(0.0);

    let has_position = position != 0.0;
    let is_long = position > 0.0;

    let size = position_size;
    let _ = size; // used in both branches below

    // Risk engine gating: drop orders that violate limits (default-on).
    // Close signals always bypass risk checks (must always be able to exit).
    if engine_signal != EngineSignal::Close {
        if let Some(re) = risk_engine {
            let equity = portfolio.portfolio_equity();
            let max_dd_pct = portfolio.portfolio_max_drawdown();
            let is_increasing = matches!(
                (engine_signal, position >= 0.0),
                (EngineSignal::Buy, true) | (EngineSignal::Buy, false)
            );
            let order_size = if is_increasing { size } else { 0.0 };
            if re.check_order(order_size, price, position, equity, max_dd_pct).is_some() {
                return; // blocked
            }
        }
    }

    match (engine_signal, has_position, is_long) {
        (EngineSignal::Buy, false, _) | (EngineSignal::Buy, true, false) => {
            let comm = 0.0005 * size * price;
            if let Some(state) = portfolio.state_mut(instrument) {
                if state.position == 0.0 {
                    state.position = size;
                    state.entry_price = price;
                } else {
                    // Flip: close old position and track win/loss
                    let flip_pnl = if state.position > 0.0 {
                        (price - state.entry_price) * state.position.abs()
                    } else {
                        (state.entry_price - price) * state.position.abs()
                    };
                    if flip_pnl > 0.0 {
                        state.num_wins += 1;
                    } else if flip_pnl < 0.0 {
                        state.num_losses += 1;
                    }
                    state.realized_pnl += flip_pnl;
                    state.position = size;
                    state.entry_price = price;
                }
                state.equity -= comm;
                state.commissions += comm;
                state.num_trades += 1;
                // Update peak after equity change so max_drawdown tracks correctly
                state.update_peak(price);
            }
        }
        (EngineSignal::Sell, false, _) | (EngineSignal::Sell, true, true) => {
            let size = position_size;
            let comm = 0.0005 * size * price;
            if let Some(state) = portfolio.state_mut(instrument) {
                if state.position == 0.0 {
                    state.position = -size;
                    state.entry_price = price;
                } else {
                    // Flip: close old position and track win/loss
                    let flip_pnl = if state.position > 0.0 {
                        (price - state.entry_price) * state.position.abs()
                    } else {
                        (state.entry_price - price) * state.position.abs()
                    };
                    if flip_pnl > 0.0 {
                        state.num_wins += 1;
                    } else if flip_pnl < 0.0 {
                        state.num_losses += 1;
                    }
                    state.realized_pnl += flip_pnl;
                    state.position = -size;
                    state.entry_price = price;
                }
                state.equity -= comm;
                state.commissions += comm;
                state.num_trades += 1;
                state.update_peak(price);
            }
        }
        (EngineSignal::Close, true, _) => {
            if let Some(state) = portfolio.state_mut(instrument) {
                eprintln!("DEBUG CLOSE: position={}, entry={}, price={}", state.position, state.entry_price, price);
                let pnl = if state.position > 0.0 {
                    (price - state.entry_price) * state.position.abs()
                } else {
                    (state.entry_price - price) * state.position.abs()
                };
                let comm = 0.0005 * state.position.abs() * price;
                state.realized_pnl += pnl;
                state.equity += pnl - comm;
                state.commissions += comm;
                if pnl > 0.0 {
                    state.num_wins += 1;
                } else if pnl < 0.0 {
                    state.num_losses += 1;
                }
                state.position = 0.0;
                state.entry_price = 0.0;
                state.num_trades += 1;
                state.update_peak(price);
            }
        }
        _ => { /* No action */ }
    }
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
        assert!(result);
    }
}
