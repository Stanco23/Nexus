//! Backtest cache — authoritative state for orders, positions, accounts, and ticks.
//!
//! The cache maintains indices for all US-003, US-004, US-005 query paths:
//! - US-003: `get_order`, `get_orders_for_strategy`, `get_orders_for_instrument`
//! - US-004: `get_positions_open`, `get_equity_for_venue`
//! - US-005: `Cache::snapshot`

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::database::{Database, DatabaseError, SqliteDatabase};
use crate::engine::account::{Account, AccountId, Position};
use crate::engine::orders::Order;
pub use crate::buffer::BarType;
use crate::instrument::{InstrumentId, Venue};
use crate::instrument::registry::InstrumentRegistry;
use crate::instrument::Instrument as InstrumentDef;
use crate::messages::{ClientOrderId, PositionId, StrategyId, VenueOrderId};
use crate::engine::oms::{OmsOrder, OrderState};

// =============================================================================
// SECTION 1: Data Types (TradeTick, Bar, QuoteTick)
// =============================================================================

/// Trade side — which side initiated the trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TradeSide {
    Buy,
    Sell,
}

/// A single trade tick — directional price/size event.
///
/// Produced by live WebSocket adapters and stored in pre-decoded RingBuffers.
/// Used by DataEngine for fan-out to subscribed actors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TradeTick {
    /// Nanoseconds timestamp of the trade event.
    pub ts_event: u64,
    /// Nanoseconds timestamp when received by the system.
    pub ts_init: u64,
    /// Trade price.
    pub price: f64,
    /// Trade size (quantity).
    pub size: f64,
    /// Which side initiated the trade (aggressor).
    pub side: TradeSide,
    /// Optional trade identifier from exchange.
    pub trade_id: Option<TradeId>,
    /// The instrument for this trade.
    pub instrument_id: InstrumentId,
}

/// A bar — OHLCV price bar with aggregated volume.
///
/// Produced by BarAggregator from tick stream, or loaded from TVC3 storage.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Bar {
    /// Nanoseconds timestamp of the bar (typically bar close time).
    pub ts_event: u64,
    /// Nanoseconds timestamp when received/created.
    pub ts_init: u64,
    /// Opening price.
    pub open: f64,
    /// Highest price in the period.
    pub high: f64,
    /// Lowest price in the period.
    pub low: f64,
    /// Closing price.
    pub close: f64,
    /// Total volume (buy + sell).
    pub volume: f64,
    /// Volume from buy-side aggressor trades.
    pub buy_volume: f64,
    /// Volume from sell-side aggressor trades.
    pub sell_volume: f64,
    /// Number of ticks in this bar.
    pub tick_count: u32,
    /// The instrument for this bar.
    pub instrument_id: InstrumentId,
}

/// Quote tick — best bid/ask snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuoteTick {
    pub ts_event: u64,
    pub ts_init: u64,
    pub bid_price: f64,
    pub ask_price: f64,
    pub bid_size: f64,
    pub ask_size: f64,
    pub instrument_id: InstrumentId,
}

/// Instrument definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instrument {
    pub id: InstrumentId,
    pub raw_symbol: String,
}

pub use crate::instrument::SyntheticInstrument;

/// Bar type — re-exported from buffer module for cache use.
/// Uses buffer::BarType which has symbol + venue + spec + aggregation_source.

/// Order book — level 2 bid/ask.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OrderBook {
    pub instrument_id: InstrumentId,
    pub bids: Vec<(f64, f64)>, // (price, size)
    pub asks: Vec<(f64, f64)>, // (price, size)
    pub ts_event: u64,
}

/// Trade identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TradeId(pub String);

// =============================================================================
// SECTION 2: Cache
// =============================================================================

/// Backtest state cache.
///
/// Maintains all authoritative state plus indices for O(1) lookup by:
/// - venue, strategy, instrument, position_id, client_order_id
/// - open/closed subsets for orders and positions
#[allow(dead_code)]
#[derive(Clone)]
pub struct Cache {
    // Core maps
    instruments: HashMap<InstrumentId, Arc<Instrument>>,
    synthetics: HashMap<InstrumentId, Arc<SyntheticInstrument>>,
    orders: HashMap<ClientOrderId, Order>,
    /// OMS orders — keyed by ClientOrderId for the OMS state machine
    oms_orders: HashMap<ClientOrderId, OmsOrder>,
    positions: HashMap<PositionId, Position>,
    accounts: HashMap<AccountId, Account>,
    quote_ticks: HashMap<InstrumentId, VecDeque<QuoteTick>>,
    trade_ticks: HashMap<InstrumentId, VecDeque<TradeTick>>,
    bars: HashMap<BarType, VecDeque<Bar>>,
    order_books: HashMap<InstrumentId, OrderBook>,

