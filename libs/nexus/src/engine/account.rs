//! Account model for live trading — multi-venue, multi-currency.
//!
//! Replaces the simple MarginAccount from Phase 4.6 with a full-featured
//! Account that tracks per-venue balances, positions, and orders.
//!
//! Nautilus Source: `accounting/accounts/base.pyx`, `accounting/accounts/margin.pyx`,
//! `accounting/accounts/cash.pyx`

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::messages::{PositionId, StrategyId, OrderSide};
use crate::instrument::{InstrumentId, Venue};

/// Order Management System type — determines how orders map to positions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OmsType {
    /// Single order → single position (no hedging).
    /// Position is shared across all orders for the same instrument.
    SingleOrder,
    /// One position per venue (hedged mode).
    /// Same instrument on different venues = separate positions.
    Hedge,
    /// Netting mode — positions net across venues.
    /// BUY and SELL orders net to a single net position.
    Netting,
}

/// Account identifier — venue-specific account.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AccountId(pub String);

impl AccountId {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for AccountId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for AccountId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// A currency identifier (e.g., "USDT", "BTC", "USD").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Currency(pub String);

impl Currency {
    pub fn new(code: &str) -> Self {
        Self(code.to_uppercase())
    }
}

impl std::fmt::Display for Currency {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A position — the aggregated state of a single instrument for a strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Position {
    pub position_id: PositionId,
    pub instrument_id: InstrumentId,
    pub strategy_id: StrategyId,
    pub venue: Venue,
    pub side: OrderSide,
    pub quantity: f64,
    pub avg_fill_price: f64,
    pub realized_pnl: f64,
    pub unrealized_pnl: f64,
    pub ts_opened: u64,
}

impl Position {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        position_id: PositionId,
        instrument_id: InstrumentId,
        strategy_id: StrategyId,
        venue: Venue,
        side: OrderSide,
        quantity: f64,
        avg_fill_price: f64,
        ts_opened: u64,
    ) -> Self {
        Self {
            position_id,
            instrument_id,
            strategy_id,
            venue,
            side,
            quantity,
            avg_fill_price,
            realized_pnl: 0.0,
            unrealized_pnl: 0.0,
            ts_opened,
        }
    }

    pub fn is_open(&self) -> bool {
        self.quantity != 0.0
    }

    pub fn is_closed(&self) -> bool {
        self.quantity == 0.0
    }
}

/// Balance for a single currency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Balance {
    /// Total balance (available + locked).
    pub total: f64,
    /// Balance locked by open orders.
    pub locked: f64,
    /// Balance available for new orders.
    pub available: f64,
}

impl Balance {
    pub fn new(total: f64) -> Self {
        Self {
            total,
            locked: 0.0,
            available: total,
        }
    }

    pub fn with_locked(total: f64, locked: f64) -> Self {
        Self {
            total,
            locked,
            available: (total - locked).max(0.0),
        }
    }

    /// Lock funds for an order. Returns true if sufficient available balance.
    pub fn lock(&mut self, amount: f64) -> bool {
        if self.available >= amount {
            self.locked += amount;
            self.available -= amount;
            true
        } else {
            false
        }
    }

    /// Unlock funds when order is filled or cancelled.
    pub fn unlock(&mut self, amount: f64) {
        self.locked = (self.locked - amount).max(0.0);
        self.available = (self.total - self.locked).max(0.0);
    }

    /// Add to total (deposits, profits).
    pub fn credit(&mut self, amount: f64) {
        self.total += amount;
        self.available += amount;
    }

    /// Subtract from total (withdrawals, losses).
    pub fn debit(&mut self, amount: f64) {
        self.total = (self.total - amount).max(0.0);
        self.available = (self.total - self.locked).max(0.0);
    }
}

/// Account event for state changes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AccountEvent {
    /// Initial state event.
    Created {
        account_id: AccountId,
        base_currency: Option<Currency>,
        ts_init: u64,
    },
    /// Balance updated for a currency.
    BalanceUpdated {
        currency: Currency,
        balance: Balance,
        ts_event: u64,
    },
    /// Margin calculation updated.
    MarginUpdated {
        margin_used: f64,
        margin_available: f64,
        ts_event: u64,
    },
    /// Order fill applied to account.
    OrderApplied {
        client_order_id: String,
        instrument_id: String,
        side: crate::messages::OrderSide,
        filled_qty: f64,
        fill_price: f64,
        commission: f64,
        ts_event: u64,
    },
    /// Position opened.
    PositionOpened {
        position_id: String,
        instrument_id: String,
        side: crate::messages::OrderSide,
        quantity: f64,
        avg_fill_price: f64,
        ts_event: u64,
    },
    /// Position closed.
    PositionClosed {
        position_id: String,
        instrument_id: String,
        realized_pnl: f64,
        commission: f64,
        ts_event: u64,
    },
    /// Funding payment applied (perpetuals).
    FundingPaid {
        currency: Currency,
        amount: f64,
        ts_event: u64,
    },
    /// Liquidation occurred.
    Liquidation {
        position_id: String,
        reason: String,
        ts_event: u64,
    },
}

