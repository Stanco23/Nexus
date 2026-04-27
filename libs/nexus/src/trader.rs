//! Trader — Top-level orchestration entry point for live trading.
//!
//! The Trader owns all live trading components and manages their lifecycle:
//! - DataEngine, RiskEngine, Oms
//! - User-defined actors (strategies, adapters)
//! - Optional database for state persistence
//!
//! # Usage
//!
//! ```ignore
//! let config = TraderConfig {
//!     trader_id: TraderId::new("TRADER-001"),
//!     instances: vec![InstanceConfig {
//!         venue: Venue::new("BINANCE"),
//!         account_id: AccountId::new("ACC-001"),
//!         api_key: "...".to_string(),
//!         api_secret: "...".to_string(),
//!         paper_trading: false,
//!     }],
//!     risk: RiskConfig::default(),
//!     database: DatabaseConfig::default(),
//! };
//!
//! let trader = Trader::new(config);
//! trader.add_actor(Box::new(my_strategy));
//! trader.start();
//! ```

use std::sync::Arc;
use thiserror::Error;

use serde::{Deserialize, Serialize};
use tokio;

use crate::actor::{Actor, Clock, MessageBus, SystemClock};
use crate::cache::Cache;
use crate::data::engine::DataEngine;
use crate::database::{Database, MemoryDatabase};
use crate::engine::account::Position;
use crate::engine::oms::Oms;
use crate::engine::orders::Order;
use crate::engine::risk::{RiskConfig, RiskEngine};
use crate::instrument::Venue;
use crate::live::MarketDataAdapter;
use crate::messages::{OrderFilled, SubmitOrder, TraderId};
use crate::paper::PaperExecution;

// =============================================================================
// TraderError
// =============================================================================

#[derive(Debug, Error)]
pub enum TraderError {
    #[error("Actor error: {0}")]
    Actor(String),

    #[error("Database error: {0}")]
    Database(#[from] crate::database::DatabaseError),

    #[error("Clock error: {0}")]
    Clock(String),

    #[error("Trader already started")]
    AlreadyStarted,

    #[error("Trader not started")]
    NotStarted,

    #[error("No database configured for restore")]
    NoDatabase,

    #[error("Actor {0} not found")]
    ActorNotFound(String),

    #[error("Serialization error: {0}")]
    Serialization(String),

    #[error("Deserialization error: {0}")]
    Deserialization(String),
}

// =============================================================================
// InstanceConfig
// =============================================================================

/// Configuration for a single venue instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceConfig {
    pub venue: Venue,
    pub account_id: crate::engine::account::AccountId,
    pub api_key: String,
    pub api_secret: String,
    pub paper_trading: bool,
}

impl Default for InstanceConfig {
    fn default() -> Self {
        Self {
            venue: Venue::new("BINANCE"),
            account_id: crate::engine::account::AccountId::new("DEFAULT"),
            api_key: String::new(),
            api_secret: String::new(),
            paper_trading: true,
        }
    }
}

// =============================================================================
// DatabaseConfig
// =============================================================================

/// Configuration for database persistence.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DatabaseConfig {
    pub path: Option<std::path::PathBuf>,
    pub memory: bool,
}

impl DatabaseConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_path(path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            memory: false,
        }
    }

    pub fn in_memory() -> Self {
        Self {
            path: None,
            memory: true,
        }
    }
}

// =============================================================================
// TraderConfig
// =============================================================================

/// Configuration for a Trader instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraderConfig {
    pub trader_id: TraderId,
    pub instances: Vec<InstanceConfig>,
    pub risk: RiskConfig,
    pub database: DatabaseConfig,
}

impl TraderConfig {
    pub fn new(trader_id: TraderId) -> Self {
        Self {
            trader_id,
            instances: Vec::new(),
            risk: RiskConfig::default(),
            database: DatabaseConfig::default(),
        }
    }

    pub fn with_instances(mut self, instances: Vec<InstanceConfig>) -> Self {
        self.instances = instances;
        self
    }

    pub fn with_risk(mut self, risk: RiskConfig) -> Self {
        self.risk = risk;
        self
    }

    pub fn with_database(mut self, database: DatabaseConfig) -> Self {
        self.database = database;
        self
    }
}