    // Indices
    index_venue_account: HashMap<Venue, AccountId>,
    index_venue_orders: HashMap<Venue, HashSet<ClientOrderId>>,
    index_venue_positions: HashMap<Venue, HashSet<PositionId>>,
    index_order_position: HashMap<ClientOrderId, PositionId>,
    index_order_strategy: HashMap<ClientOrderId, StrategyId>,
    index_strategy_orders: HashMap<StrategyId, HashSet<ClientOrderId>>,
    index_strategy_positions: HashMap<StrategyId, HashSet<PositionId>>,
    index_instrument_orders: HashMap<InstrumentId, HashSet<ClientOrderId>>,
    index_instrument_positions: HashMap<InstrumentId, HashSet<PositionId>>,
    index_orders_open: HashSet<ClientOrderId>,
    index_orders_closed: HashSet<ClientOrderId>,
    // OMS order indices — oms_orders becomes authoritative in Phase 5.4
    index_oms_orders_open: HashSet<ClientOrderId>,
    index_oms_orders_closed: HashSet<ClientOrderId>,
    index_oms_orders_strategy: HashMap<StrategyId, HashSet<ClientOrderId>>,
    index_oms_orders_instrument: HashMap<InstrumentId, HashSet<ClientOrderId>>,
    index_oms_orders_venue: HashMap<VenueOrderId, ClientOrderId>,
    index_positions_open: HashSet<PositionId>,
    index_positions_closed: HashSet<PositionId>,
    // Synthetic instruments index — maps underlying symbol to synthetic IDs
    index_synthetics_underlying: HashMap<String, Vec<InstrumentId>>,

    #[allow(dead_code)]
    // Capacity
    tick_capacity: usize,
    #[allow(dead_code)]
    bar_capacity: usize,

    // Persistence
    database: Option<Arc<dyn Database>>,

    // Instrument registry for symbol/exchange lookup
    registry: Option<Arc<InstrumentRegistry>>,
}

impl Cache {
    pub fn new(tick_capacity: usize, bar_capacity: usize) -> Self {
        Self {
            instruments: HashMap::new(),
            synthetics: HashMap::new(),
            orders: HashMap::new(),
            oms_orders: HashMap::new(),
            positions: HashMap::new(),
            accounts: HashMap::new(),
            quote_ticks: HashMap::new(),
            trade_ticks: HashMap::new(),
            bars: HashMap::new(),
            order_books: HashMap::new(),
            index_venue_account: HashMap::new(),
            index_venue_orders: HashMap::new(),
            index_venue_positions: HashMap::new(),
            index_order_position: HashMap::new(),
            index_order_strategy: HashMap::new(),
            index_strategy_orders: HashMap::new(),
            index_strategy_positions: HashMap::new(),
            index_instrument_orders: HashMap::new(),
            index_instrument_positions: HashMap::new(),
            index_orders_open: HashSet::new(),
            index_orders_closed: HashSet::new(),
            index_oms_orders_open: HashSet::new(),
            index_oms_orders_closed: HashSet::new(),
            index_oms_orders_strategy: HashMap::new(),
            index_oms_orders_instrument: HashMap::new(),
            index_oms_orders_venue: HashMap::new(),
            index_positions_open: HashSet::new(),
            index_positions_closed: HashSet::new(),
            index_synthetics_underlying: HashMap::new(),
            tick_capacity,
            bar_capacity,
            database: None,
            registry: None,
        }
    }

    /// Create a new Cache with an open database connection.
    pub fn with_database(path: impl Into<PathBuf>) -> std::result::Result<Self, DatabaseError> {
        let db = Arc::new(SqliteDatabase::new(path));
        db.open()?;
        Ok(Self {
            database: Some(db),
            instruments: HashMap::new(),
            synthetics: HashMap::new(),
            orders: HashMap::new(),
            oms_orders: HashMap::new(),
            positions: HashMap::new(),
            accounts: HashMap::new(),
            quote_ticks: HashMap::new(),
            trade_ticks: HashMap::new(),
            bars: HashMap::new(),
            order_books: HashMap::new(),
            index_venue_account: HashMap::new(),
            index_venue_orders: HashMap::new(),
            index_venue_positions: HashMap::new(),
            index_order_position: HashMap::new(),
            index_order_strategy: HashMap::new(),
            index_strategy_orders: HashMap::new(),
            index_strategy_positions: HashMap::new(),
            index_instrument_orders: HashMap::new(),
            index_instrument_positions: HashMap::new(),
            index_orders_open: HashSet::new(),
            index_orders_closed: HashSet::new(),
            index_oms_orders_open: HashSet::new(),
            index_oms_orders_closed: HashSet::new(),
            index_oms_orders_strategy: HashMap::new(),
            index_oms_orders_instrument: HashMap::new(),
            index_oms_orders_venue: HashMap::new(),
            index_positions_open: HashSet::new(),
            index_positions_closed: HashSet::new(),
            index_synthetics_underlying: HashMap::new(),
            tick_capacity: 1000,
            bar_capacity: 1000,
            registry: None,
        })
    }