/// Account state — the authoritative state for a trading account.
///
/// Tracks:
/// - Per-currency balances (multi-currency support)
/// - Per-venue account mapping
/// - Margin state
/// - Commission accumulated
/// - Events for replay
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    /// The account identifier.
    pub id: AccountId,
    /// Base currency (None for multi-currency accounts).
    pub base_currency: Option<Currency>,
    /// Per-currency balances.
    pub balances: HashMap<Currency, Balance>,
    /// Per-venue account ID mapping.
    pub venue_accounts: HashMap<String, AccountId>,
    /// Total commission accumulated per currency.
    pub commissions: HashMap<Currency, f64>,
    /// Margin used across all positions.
    pub margin_used: f64,
    /// Margin available for new positions.
    pub margin_available: f64,
    /// Default leverage for margin accounts.
    pub default_leverage: f64,
    /// Per-instrument leverage overrides.
    pub leverages: HashMap<u32, f64>, // instrument_id → leverage
    /// OMS type (SingleOrder, Hedge, Netting).
    pub oms_type: OmsType,
    /// Event log for state reconstruction.
    events: Vec<AccountEvent>,
}

impl Account {
    /// Create a new account with initial balances.
    pub fn new(
        account_id: AccountId,
        base_currency: Option<Currency>,
        initial_balances: HashMap<Currency, f64>,
        oms_type: OmsType,
        default_leverage: f64,
    ) -> Self {
        let mut balances = HashMap::new();
        for (currency, total) in initial_balances {
            balances.insert(currency.clone(), Balance::new(total));
        }

        let mut s = Self {
            id: account_id,
            base_currency,
            balances,
            venue_accounts: HashMap::new(),
            commissions: HashMap::new(),
            margin_used: 0.0,
            margin_available: 0.0,
            default_leverage,
            leverages: HashMap::new(),
            oms_type,
            events: Vec::new(),
        };

        s.events.push(AccountEvent::Created {
            account_id: s.id.clone(),
            base_currency: s.base_currency.clone(),
            ts_init: 0, // Will be set on first use
        });

        s
    }

    /// Add a venue account mapping.
    pub fn add_venue_account(&mut self, venue: &str, account_id: AccountId) {
        self.venue_accounts.insert(venue.to_string(), account_id);
    }

    /// Get balance for a currency.
    pub fn balance(&self, currency: &Currency) -> Option<&Balance> {
        self.balances.get(currency)
    }

    /// Get total balance for a currency.
    pub fn balance_total(&self, currency: &Currency) -> f64 {
        self.balances.get(currency).map(|b| b.total).unwrap_or(0.0)
    }

    /// Get available balance for a currency.
    pub fn balance_free(&self, currency: &Currency) -> f64 {
        self.balances.get(currency).map(|b| b.available).unwrap_or(0.0)
    }

    /// Get locked balance for a currency.
    pub fn balance_locked(&self, currency: &Currency) -> f64 {
        self.balances.get(currency).map(|b| b.locked).unwrap_or(0.0)
    }

    /// Get commission accumulated for a currency.
    pub fn commission(&self, currency: &Currency) -> f64 {
        self.commissions.get(currency).copied().unwrap_or(0.0)
    }

    /// Lock balance for an order (returns true if successful).
    pub fn lock_balance(&mut self, currency: &Currency, amount: f64) -> bool {
        if let Some(balance) = self.balances.get_mut(currency) {
            let success = balance.lock(amount);
            if success {
                self.events.push(AccountEvent::BalanceUpdated {
                    currency: currency.clone(),
                    balance: balance.clone(),
                    ts_event: 0,
                });
            }
            success
        } else {
            false
        }
    }