impl Default for TraderConfig {
    fn default() -> Self {
        Self {
            trader_id: TraderId::new("DEFAULT-TRADER"),
            instances: Vec::new(),
            risk: RiskConfig::default(),
            database: DatabaseConfig::default(),
        }
    }
}

// =============================================================================
// Trader
// =============================================================================

/// Top-level orchestration entry point for live trading.
///
/// The Trader owns all core live trading components and manages their lifecycle.
/// It provides a single start/stop interface for the entire trading system.
///
/// # Components
///
/// - **DataEngine**: Handles market data subscriptions and routing
/// - **RiskEngine**: Pre-trade and intraday risk management
/// - **Oms**: Order management system and state machine
/// - **Actors**: User-defined strategies, adapters, and other components
///
/// # Example
///
/// ```ignore
/// let mut trader = Trader::new(config);
/// trader.add_actor(Box::new(MyStrategy::new()));
/// trader.add_actor(Box::new(BinanceMarketAdapter::new()));
/// trader.start().expect("Failed to start trader");
/// ```
pub struct Trader {
    trader_id: TraderId,
    config: TraderConfig,
    cache: Arc<std::sync::Mutex<Cache>>,
    msgbus: Arc<MessageBus>,
    clock: Box<dyn Clock>,
    data_engine: Arc<std::sync::Mutex<DataEngine>>,
    risk_engine: RiskEngine,
    oms: Oms,
    actors: Vec<Box<dyn Actor>>,
    database: Option<Arc<dyn Database>>,
    paper_execution: Option<Arc<std::sync::Mutex<dyn PaperExecution>>>,
    started: bool,
}