    /// Load cache state from database.
    /// Reconstructs ALL indices from loaded orders and positions.
    pub fn load_from_database(db: Arc<dyn Database>) -> std::result::Result<Self, DatabaseError> {
        let orders = db.load_orders()?;
        let positions = db.load_positions()?;
        let accounts = db.load_accounts()?;

        let mut cache = Self::new(1000, 1000);
        cache.orders = orders;
        cache.positions = positions;
        cache.accounts = accounts;

        // Reconstruct ALL indices from loaded orders
        for (coid, order) in &cache.orders {
            cache.index_order_strategy
                .insert(coid.clone(), order.strategy_id.clone());
            cache.index_strategy_orders
                .entry(order.strategy_id.clone())
                .or_default()
                .insert(coid.clone());
            cache.index_instrument_orders
                .entry(order.instrument_id.clone())
                .or_default()
                .insert(coid.clone());
            if order.filled {
                cache.index_orders_closed.insert(coid.clone());
            } else {
                cache.index_orders_open.insert(coid.clone());
            }
            cache.index_venue_orders
                .entry(order.venue.clone())
                .or_default()
                .insert(coid.clone());
        }

        // Reconstruct ALL indices from loaded positions
        for (pid, pos) in &cache.positions {
            cache.index_venue_positions
                .entry(pos.venue.clone())
                .or_default()
                .insert(pid.clone());
            cache.index_strategy_positions
                .entry(pos.strategy_id.clone())
                .or_default()
                .insert(pid.clone());
            cache.index_instrument_positions
                .entry(pos.instrument_id.clone())
                .or_default()
                .insert(pid.clone());
            if pos.quantity == 0.0 {
                cache.index_positions_closed.insert(pid.clone());
            } else {
                cache.index_positions_open.insert(pid.clone());
            }
        }

        cache.database = Some(db);
        Ok(cache)
    }

    // =============================================================================
    // US-003: Order Queries
    // =============================================================================

    /// Get a single order by client_order_id.
    /// Returns None for OMS-managed orders during transition — use get_oms_order() instead.
    pub fn get_order(&self, id: &ClientOrderId) -> Option<&Order> {
        // During OMS transition, oms_orders is authoritative for new orders
        if self.oms_orders.contains_key(id) {
            return None;
        }
        self.orders.get(id)
    }

    /// Get all orders for a strategy.
    pub fn get_orders_for_strategy(&self, strategy_id: &StrategyId) -> Vec<&Order> {
        self.index_strategy_orders
            .get(strategy_id)
            .map(|ids| ids.iter().filter_map(|id| self.orders.get(id)).collect())
            .unwrap_or_default()
    }

    /// Get all orders for an instrument.
    pub fn get_orders_for_instrument(&self, instrument_id: &InstrumentId) -> Vec<&Order> {
        self.index_instrument_orders
            .get(instrument_id)
            .map(|ids| ids.iter().filter_map(|id| self.orders.get(id)).collect())
            .unwrap_or_default()
    }

    // =============================================================================
    // US-004: Position & Account Queries
    // =============================================================================

    /// Add an account to the cache and register its venue mapping.
    pub fn add_account(&mut self, account: Account) {
        let account_id = account.id.clone();
        let venue_accounts = account.venue_accounts.clone();
        self.accounts.insert(account_id.clone(), account);
        for (venue, acc_id) in venue_accounts {
            self.index_venue_account
                .insert(Venue::new(&venue), acc_id);
        }
    }

    /// Get the AccountId for a venue.
    pub fn account_for_venue(&self, venue: &Venue) -> Option<&AccountId> {
        self.index_venue_account.get(venue)
    }

    /// Get positions open for a venue.
    pub fn get_positions_for_venue(&self, venue: &Venue) -> Vec<&Position> {
        self.index_venue_positions
            .get(venue)
            .map(|ids| ids.iter().filter_map(|id| self.positions.get(id)).collect())
            .unwrap_or_default()
    }

    /// Get position by PositionId.
    pub fn get_position(&self, id: &PositionId) -> Option<&Position> {
        self.positions.get(id)
    }

    /// Get position by InstrumentId.
    /// Returns the first open position for the instrument, if any.
    pub fn get_position_for_instrument(&self, instrument_id: &InstrumentId) -> Option<&Position> {
        self.index_instrument_positions
            .get(instrument_id)
            .and_then(|ids| ids.iter().find(|id| {
                self.positions.get(id).map(|p| p.quantity != 0.0).unwrap_or(false)
            }))
            .and_then(|id| self.positions.get(id))
    }

    /// Get all open positions.
    pub fn get_positions_open(&self) -> Vec<&Position> {
        self.index_positions_open
            .iter()
            .filter_map(|id| self.positions.get(id))
            .collect()
    }

    /// Get equity for a venue account.
    pub fn get_equity_for_venue(&self, venue: &Venue) -> f64 {
        self.index_venue_account
            .get(venue)
            .and_then(|acc_id| self.accounts.get(acc_id))
            .map(|acc| acc.equity())
            .unwrap_or(0.0)
    }