    /// Unlock balance when order is filled or cancelled.
    pub fn unlock_balance(&mut self, currency: &Currency, amount: f64) {
        if let Some(balance) = self.balances.get_mut(currency) {
            balance.unlock(amount);
            self.events.push(AccountEvent::BalanceUpdated {
                currency: currency.clone(),
                balance: balance.clone(),
                ts_event: 0,
            });
        }
    }

    /// Apply a credit (deposit, profit).
    pub fn credit(&mut self, currency: &Currency, amount: f64) {
        if let Some(balance) = self.balances.get_mut(currency) {
            balance.credit(amount);
            self.events.push(AccountEvent::BalanceUpdated {
                currency: currency.clone(),
                balance: balance.clone(),
                ts_event: 0,
            });
        }
    }

    /// Apply a debit (withdrawal, loss).
    pub fn debit(&mut self, currency: &Currency, amount: f64) {
        if let Some(balance) = self.balances.get_mut(currency) {
            balance.debit(amount);
            self.events.push(AccountEvent::BalanceUpdated {
                currency: currency.clone(),
                balance: balance.clone(),
                ts_event: 0,
            });
        }
    }

    /// Apply a commission charge.
    pub fn apply_commission(&mut self, currency: &Currency, amount: f64) {
        *self.commissions.entry(currency.clone()).or_insert(0.0) += amount;
        self.debit(currency, amount);
    }

    /// Apply an order fill to the account.
    ///
    /// - Buy: debit (deduct) quote currency = filled_qty * fill_price + commission
    /// - Sell: credit (add) quote currency = filled_qty * fill_price - commission
    #[allow(clippy::too_many_arguments)]
    pub fn update_with_order(
        &mut self,
        client_order_id: String,
        instrument_id: String,
        side: crate::messages::OrderSide,
        filled_qty: f64,
        fill_price: f64,
        commission: f64,
        ts_event: u64,
    ) {
        let currency = self.base_currency.clone().unwrap_or_else(|| {
            self.balances.keys().next().cloned().unwrap_or(Currency::new("USDT"))
        });

        let cost = filled_qty * fill_price;
        if matches!(side, crate::messages::OrderSide::Buy) {
            self.debit(&currency, cost + commission);
        } else {
            self.credit(&currency, cost - commission);
        }

        *self.commissions.entry(currency.clone()).or_insert(0.0) += commission;

        self.events.push(AccountEvent::OrderApplied {
            client_order_id,
            instrument_id,
            side,
            filled_qty,
            fill_price,
            commission,
            ts_event,
        });
    }

    /// Get the leverage for an instrument.
    pub fn leverage(&self, instrument_id: u32) -> f64 {
        self.leverages.get(&instrument_id).copied().unwrap_or(self.default_leverage)
    }

    /// Set leverage for a specific instrument.
    pub fn set_leverage(&mut self, instrument_id: u32, leverage: f64) {
        self.leverages.insert(instrument_id, leverage);
    }

    /// Calculate initial margin required for a position.
    pub fn calc_margin_init(&self, notional: f64) -> f64 {
        notional / self.default_leverage
    }

    /// Calculate margin required for an order size at a price.
    pub fn margin_required(&self, price: f64, size: f64) -> f64 {
        let notional = price * size;
        notional / self.default_leverage
    }

    /// Check if margin is available for a new position.
    pub fn has_margin_for(&self, price: f64, size: f64) -> bool {
        let required = self.margin_required(price, size);
        self.margin_available >= required
    }

    /// Update margin state.
    pub fn update_margin(&mut self, margin_used: f64, margin_available: f64) {
        self.margin_used = margin_used;
        self.margin_available = margin_available;
    }

    /// Get equity (total balance across all currencies).
    pub fn equity(&self) -> f64 {
        self.balances.values().map(|b| b.total).sum()
    }

    /// Get margin ratio.
    pub fn margin_ratio(&self) -> f64 {
        if self.equity() > 0.0 {
            self.margin_used / self.equity()
        } else {
            f64::INFINITY
        }
    }

    /// Check if in margin call.
    pub fn in_margin_call(&self, threshold: f64) -> bool {
        self.margin_ratio() > threshold
    }

    /// Get all currencies tracked.
    pub fn currencies(&self) -> Vec<Currency> {
        self.balances.keys().cloned().collect()
    }

    /// Get all events (for state reconstruction).
    pub fn events(&self) -> &[AccountEvent] {
        &self.events
    }