impl Trader {
    /// Create a new Trader from configuration.
    ///
    /// Initializes all core components (DataEngine, RiskEngine, Oms, Cache).
    /// Call `add_actor()` to register strategies and adapters, then `start()`.
    pub fn new(config: TraderConfig) -> Self {
        let cache = Arc::new(std::sync::Mutex::new(Cache::new(100_000, 10_000)));
        let msgbus = Arc::new(MessageBus::new());
        let clock: Box<dyn Clock> = Box::new(SystemClock::new());

        // Build DataEngine with shared msgbus — registers endpoints for subscribe/unsubscribe
        let data_engine = Arc::new(std::sync::Mutex::new(DataEngine::new_with_components(
            config.trader_id.clone(),
            Arc::clone(&msgbus),
            Box::new(SystemClock::new()),
        )));
        // Initialize the DataEngine (must hold lock during init to avoid races)
        data_engine.lock().unwrap().initialize();

        // Build RiskEngine with configured risk parameters
        let risk_engine = RiskEngine::new(config.risk.clone(), 100_000.0);

        // Build Oms with shared cache and msgbus
        let oms = Oms::new(
            Arc::clone(&cache),
            Arc::clone(&msgbus),
            crate::engine::account::OmsType::Hedge,
            None,
            None, // submit_child: set via set_submit_child_fn() in live mode
        );

        // Initialize database if configured
        let database = if config.database.memory {
            Some(Arc::new(MemoryDatabase::new()) as Arc<dyn Database>)
        } else if let Some(ref path) = config.database.path {
            let db = crate::database::SqliteDatabase::new(path);
            match db.open() {
                Ok(_) => Some(Arc::new(db) as Arc<dyn Database>),
                Err(e) => {
                    eprintln!("Trader::new: failed to open database at {:?}: {}", path, e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            trader_id: config.trader_id.clone(),
            config,
            cache,
            msgbus: Arc::clone(&msgbus),
            clock,
            data_engine,
            risk_engine,
            oms,
            actors: Vec::new(),
            database,
            paper_execution: None,
            started: false,
        }
    }

    /// Set paper execution engine for simulated trading.
    /// When set, `paper_submit_order()` routes orders through the paper engine
    /// instead of live execution. Paper fills are applied to the same OMS used
    /// for live trading, keeping positions in sync.
    pub fn with_paper_execution(mut self, exec: Arc<std::sync::Mutex<dyn PaperExecution>>) -> Self {
        self.paper_execution = Some(exec);
        self
    }

    /// Submit an order to the paper execution engine.
    /// Returns fills generated by the paper simulation (sync, immediate).
    /// Fills are applied to the OMS to keep position state in sync.
    /// Panics if paper execution is not configured — check `paper_execution.is_some()` first.
    pub fn paper_submit_order(&mut self, submit: &SubmitOrder) -> Vec<OrderFilled> {
        let paper = self.paper_execution.as_ref().expect(
            "paper_submit_order called but no paper_execution configured. Call with_paper_execution() first.",
        );
        let mut paper = paper.lock().unwrap();
        let client_order_id = submit.client_order_id.clone();
        let fills = paper.submit_order(submit);
        for fill in &fills {
            self.oms.apply_fill(&client_order_id, fill);
        }
        fills
    }

    /// Add an actor (strategy, adapter, etc.) to the trader.
    ///
    /// Actors are started in registration order when `start()` is called.
    pub fn add_actor(&mut self, actor: Box<dyn Actor>) {
        self.actors.push(actor);
    }

    /// Start all actors and core components.
    ///
    /// Calls `on_start()` on all actors in order.
    /// Returns an error if the trader was already started.
    pub fn start(&mut self) -> Result<(), TraderError> {
        if self.started {
            return Err(TraderError::AlreadyStarted);
        }

        for actor in &mut self.actors {
            actor.component_mut().initialize();
            actor.component_mut().start();
            actor.on_start();
        }

        self.started = true;
        Ok(())
    }

    /// Stop all actors and core components.
    ///
    /// Calls `on_stop()` on all actors in reverse order.
    /// Safe to call even if the trader is not running.
    pub fn stop(&mut self) {
        if !self.started {
            return;
        }

        // Stop actors in reverse order
        for actor in self.actors.iter_mut().rev() {
            actor.on_stop();
            actor.component_mut().stop();
        }

        self.started = false;
    }

    /// Restore trader state from the database.
    ///
    /// Loads all orders, positions, and accounts from the configured database
    /// and reconstructs internal state. Should be called after construction
    /// and before `start()` when resuming a previous session.
    ///
    /// # Errors
    ///
    /// Returns `NoDatabase` if no database was configured at construction.
    pub fn restore_from_database(&mut self) -> Result<(), TraderError> {
        let db = self.database.as_ref().ok_or(TraderError::NoDatabase)?;

        // Load orders and positions from database
        let orders = db.load_orders()?;
        let positions = db.load_positions()?;
        let accounts = db.load_accounts()?;

        // Restore to cache
        let mut cache = self.cache.lock().unwrap();

        for (_coid, order) in orders {
            cache.update_order(order);
        }

        for (_pid, position) in positions {
            cache.update_position(position);
        }

        for (_aid, account) in accounts {
            cache.add_account(account);
        }

        Ok(())
    }

    /// Get a reference to the cache.
    pub fn cache(&self) -> &Arc<std::sync::Mutex<Cache>> {
        &self.cache
    }

    /// Get a reference to the message bus.
    pub fn msgbus(&self) -> &MessageBus {
        &self.msgbus
    }

    /// Get a reference to the data engine.
    pub fn data_engine(&self) -> &Arc<std::sync::Mutex<DataEngine>> {
        &self.data_engine
    }

    /// Get a mutable reference to the data engine.
    pub fn data_engine_mut(&mut self) -> &mut Arc<std::sync::Mutex<DataEngine>> {
        &mut self.data_engine
    }

    /// Get a reference to the risk engine.
    pub fn risk_engine(&self) -> &RiskEngine {
        &self.risk_engine
    }

    /// Get a mutable reference to the risk engine.
    pub fn risk_engine_mut(&mut self) -> &mut RiskEngine {
        &mut self.risk_engine
    }

    /// Get a reference to the OMS.
    pub fn oms(&self) -> &Oms {
        &self.oms
    }

    /// Get a mutable reference to the OMS.
    pub fn oms_mut(&mut self) -> &mut Oms {
        &mut self.oms
    }

    /// Get a reference to the clock.
    pub fn clock(&self) -> &dyn Clock {
        &*self.clock
    }

    /// Get the trader ID.
    pub fn trader_id(&self) -> &TraderId {
        &self.trader_id
    }

    /// Get the trader configuration.
    pub fn config(&self) -> &TraderConfig {
        &self.config
    }

    /// Check if the trader is running.
    pub fn is_started(&self) -> bool {
        self.started
    }

    /// Get the count of registered actors.
    pub fn actor_count(&self) -> usize {
        self.actors.len()
    }

    /// Remove an actor from the traders list by ID.
    ///
    /// Returns the removed actor on success, or an error if not found.
    pub fn remove_actor(&mut self, actor_id: &str) -> Result<Box<dyn Actor>, TraderError> {
        let idx = self
            .actors
            .iter()
            .position(|a| a.component().id.to_string() == actor_id || a.component().name == actor_id);

        match idx {
            Some(idx) => Ok(self.actors.remove(idx)),
            None => Err(TraderError::ActorNotFound(actor_id.to_string())),
        }
    }

    /// Serialize the entire Trader state for persistence.
    ///
    /// Returns a binary blob containing trader_id, config, positions, orders,
    /// and equity. Use `load()` to restore from this state.
    pub fn save(&self) -> Vec<u8> {
        let cache = self.cache.lock().unwrap();

        // Collect all positions from cache
        let positions: Vec<Position> = cache.get_all_positions().into_iter().cloned().collect();

        // Collect all orders from cache
        let orders: Vec<Order> = cache.get_all_orders().into_iter().cloned().collect();

        // Calculate total equity from accounts
        let equity: f64 = cache.get_all_accounts().iter().map(|a| a.equity()).sum();

        let state = TraderState {
            trader_id: self.trader_id.clone(),
            positions,
            orders,
            equity,
        };

        serde_json::to_vec(&state).unwrap_or_default()
    }

    /// Deserialize and restore Trader state.
    ///
    /// Parses the bytes and restores positions, orders, and equity to the cache.
    /// Returns an error if deserialization fails.
    pub fn load(&mut self, state: &[u8]) -> Result<(), TraderError> {
        let state: TraderState = serde_json::from_slice(state)
            .map_err(|e| TraderError::Deserialization(e.to_string()))?;

        let mut cache = self.cache.lock().unwrap();

        // Restore positions
        for position in state.positions {
            cache.update_position(position);
        }

        // Restore orders
        for order in state.orders {
            cache.update_order(order);
        }

        Ok(())
    }

    /// Check for open orders and positions that weren't closed properly.
    ///
    /// On startup, this helps detect state inconsistencies from improper shutdowns.
    /// Returns a list of warning messages for any residual open state.
    pub fn check_residuals(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        let cache = self.cache.lock().unwrap();

        // Check for open orders
        for coid in cache.get_open_order_ids() {
            if let Some(order) = cache.get_all_orders().iter().find(|o| &o.client_order_id == coid) {
                warnings.push(format!(
                    "Open order: {} on {}",
                    order.client_order_id, order.instrument_id
                ));
            }
        }

        // Check for OMS open orders
        for coid in cache.get_open_oms_order_ids() {
            if let Some(order) = cache.get_oms_order(coid) {
                warnings.push(format!(
                    "Open OMS order: {} on {}",
                    order.client_order_id, order.instrument_id
                ));
            }
        }

        // Check for open positions
        for pid in cache.get_open_position_ids() {
            if let Some(position) = cache.get_position(pid) {
                let side_str = match position.side {
                    crate::messages::OrderSide::Buy => "LONG",
                    crate::messages::OrderSide::Sell => "SHORT",
                };
                warnings.push(format!(
                    "Open position: {} {} {} {}",
                    side_str,
                    position.quantity,
                    position.instrument_id,
                    position.position_id
                ));
            }
        }

        warnings
    }
}

// =============================================================================
// TraderState — Serialization
// =============================================================================

/// Serializable state for Trader persistence.
///
/// Captures trading state needed to resume a session after restart.
/// Does NOT include database config (which may contain secrets).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraderState {
    pub trader_id: TraderId,
    pub positions: Vec<Position>,
    pub orders: Vec<Order>,
    pub equity: f64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor::{Actor, Component, MessageBus, TestClock, TradingState};
    use crate::messages::StrategyId;
    use std::sync::Arc;

    struct TestActor {
        component: Component,
        trader_id: TraderId,
        started: bool,
        stopped: bool,
    }

    impl TestActor {
        fn new(trader_id: TraderId, id: u64) -> Self {
            let bus = MessageBus::new();
            let clock = Box::new(TestClock::new());
            let logger = crate::actor::Logger::new("TestActor");
            let component = Component::new(id, "TestActor", trader_id.clone(), clock, bus, logger);
            Self {
                component,
                trader_id,
                started: false,
                stopped: false,
            }
        }
    }

    impl Actor for TestActor {
        fn component(&self) -> &Component {
            &self.component
        }

        fn component_mut(&mut self) -> &mut Component {
            &mut self.component
        }

        fn trader_id(&self) -> &str {
            &self.trader_id.0
        }

        fn trader_id_obj(&self) -> &TraderId {
            &self.trader_id
        }

        fn on_start(&mut self) {
            self.started = true;
        }

        fn on_stop(&mut self) {
            self.stopped = true;
        }
    }

    #[test]
    fn test_trader_new_creates_all_components() {
        let config = TraderConfig::new(TraderId::new("TEST-TRADER-001"));
        let trader = Trader::new(config);

        assert_eq!(trader.trader_id.to_string(), "TEST-TRADER-001");
        assert!(!trader.is_started());
        assert_eq!(trader.actor_count(), 0);
    }

    #[test]
    fn test_trader_add_actor() {
        let mut config = TraderConfig::new(TraderId::new("TEST-TRADER"));
        config.database = DatabaseConfig::in_memory();
        let mut trader = Trader::new(config);

        let actor = TestActor::new(TraderId::new("TEST-TRADER"), 1);
        trader.add_actor(Box::new(actor));

        assert_eq!(trader.actor_count(), 1);
    }

    #[test]
    fn test_trader_start_and_stop() {
        let mut config = TraderConfig::new(TraderId::new("TEST-TRADER"));
        config.database = DatabaseConfig::in_memory();
        let mut trader = Trader::new(config);

        let actor = TestActor::new(TraderId::new("TEST-TRADER"), 1);
        trader.add_actor(Box::new(actor));

        trader.start().expect("start failed");
        assert!(trader.is_started());

        trader.stop();
        assert!(!trader.is_started());
    }

    #[test]
    fn test_trader_start_twice_returns_error() {
        let mut config = TraderConfig::new(TraderId::new("TEST-TRADER"));
        config.database = DatabaseConfig::in_memory();
        let mut trader = Trader::new(config);

        trader.start().expect("first start failed");
        let result = trader.start();
        assert!(matches!(result, Err(TraderError::AlreadyStarted)));
    }

    #[test]
    fn test_trader_restore_from_database_no_database() {
        let config = TraderConfig::new(TraderId::new("TEST-TRADER"));
        let mut trader = Trader::new(config);

        let result = trader.restore_from_database();
        assert!(matches!(result, Err(TraderError::NoDatabase)));
    }

    #[test]
    fn test_trader_config_builder() {
        let config = TraderConfig::new(TraderId::new("BUILDER-TRADER"))
            .with_risk(RiskConfig::new().with_max_position_size(10.0))
            .with_instances(vec![InstanceConfig::default()])
            .with_database(DatabaseConfig::in_memory());

        let trader = Trader::new(config);
        assert_eq!(trader.trader_id.to_string(), "BUILDER-TRADER");
        assert_eq!(trader.actor_count(), 0);
    }

    #[test]
    fn test_trader_components_accessible() {
        let config = TraderConfig::new(TraderId::new("COMPONENT-TEST"))
            .with_database(DatabaseConfig::in_memory());
        let mut trader = Trader::new(config);

        // All core components should be accessible
        let _ = trader.cache();
        let _ = trader.msgbus();
        let _ = trader.data_engine();
        let _ = trader.data_engine_mut();
        let _ = trader.risk_engine();
        let _ = trader.risk_engine_mut();
        let _ = trader.oms();
        let _ = trader.oms_mut();
        let _ = trader.clock();
        let _ = trader.config();
    }

    #[test]
    fn test_trader_remove_actor() {
        let mut config = TraderConfig::new(TraderId::new("TEST-TRADER"));
        config.database = DatabaseConfig::in_memory();
        let mut trader = Trader::new(config);

        let actor = TestActor::new(TraderId::new("TEST-TRADER"), 1);
        trader.add_actor(Box::new(actor));

        assert_eq!(trader.actor_count(), 1);

        // Remove by component name
        let result = trader.remove_actor("TestActor");
        assert!(result.is_ok());
        assert_eq!(trader.actor_count(), 0);
    }

    #[test]
    fn test_trader_remove_actor_not_found() {
        let mut config = TraderConfig::new(TraderId::new("TEST-TRADER"));
        config.database = DatabaseConfig::in_memory();
        let mut trader = Trader::new(config);

        let result = trader.remove_actor("NonExistentActor");
        assert!(matches!(result, Err(TraderError::ActorNotFound(_))));
    }

    #[test]
    fn test_trader_save_and_load() {
        let mut config = TraderConfig::new(TraderId::new("SAVE-LOAD-TRADER"));
        config.database = DatabaseConfig::in_memory();
        let mut trader = Trader::new(config);

        // Save empty state
        let state_bytes = trader.save();
        assert!(!state_bytes.is_empty());

        // Verify we can deserialize
        let loaded: TraderState = serde_json::from_slice(&state_bytes).unwrap();
        assert_eq!(loaded.trader_id.to_string(), "SAVE-LOAD-TRADER");
        assert!(loaded.positions.is_empty());
        assert!(loaded.orders.is_empty());
    }

    #[test]
    fn test_trader_check_residuals_empty() {
        let mut config = TraderConfig::new(TraderId::new("TEST-TRADER"));
        config.database = DatabaseConfig::in_memory();
        let trader = Trader::new(config);

        let residuals = trader.check_residuals();
        assert!(residuals.is_empty());
    }
}

// =============================================================================
// TradingNode — Phase 5.7
// =============================================================================

/// Configuration for the TradingNode runner.
#[derive(Debug, Clone)]
pub struct NodeConfig {
    /// Seconds between health heartbeat writes. 0 = disabled.
    pub health_interval_secs: u64,
    /// Whether to handle SIGTERM for graceful shutdown.
    pub signal_handling: bool,
}


impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            health_interval_secs: 30,
            signal_handling: true,
        }
    }
}