    // =============================================================================
    // US-003 / US-004: Update Methods
    // =============================================================================

    /// Update an order in the cache.
    ///
    /// Handles open/closed transitions based on `order.filled`.
    pub fn update_order(&mut self, order: Order) {
        let client_order_id = order.client_order_id.clone();
        let strategy_id = order.strategy_id.clone();
        let instrument_id = order.instrument_id.clone();
        let filled = order.filled;

        // Insert/update in orders map (order consumed here)
        self.orders.insert(client_order_id.clone(), order);

        // Update index_order_strategy (always)
        self.index_order_strategy
            .insert(client_order_id.clone(), strategy_id.clone());

        // Update open/closed indices based on filled status
        if filled {
            self.index_orders_open.remove(&client_order_id);
            self.index_orders_closed.insert(client_order_id.clone());
        } else {
            self.index_orders_open.insert(client_order_id.clone());
            self.index_orders_closed.remove(&client_order_id);
        }

        // Update instrument index
        self.index_instrument_orders
            .entry(instrument_id)
            .or_default()
            .insert(client_order_id.clone());

        // Update strategy index
        self.index_strategy_orders
            .entry(strategy_id)
            .or_default()
            .insert(client_order_id);
    }

    /// Update an OMS order in the cache.
    /// Maintains all OMS indices (open/closed, strategy, instrument, venue).
    pub fn update_oms_order(&mut self, order: OmsOrder) {
        let client_order_id = order.client_order_id.clone();
        let strategy_id = order.strategy_id.clone();
        let instrument_id = crate::instrument::InstrumentId::parse(&order.instrument_id)
            .unwrap_or_else(|_| crate::instrument::InstrumentId::new(&order.instrument_id, ""));

        // Insert/update in oms_orders map
        self.oms_orders.insert(client_order_id.clone(), order);

        // Determine terminal state
        let state = self.oms_orders.get(&client_order_id).map(|o| o.state.clone()).unwrap_or(OrderState::Pending);

        // Update open/closed indices based on state
        match &state {
            OrderState::Pending | OrderState::Accepted | OrderState::PartiallyFilled => {
                self.index_oms_orders_open.insert(client_order_id.clone());
                self.index_oms_orders_closed.remove(&client_order_id);
            }
            OrderState::Filled | OrderState::Cancelled | OrderState::Rejected => {
                self.index_oms_orders_open.remove(&client_order_id);
                self.index_oms_orders_closed.insert(client_order_id.clone());
                // Remove from venue secondary index on terminal state
                if let Some(venue_id) = self.oms_orders.get(&client_order_id).and_then(|o| o.venue_order_id.clone()) {
                    self.index_oms_orders_venue.remove(&venue_id);
                }
            }
        }

        // Update venue_order_id secondary index (only for non-terminal states)
        if let OrderState::Pending | OrderState::Accepted | OrderState::PartiallyFilled = &state {
            if let Some(venue_id) = self.oms_orders.get(&client_order_id).and_then(|o| o.venue_order_id.clone()) {
                self.index_oms_orders_venue.insert(venue_id, client_order_id.clone());
            }
        }

        // Update strategy index
        self.index_oms_orders_strategy
            .entry(strategy_id.clone())
            .or_default()
            .insert(client_order_id.clone());

        // Update instrument index
        self.index_oms_orders_instrument
            .entry(instrument_id)
            .or_default()
            .insert(client_order_id);
    }

    /// Get an OMS order by client_order_id.
    pub fn get_oms_order(&self, id: &ClientOrderId) -> Option<&OmsOrder> {
        self.oms_orders.get(id)
    }

    /// Get an OMS order by venue_order_id (secondary lookup).
    /// Used when WS messages carry exchange_order_id.
    pub fn get_oms_order_by_venue(&self, venue_order_id: &VenueOrderId) -> Option<&OmsOrder> {
        self.index_oms_orders_venue
            .get(venue_order_id)
            .and_then(|coid| self.oms_orders.get(coid))
    }

    /// Get a ClientOrderId from a VenueOrderId (for reconciliation).
    pub fn get_client_order_id_by_venue(&self, venue_order_id: &VenueOrderId) -> Option<ClientOrderId> {
        self.index_oms_orders_venue.get(venue_order_id).cloned()
    }

    /// Get a mutable reference to the OMS orders map for Oms.
    pub(crate) fn oms_orders_mut(&mut self) -> &mut HashMap<ClientOrderId, OmsOrder> {
        &mut self.oms_orders
    }

    /// Update the venue secondary index when an order is accepted.
    pub(crate) fn oms_update_venue_index(&mut self, venue_order_id: VenueOrderId, client_order_id: ClientOrderId) {
        self.index_oms_orders_venue.insert(venue_order_id, client_order_id);
    }