    /// Apply an external balance update (from exchange).
    pub fn apply_balance_update(&mut self, currency: Currency, total: f64, locked: f64) {
        let balance = Balance::with_locked(total, locked);
        self.balances.insert(currency.clone(), balance.clone());
        self.events.push(AccountEvent::BalanceUpdated {
            currency,
            balance,
            ts_event: 0,
        });
    }

    /// Apply a position open event.
    pub fn apply_position_open(
        &mut self,
        position_id: String,
        instrument_id: String,
        side: crate::messages::OrderSide,
        quantity: f64,
        avg_fill_price: f64,
        ts_event: u64,
    ) {
        // Reserve margin for the position
        let notional = avg_fill_price * quantity;
        let margin = self.calc_margin_init(notional);
        self.margin_used += margin;
        self.margin_available = (self.equity() - self.margin_used).max(0.0);

        self.events.push(AccountEvent::PositionOpened {
            position_id,
            instrument_id,
            side,
            quantity,
            avg_fill_price,
            ts_event,
        });
    }

    /// Apply a position close event.
    pub fn apply_position_close(
        &mut self,
        position_id: String,
        instrument_id: String,
        realized_pnl: f64,
        commission: f64,
        ts_event: u64,
    ) {
        // Release margin and credit PnL
        // For simplicity, we release a proportional margin (actual implementation
        // would track per-position margin separately)
        if self.margin_used > 0.0 {
            self.margin_used = (self.margin_used * 0.9).max(0.0); // simplified
        }
        self.margin_available = (self.equity() - self.margin_used).max(0.0);

        // Credit realized PnL
        if realized_pnl != 0.0 {
            // Assuming quote currency - in real implementation would map to correct currency
            for balance in self.balances.values_mut() {
                balance.total += realized_pnl;
                balance.available += realized_pnl;
            }
        }

        // Deduct commission
        if commission > 0.0 {
            for balance in self.balances.values_mut() {
                balance.total -= commission;
                balance.available -= commission;
            }
        }

        self.events.push(AccountEvent::PositionClosed {
            position_id,
            instrument_id,
            realized_pnl,
            commission,
            ts_event,
        });
    }
}

/// Generates unique PositionIds based on venue, strategy, instrument, and a serial counter.
/// The counter is per-during (stored in the generator) so IDs are stable across restarts
/// when the same inputs produce the same outputs.
pub struct PositionIdGenerator {
    counters: HashMap<(String, String, String), u64>, // (venue, strategy, instrument) → counter
}

impl PositionIdGenerator {
    pub fn new() -> Self {
        Self { counters: HashMap::new() }
    }

    /// Generate the next PositionId for a (venue, strategy_id, instrument_id) triple.
    /// Format: "POS-{venue}-{strategy}-{instrument_id:08X}-{counter}"
    pub fn next(&mut self, venue: &Venue, strategy_id: &StrategyId, instrument_id: &InstrumentId) -> PositionId {
        let key = (venue.code.clone(), strategy_id.0.clone(), format!("{:08X}", instrument_id.id));
        let counter = self.counters.entry(key).or_insert(0);
        let id = PositionId::new(&format!("POS-{}-{}-{:08X}-{}", venue.code, strategy_id.0, instrument_id.id, counter));
        *counter += 1;
        id
    }

    /// Generate the next PositionId using OmsType-aware key.
    pub fn next_with_oms(
        &mut self,
        venue: &Venue,
        strategy_id: &StrategyId,
        instrument_id: &InstrumentId,
        oms_type: OmsType,
    ) -> PositionId {
        match oms_type {
            OmsType::SingleOrder => {
                // Key is (strategy, instrument) only — no venue component
                let key = (String::new(), strategy_id.0.clone(), format!("{:08X}", instrument_id.id));
                let counter = self.counters.entry(key).or_insert(0);
                let id = PositionId::new(&format!("POS-{}-{:08X}-{}", strategy_id.0, instrument_id.id, counter));
                *counter += 1;
                id
            }
            OmsType::Hedge => {
                // Key includes venue — one position per venue per instrument
                let key = (venue.code.clone(), strategy_id.0.clone(), format!("{:08X}", instrument_id.id));
                let counter = self.counters.entry(key).or_insert(0);
                let id = PositionId::new(&format!("POS-{}-{}-{:08X}-{}", venue.code, strategy_id.0, instrument_id.id, counter));
                *counter += 1;
                id
            }
            OmsType::Netting => {
                // Key is (strategy, instrument) only — nets across all venues
                let key = (String::new(), strategy_id.0.clone(), format!("{:08X}", instrument_id.id));
                let counter = self.counters.entry(key).or_insert(0);
                let id = PositionId::new(&format!("POS-NET-{}-{:08X}-{}", strategy_id.0, instrument_id.id, counter));
                *counter += 1;
                id
            }
        }
    }
}