/// TradingNode wraps a Trader with production concerns:
/// - SIGTERM graceful shutdown (via sigwait in a dedicated thread)
/// - Health heartbeat writes to the database
/// - Market data adapter management (Binance, Bybit, OKX)
///
/// The recommended way to run a live trading node.
pub struct TradingNode {
    trader: Arc<std::sync::Mutex<Trader>>,
    config: NodeConfig,
    /// Market data adapters (WebSocket). Each adapter's receive loop is
    /// driven by the tokio runtime started in `run_blocking()`.
    /// Wrapped in Arc<Mutex<..>> so the async adapter tasks can be moved
    /// into the tokio runtime without cloning the Box<dyn Trait>.
    market_adapters: Arc<std::sync::Mutex<Vec<Box<dyn MarketDataAdapter>>>>,
}


impl TradingNode {
    /// Create a new TradingNode wrapping the given Trader.
    pub fn new(trader: Trader, config: NodeConfig) -> Self {
        Self {
            trader: Arc::new(std::sync::Mutex::new(trader)),
            config,
            market_adapters: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Register a market data adapter (Binance, Bybit, OKX, etc.).
    ///
    /// The adapter's WebSocket receive loop is driven by the tokio runtime
    /// started in `run_blocking()`. This must be called BEFORE `run_blocking()`.
    pub fn add_market_adapter(&mut self, adapter: Box<dyn MarketDataAdapter>) {
        self.market_adapters.lock().unwrap().push(adapter);
    }

    /// Start the node: spawns background threads for SIGTERM and health,
    /// then blocks the current thread forever.
    ///
    /// SIGTERM: uses `pthread_sigmask` + `sigwait` via raw libc FFI in a dedicated
    /// waiter thread. No raw signal handler needed.
    ///
    /// Health: pure std thread, locks Trader only for the brief duration of
    /// `clock().timestamp_ns()` + `database.save_heartbeat()`.
    ///
    /// Startup sequence:
    /// 1. Build a single-threaded tokio runtime on the current thread
    /// 2. Enter the runtime and call `trader.start()` — this fires up all
    ///    Actor WS loops (ExecutionClient spawn_local) and connects adapters
    /// 3. Park the async executor in `block_on()` — the runtime's worker
    ///    thread drives all spawned tasks (market data receive loops)
    /// 4. On SIGTERM: `trader.stop()` fires via oneshot signal,
    ///    `block_on()` returns, runtime is dropped (clean shutdown)
    pub fn run_blocking(&self) {
        let trader = Arc::clone(&self.trader);
        let health_secs = self.config.health_interval_secs;
        let signal_on = self.config.signal_handling;
        // Extract adapters from the Arc<Mutex<..>> — drain the vec so each
        // adapter is owned by exactly one tokio task (no use-after-move).
        let adapters: Vec<Box<dyn MarketDataAdapter>> =
            std::mem::take(&mut *self.market_adapters.lock().unwrap());

        // Shutdown signal: SIGTERM handler sends, block_on receives, unblocks
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();

        // SIGTERM via sigwait — pure std, no tokio, no raw C signal handler.
        if signal_on {
            let t_sig = Arc::clone(&trader);
            std::thread::spawn(move || {
                // Block SIGTERM in this thread, then use sigwait to receive it.
                let mut sigset: libc::sigset_t = unsafe { std::mem::zeroed() };
                unsafe { libc::sigemptyset(&mut sigset) };
                unsafe { libc::sigaddset(&mut sigset, libc::SIGTERM) };


                // Apply signal mask: block SIGTERM so sigwait can receive it
                let ret = unsafe {
                    libc::pthread_sigmask(libc::SIG_BLOCK, &sigset, std::ptr::null_mut())
                };
                if ret != 0 {
                    eprintln!("[TradingNode] pthread_sigmask failed: {}", ret);
                    return;
                }

                // sigwait until SIGTERM arrives
                let mut sig: libc::c_int = 0;
                let ret2 = unsafe { libc::sigwait(&sigset, &mut sig) };
                if ret2 != 0 {
                    eprintln!("[TradingNode] sigwait failed: {}", ret2);
                    return;
                }

                eprintln!("[TradingNode] SIGTERM received, shutting down");
                // Signal the block_on to exit and stop the trader
                let _ = shutdown_tx.send(());
                t_sig.lock().unwrap().stop();
            });
        }

        // Health monitor — pure std thread, locks Trader briefly for heartbeat
        if health_secs > 0 {
            let t_hm = Arc::clone(&trader);
            std::thread::spawn(move || {
                let dur = std::time::Duration::from_secs(health_secs);
                loop {
                    std::thread::sleep(dur);
                    let ts = t_hm.lock().unwrap().clock().timestamp_ns();
                    let tid = t_hm.lock().unwrap().trader_id().clone();
                    if let Some(ref db) = t_hm.lock().unwrap().database {
                        if let Err(e) = db.save_heartbeat(&tid, ts) {
                            eprintln!("[TradingNode] heartbeat error: {}", e);
                        }
                    }
                }
            });
        }

        // Build and run the tokio runtime for all async market adapter loops.
        // Single-threaded: supports spawn_local from ExecutionClient::on_start().
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime build should not fail");

        // Enter the runtime so we can spawn + call trader.start() from this thread
        runtime.block_on(async {
            // Start all actors (ExecutionClient, etc.) — this calls connect().await
            // which spawn_local's the WS receive loops onto THIS runtime thread.
            trader.lock().unwrap().start().expect("trader already started");

            // Start all market data adapters — each connect().await spawns the
            // WebSocket receive loop as a background tokio task.
            for mut adapter in adapters {
                tokio::spawn(async move {
                    if let Err(e) = adapter.connect().await {
                        eprintln!("[TradingNode] market adapter connect error: {}", e);
                    }
                    // receive_loop runs indefinitely inside connect(), reconnecting on drop
                });
            }

            // Park the async executor forever — exit when SIGTERM fires
            tokio::select! {
                _ = tokio::task::yield_now() => {
                    // yield_to! macro loops forever on this branch
                    loop {
                        tokio::task::yield_now().await;
                    }
                }
                _ = &mut shutdown_rx => {
                    eprintln!("[TradingNode] shutdown signal received");
                }
            }
        });

        // Runtime shutdown: all adapter tasks are dropped, connections closed
        eprintln!("[TradingNode] runtime exited");
    }
}