    /// Get open OMS order client_order_ids for a strategy.
    pub(crate) fn oms_get_open_for_strategy(&self, strategy_id: &StrategyId) -> Vec<ClientOrderId> {
        self.index_oms_orders_strategy
            .get(strategy_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect()
    }

    /// Get open OMS order client_order_ids.
    pub(crate) fn oms_get_open_order_ids(&self) -> Vec<ClientOrderId> {
        self.index_oms_orders_open.iter().cloned().collect()
    }

    /// Check if OMS orders contains a client_order_id.
    pub(crate) fn oms_contains(&self, coid: &ClientOrderId) -> bool {
        self.oms_orders.contains_key(coid)
    }

    /// Get a mutable reference to an OMS order.
    pub(crate) fn oms_get_mut(&mut self, coid: &ClientOrderId) -> Option<&mut OmsOrder> {
        self.oms_orders.get_mut(coid)
    }

    /// Update a position in the cache.
    ///
    /// Handles open/closed transitions based on `position.quantity == 0`.
    pub fn update_position(&mut self, position: Position) {
        let position_id = position.position_id.clone();
        let strategy_id = position.strategy_id.clone();
        let instrument_id = position.instrument_id.clone();
        let quantity = position.quantity;
        let venue = position.venue.clone();

        // Insert/update in positions map (position consumed here)
        self.positions.insert(position_id.clone(), position);

        // Now use position_id for index operations
        if quantity == 0.0 {
            self.index_positions_open.remove(&position_id);
            self.index_positions_closed.insert(position_id.clone());
        } else {
            self.index_positions_open.insert(position_id.clone());
            self.index_positions_closed.remove(&position_id);
        }

        // Update instrument index
        if quantity == 0.0 {
            // Position closed — remove from instrument index
            if let Some(ids) = self.index_instrument_positions.get_mut(&instrument_id) {
                ids.remove(&position_id);
            }
        } else {
            self.index_instrument_positions
                .entry(instrument_id)
                .or_default()
                .insert(position_id.clone());
        }

        // Update strategy index
        if quantity == 0.0 {
            // Position closed — remove from strategy index
            if let Some(ids) = self.index_strategy_positions.get_mut(&strategy_id) {
                ids.remove(&position_id);
            }
        } else {
            self.index_strategy_positions
                .entry(strategy_id)
                .or_default()
                .insert(position_id.clone());
        }

        // maintain venue position index (venue index keeps ALL positions, not just open)
        self.index_venue_positions
            .entry(venue)
            .or_default()
            .insert(position_id);
    }

    // =============================================================================
    // Instrument Registry Integration (Phase 5.0i)
    // =============================================================================

    /// Set the instrument registry for symbol/exchange lookups.
    /// Called by Trader builder during initialization.
    pub fn set_registry(&mut self, registry: Arc<InstrumentRegistry>) {
        self.registry = Some(registry);
    }

    /// Get an instrument by InstrumentId (hash-based).
    /// Delegates to the InstrumentRegistry for authoritative lookups.
    pub fn get_instrument(&self, instrument_id: InstrumentId) -> Option<InstrumentDef> {
        self.registry.as_ref()?.get(instrument_id.id)
    }

    /// Add an instrument to the stub instruments map (backward compat).
    /// Note: This populates the stub `self.instruments` HashMap, NOT the registry.
    /// Converts from the full instrument::Instrument (with symbol/venue/u32 id)
    /// to the cache's Instrument stub (with InstrumentId/raw_symbol).
    pub fn add_instrument(&mut self, instrument: InstrumentDef) {
        let raw_symbol = format!("{}.{}", instrument.symbol, instrument.venue);
        let instrument_id = InstrumentId::new(&instrument.symbol, &instrument.venue);
        let cache_instrument = Instrument {
            id: instrument_id.clone(),
            raw_symbol,
        };
        self.instruments.insert(instrument_id, Arc::new(cache_instrument));
    }

    /// Add a synthetic instrument and populate the underlying index.
    pub fn add_synthetic(&mut self, synthetic: SyntheticInstrument) {
        // Use the first component as the underlying for indexing
        let underlying = if synthetic.component_names.is_empty() {
            "UNKNOWN"
        } else {
            &synthetic.component_names[0]
        };
        let synthetic_id = synthetic.id.clone();
        self.index_synthetics_underlying
            .entry(underlying.to_string())
            .or_default()
            .push(synthetic_id.clone());
        self.synthetics.insert(synthetic_id, Arc::new(synthetic));
    }

    /// Get synthetic instruments by underlying symbol.
    pub fn get_synthetics_by_underlying(&self, symbol: &str) -> Vec<InstrumentId> {
        self.index_synthetics_underlying.get(symbol).cloned().unwrap_or_default()
    }
}

// =============================================================================
// US-005: Cache Snapshot
// =============================================================================

/// Point-in-time snapshot of cache state.
///
/// Serialized for Monte Carlo / Walk-Forward reseeding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheSnapshot {
    pub orders: HashMap<ClientOrderId, Order>,
    pub positions: HashMap<PositionId, Position>,
    pub accounts: HashMap<AccountId, Account>,
    pub timestamp_ns: u64,
}