impl Default for PositionIdGenerator {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for PositionIdGenerator {
    fn clone(&self) -> Self {
        Self { counters: self.counters.clone() }
    }
}

#[cfg(test)]
mod position_id_generator_tests {
    use super::*;

    #[test]
    fn test_position_id_generator_hedge_different_venues() {
        let mut gen = PositionIdGenerator::new();
        let venue_binance = Venue::new("BINANCE");
        let venue_bybit = Venue::new("BYBIT");
        let strategy = StrategyId::new("EMA_BOT");
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");

        let id1 = gen.next_with_oms(&venue_binance, &strategy, &instrument, OmsType::Hedge);
        let id2 = gen.next_with_oms(&venue_bybit, &strategy, &instrument, OmsType::Hedge);

        assert_ne!(id1, id2);
        assert!(id1.to_string().contains("BINANCE"));
        assert!(id2.to_string().contains("BYBIT"));
    }

    #[test]
    fn test_position_id_generator_single_order_different_strategies() {
        let mut gen = PositionIdGenerator::new();
        let venue = Venue::new("BINANCE");
        let strat_a = StrategyId::new("strat-A");
        let strat_b = StrategyId::new("strat-B");
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");

        let id1 = gen.next_with_oms(&venue, &strat_a, &instrument, OmsType::SingleOrder);
        let id2 = gen.next_with_oms(&venue, &strat_b, &instrument, OmsType::SingleOrder);

        assert_ne!(id1, id2);
    }

    #[test]
    fn test_position_id_generator_netting_same_instrument() {
        let mut gen = PositionIdGenerator::new();
        let venue = Venue::new("BINANCE");
        let strategy = StrategyId::new("strat-A");
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");

        let id1 = gen.next_with_oms(&venue, &strategy, &instrument, OmsType::Netting);
        let id2 = gen.next_with_oms(&venue, &strategy, &instrument, OmsType::Netting);

        assert!(id1.to_string().contains("NET"));
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_position_id_generator_deterministic_same_inputs() {
        let mut gen1 = PositionIdGenerator::new();
        let mut gen2 = PositionIdGenerator::new();
        let venue = Venue::new("BINANCE");
        let strategy = StrategyId::new("strat-A");
        let instrument = InstrumentId::new("BTC-USDT", "BINANCE");

        let id1 = gen1.next_with_oms(&venue, &strategy, &instrument, OmsType::Hedge);
        let id2 = gen2.next_with_oms(&venue, &strategy, &instrument, OmsType::Hedge);

        assert_eq!(id1, id2);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_account_single_order_mode() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::SingleOrder,
            10.0, // 10x leverage
        );

        assert_eq!(account.balance_total(&Currency::new("USDT")), 10_000.0);
        assert_eq!(account.balance_free(&Currency::new("USDT")), 10_000.0);

        // Lock margin for position
        let margin = account.margin_required(50_000.0, 1.0); // 1 BTC at $50K
        assert!((margin - 5_000.0).abs() < 0.01);

        account.update_margin(5_000.0, 5_000.0);
        assert!(account.has_margin_for(50_000.0, 1.0));

        // OmsType::Hedge would allow separate positions per venue
        // OmsType::Netting would net opposite orders
    }

    #[test]
    fn test_account_hedge_mode() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::Hedge,
            10.0,
        );

        account.add_venue_account("BINANCE", AccountId::new("ACC-BINANCE-001"));
        account.add_venue_account("BYBIT", AccountId::new("ACC-BYBIT-001"));