impl Cache {
    /// Take a point-in-time snapshot of orders, positions, and accounts.
    ///
    /// Note: `timestamp_ns` is left as 0 — the caller sets it to the
    /// current simulation time.
    pub fn snapshot(&self) -> CacheSnapshot {
        let snap = CacheSnapshot {
            orders: self.orders.clone(),
            positions: self.positions.clone(),
            accounts: self.accounts.clone(),
            timestamp_ns: 0,
        };

        if let Some(db) = &self.database {
            for order in self.orders.values() {
                if let Err(e) = db.save_order(order) {
                    eprintln!("snapshot: failed to persist order: {}", e);
                }
            }
            for position in self.positions.values() {
                if let Err(e) = db.save_position(position) {
                    eprintln!("snapshot: failed to persist position: {}", e);
                }
            }
            for account in self.accounts.values() {
                if let Err(e) = db.save_account(account) {
                    eprintln!("snapshot: failed to persist account: {}", e);
                }
            }
            if let Err(e) = db.save_equity_curve(&snap) {
                eprintln!("snapshot: failed to persist equity curve: {}", e);
            }
        }

        snap
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::account::{AccountId, Currency, OmsType, Position, PositionIdGenerator};
    use crate::engine::orders::{OrderSide as OOrderSide, OrderType as OOrderType};
    use crate::messages::{OrderSide, OrderType};

    fn make_test_order(
        client_order_id: &str,
        strategy_id: &str,
        instrument_id: InstrumentId,
        venue: Venue,
        filled: bool,
    ) -> Order {
        let mut order = Order::new(
            1,
            ClientOrderId::new(client_order_id),
            StrategyId::new(strategy_id),
            instrument_id,
            venue,
            OOrderSide::Buy,
            OOrderType::Market,
            0.0,
            1.0,
            None,
            None,
        );
        order.filled = filled;
        order
    }

    fn make_test_position(
        position_id: &str,
        strategy_id: &str,
        instrument_id: InstrumentId,
        venue: Venue,
        quantity: f64,
    ) -> Position {
        Position::new(
            PositionId::new(position_id),
            instrument_id,
            StrategyId::new(strategy_id),
            venue,
            OrderSide::Buy,
            quantity,
            50_000.0,
            0,
        )
    }

    #[test]
    fn test_update_order_moves_open_to_closed() {
        let mut cache = Cache::new(1000, 1000);
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");
        let venue = Venue::new("BINANCE");

        let order = make_test_order("ORD-001", "strat-1", instrument.clone(), venue.clone(), false);
        cache.update_order(order);
        assert!(cache.index_orders_open.contains(&ClientOrderId::new("ORD-001")));

        let filled_order =
            make_test_order("ORD-001", "strat-1", instrument, venue, true);
        cache.update_order(filled_order);
        assert!(!cache.index_orders_open.contains(&ClientOrderId::new("ORD-001")));
        assert!(cache
            .index_orders_closed
            .contains(&ClientOrderId::new("ORD-001")));
    }

    #[test]
    fn test_get_orders_for_strategy() {
        let mut cache = Cache::new(1000, 1000);
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");
        let venue = Venue::new("BINANCE");

        cache.update_order(make_test_order(
            "ORD-001",
            "strat-1",
            instrument.clone(),
            venue.clone(),
            false,
        ));
        cache.update_order(make_test_order(
            "ORD-002",
            "strat-1",
            instrument.clone(),
            venue.clone(),
            false,
        ));
        cache.update_order(make_test_order(
            "ORD-003",
            "strat-2",
            instrument,
            venue,
            false,
        ));

        let strat1_orders = cache.get_orders_for_strategy(&StrategyId::new("strat-1"));
        assert_eq!(strat1_orders.len(), 2);

        let strat2_orders = cache.get_orders_for_strategy(&StrategyId::new("strat-2"));
        assert_eq!(strat2_orders.len(), 1);
    }

    #[test]
    fn test_get_orders_for_instrument() {
        let mut cache = Cache::new(1000, 1000);
        let venue = Venue::new("BINANCE");

        cache.update_order(make_test_order(
            "ORD-001",
            "strat-1",
            InstrumentId::new("BTC-USDT", "BINANCE"),
            venue.clone(),
            false,
        ));
        cache.update_order(make_test_order(
            "ORD-002",
            "strat-1",
            InstrumentId::new("ETH-USDT", "BINANCE"),
            venue,
            false,
        ));

        let btc_orders = cache.get_orders_for_instrument(&InstrumentId::new("BTC-USDT", "BINANCE"));
        assert_eq!(btc_orders.len(), 1);
    }

    #[test]
    fn test_update_position_open_to_closed() {
        let mut cache = Cache::new(1000, 1000);
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");
        let venue = Venue::new("BINANCE");

        let pos = make_test_position("POS-001", "strat-1", instrument.clone(), venue.clone(), 1.0);
        cache.update_position(pos);
        assert!(cache.index_positions_open.contains(&PositionId::new("POS-001")));

        let closed =
            make_test_position("POS-001", "strat-1", instrument, venue, 0.0);
        cache.update_position(closed);
        assert!(!cache.index_positions_open.contains(&PositionId::new("POS-001")));
        assert!(cache
            .index_positions_closed
            .contains(&PositionId::new("POS-001")));
    }

    #[test]
    fn test_cache_snapshot() {
        let cache = Cache::new(1000, 1000);
        let snap = cache.snapshot();
        assert!(snap.orders.is_empty());
        assert!(snap.positions.is_empty());
    }

    #[test]
    fn test_cache_load_from_database_reconstructs_indices() {
        use std::sync::Arc;
        use crate::database::{Database, MemoryDatabase};
        use crate::messages::StrategyId;
        use crate::engine::orders::{OrderSide as OOrderSide, OrderType as OOrderType};

        let db = Arc::new(MemoryDatabase::new());

        let instrument_a = crate::instrument::InstrumentId::new("BTC-USDT", "BINANCE");
        let instrument_b = crate::instrument::InstrumentId::new("ETH-USDT", "BINANCE");
        let venue = crate::instrument::Venue::new("BINANCE");

        // Create orders directly in DB (simulating prior run)
        let mut order1 = crate::engine::orders::Order::new(
            1,
            crate::messages::ClientOrderId::new("ORD-001"),
            StrategyId::new("strat-A"),
            instrument_a.clone(),
            venue.clone(),
            OOrderSide::Buy,
            OOrderType::Market,
            50_000.0,
            1.0,
            None,
            None,
        );
        order1.filled = false;
        db.save_order(&order1).unwrap();

        let mut order2 = crate::engine::orders::Order::new(
            2,
            crate::messages::ClientOrderId::new("ORD-002"),
            StrategyId::new("strat-A"),
            instrument_a.clone(),
            venue.clone(),
            OOrderSide::Buy,
            OOrderType::Market,
            50_000.0,
            1.0,
            None,
            None,
        );
        order2.filled = true;
        db.save_order(&order2).unwrap();

        let mut order3 = crate::engine::orders::Order::new(
            3,
            crate::messages::ClientOrderId::new("ORD-003"),
            StrategyId::new("strat-B"),
            instrument_b.clone(),
            venue.clone(),
            OOrderSide::Sell,
            OOrderType::Market,
            51_000.0,
            1.0,
            None,
            None,
        );
        order3.filled = false;
        db.save_order(&order3).unwrap();

        // Save a position
        let pos = crate::engine::account::Position::new(
            crate::messages::PositionId::new("POS-001"),
            instrument_a.clone(),
            StrategyId::new("strat-A"),
            venue,
            crate::messages::OrderSide::Buy,
            1.0,
            50_000.0,
            0,
        );
        db.save_position(&pos).unwrap();

        // Load from DB
        let cache = crate::cache::Cache::load_from_database(db).unwrap();

        // Verify raw data
        assert_eq!(cache.orders.len(), 3);
        assert_eq!(cache.positions.len(), 1);

        // Verify indices were reconstructed
        let orders_for_strat_a = cache.get_orders_for_strategy(&StrategyId::new("strat-A"));
        assert_eq!(orders_for_strat_a.len(), 2);

        let orders_for_strat_b = cache.get_orders_for_strategy(&StrategyId::new("strat-B"));
        assert_eq!(orders_for_strat_b.len(), 1);

        let open_positions = cache.get_positions_open();
        assert_eq!(open_positions.len(), 1);

        // Verify open/closed indices
        assert_eq!(cache.index_orders_open.len(), 2);
        assert_eq!(cache.index_orders_closed.len(), 1);
    }

    #[test]
    fn test_cache_snapshot_with_sqlite_database() {
        use std::sync::Arc;
        use std::fs;
        use crate::database::SqliteDatabase;
        use crate::messages::StrategyId;
        use crate::engine::orders::{OrderSide as OOrderSide, OrderType as OOrderType};

        let temp_path = format!("/tmp/test_nexus_snapshot_{}.db", std::process::id());
        let _ = fs::remove_file(&temp_path);

        // Create cache with a new SqliteDatabase at the temp path
        let mut cache = crate::cache::Cache::with_database(&temp_path).unwrap();
        let instrument = crate::instrument::InstrumentId::new("BTC-USDT", "BINANCE");
        let venue = crate::instrument::Venue::new("BINANCE");

        let order = crate::engine::orders::Order::new(
            1,
            crate::messages::ClientOrderId::new("ORD-KILL"),
            StrategyId::new("strat-X"),
            instrument.clone(),
            venue.clone(),
            OOrderSide::Buy,
            OOrderType::Market,
            50_000.0,
            1.0,
            None,
            None,
        );
        cache.update_order(order);
        let pos = crate::engine::account::Position::new(
            crate::messages::PositionId::new("POS-KILL"),
            instrument,
            StrategyId::new("strat-X"),
            venue,
            crate::messages::OrderSide::Buy,
            1.0,
            50_000.0,
            0,
        );
        cache.update_position(pos);

        // Snapshot to disk (saves current orders/positions/accounts to DB)
        cache.snapshot();

        // Simulate kill-and-restart: drop cache, reload from fresh DB pointing to same file
        drop(cache);
        let fresh_db = Arc::new(SqliteDatabase::new(&temp_path));
        fresh_db.open().unwrap();
        let reloaded = crate::cache::Cache::load_from_database(fresh_db).unwrap();

        // Verify indices were reconstructed
        let orders = reloaded.get_orders_for_strategy(&StrategyId::new("strat-X"));
        assert_eq!(orders.len(), 1);
        let open_pos = reloaded.get_positions_open();
        assert_eq!(open_pos.len(), 1);

        let _ = fs::remove_file(&temp_path);
    }

    // =============================================================================
    // US-008: Integration Tests for Cache + Account Routing
    // =============================================================================

    #[test]
    fn test_add_account_populates_venue_index() {
        let mut cache = Cache::new(1000, 1000);
        let venue = Venue::new("BINANCE");
        let account_id = AccountId::new("ACC-BIN-001");

        let mut account = Account::new(
            account_id.clone(),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::Hedge,
            10.0,
        );
        account.add_venue_account("BINANCE", account_id.clone());

        cache.add_account(account);

        assert_eq!(cache.account_for_venue(&venue), Some(&account_id));
    }

    #[test]
    fn test_hedge_oms_creates_per_venue_positions() {
        use crate::engine::account::PositionIdGenerator;
        use crate::messages::OrderSide;

        let mut cache = Cache::new(1000, 1000);
        let venue_binance = Venue::new("BINANCE");
        let venue_bybit = Venue::new("BYBIT");
        let strategy = StrategyId::new("EMA_BOT");
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");

        let mut gen = PositionIdGenerator::new();

        // Create positions for same strategy/instrument but different venues (Hedge mode)
        let pos1 = Position::new(
            gen.next_with_oms(&venue_binance, &strategy, &instrument, OmsType::Hedge),
            instrument.clone(),
            strategy.clone(),
            venue_binance.clone(),
            OrderSide::Buy,
            1.0,
            50_000.0,
            0,
        );

        let pos2 = Position::new(
            gen.next_with_oms(&venue_bybit, &strategy, &instrument, OmsType::Hedge),
            instrument,
            strategy.clone(),
            venue_bybit.clone(),
            OrderSide::Buy,
            0.5,
            51_000.0,
            0,
        );

        cache.update_position(pos1);
        cache.update_position(pos2);

        let binance_positions = cache.get_positions_for_venue(&venue_binance);
        let bybit_positions = cache.get_positions_for_venue(&venue_bybit);

        assert_eq!(binance_positions.len(), 1);
        assert_eq!(bybit_positions.len(), 1);
    }

    #[test]
    fn test_cache_get_position_by_id() {
        use crate::messages::OrderSide;

        let mut cache = Cache::new(1000, 1000);
        let venue = Venue::new("BINANCE");
        let strategy = StrategyId::new("strat-A");
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");

        let position_id = PositionId::new("POS-TEST-001");
        let position = Position::new(
            position_id.clone(),
            instrument,
            strategy,
            venue,
            OrderSide::Buy,
            1.0,
            50_000.0,
            0,
        );

        cache.update_position(position);

        assert!(cache.get_position(&position_id).is_some());
        assert_eq!(cache.get_position(&position_id).unwrap().quantity, 1.0);
    }

    #[test]
    fn test_cache_update_position_maintains_venue_index() {
        use crate::messages::OrderSide;

        let mut cache = Cache::new(1000, 1000);
        let venue = Venue::new("BINANCE");
        let strategy = StrategyId::new("strat-A");
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");
        let position_id = PositionId::new("POS-TEST-002");

        // Add open position
        let position = Position::new(
            position_id.clone(),
            instrument.clone(),
            strategy.clone(),
            venue.clone(),
            OrderSide::Buy,
            1.0,
            50_000.0,
            0,
        );
        cache.update_position(position);

        assert_eq!(cache.get_positions_for_venue(&venue).len(), 1);

        // Close position (quantity = 0)
        let closed = Position::new(
            position_id.clone(),
            instrument,
            strategy,
            venue.clone(),
            OrderSide::Buy,
            0.0,
            50_000.0,
            0,
        );
        cache.update_position(closed);

        // Venue index should still contain the position (moved to closed)
        // The get_positions_for_venue returns ALL positions for venue, not just open
        let all = cache.get_positions_for_venue(&venue);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].quantity, 0.0);
    }
}