        // In hedge mode, same instrument on different venues = separate positions
        assert_eq!(account.venue_accounts.len(), 2);
    }

    #[test]
    fn test_account_lock_unlock() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            None,
            [(Currency::new("USDT"), 10_000.0), (Currency::new("BTC"), 0.0)].into(),
            OmsType::SingleOrder,
            10.0,
        );

        let usdt = Currency::new("USDT");

        // Lock for order
        assert!(account.lock_balance(&usdt, 1_000.0));
        assert_eq!(account.balance_free(&usdt), 9_000.0);
        assert_eq!(account.balance_locked(&usdt), 1_000.0);

        // Unlock on fill
        account.unlock_balance(&usdt, 1_000.0);
        assert_eq!(account.balance_free(&usdt), 10_000.0);
        assert_eq!(account.balance_locked(&usdt), 0.0);
    }

    #[test]
    fn test_account_margin_call() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::SingleOrder,
            10.0,
        );

        // Use 90% margin (margin_ratio = 0.9)
        account.update_margin(9_000.0, 1_000.0);
        assert!(!account.in_margin_call(0.9)); // just at threshold

        // Use 95% margin
        account.update_margin(9_500.0, 500.0);
        assert!(account.in_margin_call(0.9)); // over threshold
    }

    #[test]
    fn test_account_credit_debit() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::SingleOrder,
            10.0,
        );

        let usdt = Currency::new("USDT");

        account.credit(&usdt, 5_000.0);
        assert_eq!(account.balance_total(&usdt), 15_000.0);

        account.debit(&usdt, 2_000.0);
        assert_eq!(account.balance_total(&usdt), 13_000.0);
    }

    #[test]
    fn test_account_leverage_per_instrument() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::SingleOrder,
            10.0, // default 10x
        );

        // BTC has 20x leverage
        let btc_instrument_id = 0x42435455; // "BTCUSDT" hash for example
        account.set_leverage(btc_instrument_id, 20.0);

        assert_eq!(account.leverage(btc_instrument_id), 20.0);
        assert_eq!(account.leverage(999), 10.0); // unknown → default
    }

    #[test]
    fn test_account_hedge_per_venue_mapping() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::Hedge,
            10.0,
        );
        account.add_venue_account("BINANCE", AccountId::new("ACC-BINANCE-001"));
        account.add_venue_account("BYBIT", AccountId::new("ACC-BYBIT-001"));
        account.add_venue_account("OKX", AccountId::new("ACC-OKX-001"));
        assert_eq!(account.venue_accounts.len(), 3);
        assert_eq!(account.oms_type, OmsType::Hedge);
    }

    #[test]
    fn test_apply_position_open_reserves_margin() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::SingleOrder,
            10.0,
        );
        account.apply_position_open(
            "POS-001".to_string(),
            "BTC-USDT".to_string(),
            crate::messages::OrderSide::Buy,
            1.0,
            50_000.0,
            1_000_000_000,
        );
        assert!((account.margin_used - 5_000.0).abs() < 0.01);
        let events = account.events();
        assert!(matches!(events.last(), Some(AccountEvent::PositionOpened { side: crate::messages::OrderSide::Buy, .. })));
    }

    #[test]
    fn test_apply_position_close_releases_margin() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::SingleOrder,
            10.0,
        );
        account.apply_position_open(
            "POS-001".to_string(),
            "BTC-USDT".to_string(),
            crate::messages::OrderSide::Buy,
            1.0,
            50_000.0,
            1_000_000_000,
        );
        let margin_before = account.margin_used;
        account.apply_position_close(
            "POS-001".to_string(),
            "BTC-USDT".to_string(),
            1_000.0,
            10.0,
            2_000_000_000,
        );
        assert!(account.margin_used < margin_before);
        let events = account.events();
        assert!(matches!(events.last(), Some(AccountEvent::PositionClosed { .. })));
    }

    #[test]
    fn test_update_with_order_buy_debits_quote_currency() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::SingleOrder,
            10.0,
        );
        account.update_with_order(
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            crate::messages::OrderSide::Buy,
            0.1,
            50_000.0,
            5.0,
            1_000_000_000,
        );
        assert_eq!(account.balance_total(&Currency::new("USDT")), 10_000.0 - 5_005.0);
        assert_eq!(account.commission(&Currency::new("USDT")), 5.0);
        let events = account.events();
        assert!(matches!(events.last(), Some(AccountEvent::OrderApplied { side: crate::messages::OrderSide::Buy, .. })));
    }

    #[test]
    fn test_update_with_order_sell_credits_quote_currency() {
        let mut account = Account::new(
            AccountId::new("ACC-001"),
            Some(Currency::new("USDT")),
            [(Currency::new("USDT"), 10_000.0)].into(),
            OmsType::SingleOrder,
            10.0,
        );
        account.update_with_order(
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            crate::messages::OrderSide::Sell,
            0.1,
            50_000.0,
            5.0,
            1_000_000_000,
        );
        assert_eq!(account.balance_total(&Currency::new("USDT")), 10_000.0 + 4_995.0);
        assert_eq!(account.commission(&Currency::new("USDT")), 5.0);
    }
}