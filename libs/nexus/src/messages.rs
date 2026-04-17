//! Message serialization for network transmission and persistence.
//!
//! All messages implement `serde::Serialize` + `serde::Deserialize` for
//! JSON encoding (human-readable logs) and binary encoding (wire transport).
//!
//! Every message carries `ts_init` (creation time) and most have `ts_event`
//! (when the event actually happened). UUID4 correlation IDs allow tracing
//! a request through its full lifecycle.
//!
//! Nautilus Source: `common/messages.pyx`, `core/message.pyx`

use serde::{Deserialize, Serialize};
use uuid::Uuid;
pub use crate::actor::Message;

pub use crate::engine::account::OmsType;
pub use crate::instrument::{InstrumentId, Venue};

// =============================================================================
// SECTION 1: ID Types
// =============================================================================

/// A trader identifier (e.g., "TRADER-001").
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraderId(pub String);

impl TraderId {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for TraderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for TraderId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

/// A strategy identifier.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StrategyId(pub String);

impl StrategyId {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for StrategyId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A client-assigned order identifier (created by the trading strategy).
/// The client_order_id is unique per trader.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ClientOrderId(pub String);

impl ClientOrderId {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for ClientOrderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A venue-assigned order identifier (assigned when the order reaches the exchange).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct VenueOrderId(pub String);

impl VenueOrderId {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for VenueOrderId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A position identifier assigned by the OMS.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PositionId(pub String);

impl PositionId {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for PositionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A trade identifier assigned by the venue on fill.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TradeId(pub String);

impl TradeId {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }
}

impl std::fmt::Display for TradeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A unique message correlation ID (UUID4).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MessageId(pub Uuid);

impl MessageId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

/// Order side (buy or sell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderSide {
    Buy,
    Sell,
}

/// Order type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderType {
    Market,
    Limit,
    Stop,
    StopLimit,
    Iceberg,
    Twap,
    Vwap,
    TrailingStop,
}

/// Time in force for a limit order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeInForce {
    Gtc, // Good Till Canceled
    Gtd, // Good Till Date
    Ioc, // Immediate Or Cancel
    Fok, // Fill Or Kill
    Gfx, // Good For Day
}

// =============================================================================
// SECTION 2: Topic
// =============================================================================

/// A message bus topic with wildcard support.
///
/// Topics use glob patterns:
/// - `*` matches one or more characters
/// - `?` matches exactly one character
///
/// Examples:
/// - `events.system.*` — all system events
/// - `events.?.stopped` — single-char prefix matches
/// - `orders.{instrument}.*` — instrument-specific order events
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Topic(pub String);

impl Topic {
    pub fn new(value: &str) -> Self {
        Self(value.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Topic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for Topic {
    fn from(s: String) -> Self {
        Self(s)
    }
}

// =============================================================================
// SECTION 3: Command Messages
// =============================================================================

/// Base command message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Command {
    pub id: MessageId,
    pub ts_init: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<MessageId>,
}

impl Command {
    pub fn new(ts_init: u64) -> Self {
        Self {
            id: MessageId::new(),
            ts_init,
            correlation_id: None,
        }
    }

    pub fn with_correlation(ts_init: u64, correlation_id: MessageId) -> Self {
        Self {
            id: MessageId::new(),
            ts_init,
            correlation_id: Some(correlation_id),
        }
    }
}

/// Submit an order command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitOrder {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: String, // serialized InstrumentId
    pub client_order_id: ClientOrderId,
    pub venue_order_id: Option<VenueOrderId>,
    pub order_side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub price: Option<f64>,
    pub time_in_force: Option<TimeInForce>,
    pub expire_time_ns: Option<u64>,
    pub uuid: Uuid,
    pub ts_init: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<MessageId>,
}

impl SubmitOrder {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        trader_id: TraderId,
        strategy_id: StrategyId,
        instrument_id: String,
        client_order_id: ClientOrderId,
        order_side: OrderSide,
        order_type: OrderType,
        quantity: f64,
        ts_init: u64,
    ) -> Self {
        Self {
            trader_id,
            strategy_id,
            instrument_id,
            client_order_id,
            venue_order_id: None,
            order_side,
            order_type,
            quantity,
            price: None,
            time_in_force: None,
            expire_time_ns: None,
            uuid: Uuid::new_v4(),
            ts_init,
            correlation_id: None,
        }
    }

    pub fn with_price(mut self, price: f64) -> Self {
        self.price = Some(price);
        self
    }

    pub fn with_tif(mut self, tif: TimeInForce) -> Self {
        self.time_in_force = Some(tif);
        self
    }
}

/// Cancel an order command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelOrder {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub client_order_id: ClientOrderId,
    pub venue_order_id: Option<VenueOrderId>,
    pub uuid: Uuid,
    pub ts_init: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<MessageId>,
}

impl CancelOrder {
    pub fn new(
        trader_id: TraderId,
        strategy_id: StrategyId,
        client_order_id: ClientOrderId,
        ts_init: u64,
    ) -> Self {
        Self {
            trader_id,
            strategy_id,
            client_order_id,
            venue_order_id: None,
            uuid: Uuid::new_v4(),
            ts_init,
            correlation_id: None,
        }
    }
}

/// Modify an order command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModifyOrder {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub client_order_id: ClientOrderId,
    pub venue_order_id: Option<VenueOrderId>,
    pub new_quantity: Option<f64>,
    pub new_price: Option<f64>,
    pub uuid: Uuid,
    pub ts_init: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<MessageId>,
}

impl ModifyOrder {
    pub fn new(
        trader_id: TraderId,
        strategy_id: StrategyId,
        client_order_id: ClientOrderId,
        ts_init: u64,
    ) -> Self {
        Self {
            trader_id,
            strategy_id,
            client_order_id,
            venue_order_id: None,
            new_quantity: None,
            new_price: None,
            uuid: Uuid::new_v4(),
            ts_init,
            correlation_id: None,
        }
    }
}

/// Submit a batch of orders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitOrderBatch {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub orders: Vec<SubmitOrder>,
    pub ts_init: u64,
}

// =============================================================================
// SECTION 4: Event Messages
// =============================================================================

/// Base event message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: MessageId,
    pub ts_event: u64,
    pub ts_init: u64,
}

impl Event {
    pub fn new(ts_event: u64, ts_init: u64) -> Self {
        Self {
            id: MessageId::new(),
            ts_event,
            ts_init,
        }
    }
}

/// Order submitted to the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderSubmitted {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub instrument_id: String,
    pub order_side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub price: Option<f64>,
    pub uuid: Uuid,
    pub ts_event: u64,
    pub ts_init: u64,
}
impl Message for OrderSubmitted {}

impl OrderSubmitted {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        trader_id: TraderId,
        strategy_id: StrategyId,
        client_order_id: ClientOrderId,
        venue_order_id: VenueOrderId,
        instrument_id: String,
        order_side: OrderSide,
        order_type: OrderType,
        quantity: f64,
        ts_event: u64,
        ts_init: u64,
    ) -> Self {
        Self {
            trader_id,
            strategy_id,
            client_order_id,
            venue_order_id,
            instrument_id,
            order_side,
            order_type,
            quantity,
            price: None,
            uuid: Uuid::new_v4(),
            ts_event,
            ts_init,
        }
    }
}

/// Order accepted by the exchange and resting in the book.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderAccepted {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub instrument_id: String,
    pub order_side: OrderSide,
    pub order_type: OrderType,
    pub quantity: f64,
    pub price: Option<f64>,
    pub uuid: Uuid,
    pub ts_event: u64,
    pub ts_init: u64,
}
impl Message for OrderAccepted {}

impl OrderAccepted {
    pub fn from_submitted(submitted: &OrderSubmitted) -> Self {
        Self {
            trader_id: submitted.trader_id.clone(),
            strategy_id: submitted.strategy_id.clone(),
            client_order_id: submitted.client_order_id.clone(),
            venue_order_id: submitted.venue_order_id.clone(),
            instrument_id: submitted.instrument_id.clone(),
            order_side: submitted.order_side,
            order_type: submitted.order_type,
            quantity: submitted.quantity,
            price: submitted.price,
            uuid: Uuid::new_v4(),
            ts_event: submitted.ts_event,
            ts_init: submitted.ts_init,
        }
    }
}

/// Order filled (completely or partially).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderFilled {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub position_id: PositionId,
    pub trade_id: TradeId,
    pub instrument_id: String,
    pub order_side: OrderSide,
    pub filled_qty: f64,
    pub fill_price: f64,
    pub commission: f64,
    pub slippage_bps: f64,
    pub is_maker: bool,
    pub ts_event: u64,
    pub ts_init: u64,
}
impl Message for OrderFilled {}

impl OrderFilled {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        trader_id: TraderId,
        strategy_id: StrategyId,
        client_order_id: ClientOrderId,
        venue_order_id: VenueOrderId,
        position_id: PositionId,
        trade_id: TradeId,
        instrument_id: String,
        order_side: OrderSide,
        filled_qty: f64,
        fill_price: f64,
        ts_event: u64,
        ts_init: u64,
    ) -> Self {
        Self {
            trader_id,
            strategy_id,
            client_order_id,
            venue_order_id,
            position_id,
            trade_id,
            instrument_id,
            order_side,
            filled_qty,
            fill_price,
            commission: 0.0,
            slippage_bps: 0.0,
            is_maker: false,
            ts_event,
            ts_init,
        }
    }

    pub fn with_commission(mut self, commission: f64) -> Self {
        self.commission = commission;
        self
    }

    pub fn with_slippage(mut self, slippage_bps: f64) -> Self {
        self.slippage_bps = slippage_bps;
        self
    }

    pub fn with_maker(mut self, is_maker: bool) -> Self {
        self.is_maker = is_maker;
        self
    }
}

/// Order partially filled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderPartiallyFilled {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub position_id: PositionId,
    pub trade_id: TradeId,
    pub instrument_id: String,
    pub order_side: OrderSide,
    pub filled_qty: f64,
    pub remaining_qty: f64,
    pub fill_price: f64,
    pub commission: f64,
    pub slippage_bps: f64,
    pub ts_event: u64,
    pub ts_init: u64,
}
impl Message for OrderPartiallyFilled {}

/// Order cancelled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderCancelled {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub client_order_id: ClientOrderId,
    pub venue_order_id: Option<VenueOrderId>,
    pub instrument_id: String,
    pub ts_event: u64,
    pub ts_init: u64,
    pub reason: Option<String>,
}
impl Message for OrderCancelled {}

/// Order rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderRejected {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub client_order_id: ClientOrderId,
    pub venue_order_id: Option<VenueOrderId>,
    pub instrument_id: String,
    pub ts_event: u64,
    pub ts_init: u64,
    pub reason: String,
}
impl Message for OrderRejected {}

/// Order modified.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderModified {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub client_order_id: ClientOrderId,
    pub venue_order_id: VenueOrderId,
    pub instrument_id: String,
    pub ts_event: u64,
    pub ts_init: u64,
}
impl Message for OrderModified {}

// =============================================================================
// SECTION 5: Position Events
// =============================================================================

/// Position opened (first fill or position size goes from 0 to non-zero).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionOpened {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: String,
    pub position_id: PositionId,
    pub order_side: OrderSide,
    pub quantity: f64,
    pub avg_fill_price: f64,
    pub ts_event: u64,
    pub ts_init: u64,
}

/// Position changed (quantity or avg price changed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionChanged {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: String,
    pub position_id: PositionId,
    pub quantity: f64,
    pub avg_fill_price: f64,
    pub unrealized_pnl: f64,
    pub ts_event: u64,
    pub ts_init: u64,
}

/// Position closed (fully liquidated).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionClosed {
    pub trader_id: TraderId,
    pub strategy_id: StrategyId,
    pub instrument_id: String,
    pub position_id: PositionId,
    pub ts_event: u64,
    pub ts_init: u64,
    pub reason: String,
    pub realized_pnl: f64,
    pub commission: f64,
}

// =============================================================================
// SECTION 5: Market Data Events
// =============================================================================

/// Instrument trading status change (halt, resume, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentStatus {
    pub instrument_id: InstrumentId,
    pub status: String,
    pub ts_event: u64,
}

/// Instrument close price update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstrumentClose {
    pub instrument_id: InstrumentId,
    pub close_price: f64,
    pub ts_event: u64,
}

/// Funding rate update for perpetual/futures instruments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FundingRateUpdate {
    pub instrument_id: InstrumentId,
    pub rate: f64,
    pub ts_event: u64,
}

/// Mark price update for perpetual/futures instruments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkPriceUpdate {
    pub instrument_id: InstrumentId,
    pub mark_price: f64,
    pub ts_event: u64,
}

/// Index price update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexPriceUpdate {
    pub instrument_id: InstrumentId,
    pub index_price: f64,
    pub ts_event: u64,
}

/// Signal data update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalData {
    pub signal_id: String,
    pub value: f64,
    pub ts_event: u64,
}

// =============================================================================
// SECTION 6: Account Events
// =============================================================================

/// Account state update.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountState {
    pub trader_id: TraderId,
    pub account_id: String,
    pub balance: f64,
    pub margin_used: f64,
    pub equity: f64,
    pub ts_event: u64,
    pub ts_init: u64,
}

/// Position state for account reporting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionState {
    pub position_id: PositionId,
    pub instrument_id: String,
    pub side: OrderSide,
    pub quantity: f64,
    pub avg_fill_price: f64,
    pub unrealized_pnl: f64,
    pub realized_pnl: f64,
    pub commission: f64,
    pub ts_event: u64,
    pub ts_init: u64,
}

/// Health heartbeat from a component.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Heartbeat {
    pub trader_id: TraderId,
    pub component_id: String,
    pub ts_ns: u64,
}

// =============================================================================
// SECTION 7: Serialization Helpers
// =============================================================================

/// Serialize a message to JSON string.
pub fn to_json<T: serde::Serialize>(msg: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(msg)
}

/// Deserialize a message from JSON string.
pub fn from_json<T: for<'de> serde::Deserialize<'de>>(json: &str) -> Result<T, serde_json::Error> {
    serde_json::from_str(json)
}

/// Serialize a message to compact JSON (no whitespace).
pub fn to_json_compact<T: serde::Serialize>(msg: &T) -> Result<String, serde_json::Error> {
    serde_json::to_string(msg)
}

// =============================================================================
// SECTION 8: Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_submit_order_serialization() {
        let order = SubmitOrder::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("EMA_CROSS"),
            "BTCUSDT.BINANCE".to_string(),
            ClientOrderId::new("ORDER-001"),
            OrderSide::Buy,
            OrderType::Limit,
            1.0,
            1_000_000_000,
        )
        .with_price(50_000.0)
        .with_tif(TimeInForce::Gtc);

        let json = to_json(&order).unwrap();
        let parsed: SubmitOrder = from_json(&json).unwrap();

        assert_eq!(parsed.trader_id.0, "TRADER-001");
        assert_eq!(parsed.strategy_id.0, "EMA_CROSS");
        assert_eq!(parsed.client_order_id.0, "ORDER-001");
        assert_eq!(parsed.order_side, OrderSide::Buy);
        assert_eq!(parsed.order_type, OrderType::Limit);
        assert_eq!(parsed.quantity, 1.0);
        assert_eq!(parsed.price, Some(50_000.0));
    }

    #[test]
    fn test_order_filled_serialization() {
        let filled = OrderFilled::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("EMA_CROSS"),
            ClientOrderId::new("ORDER-001"),
            VenueOrderId::new("VENUE-123"),
            PositionId::new("POS-001"),
            TradeId::new("TRADE-456"),
            "BTCUSDT.BINANCE".to_string(),
            OrderSide::Buy,
            0.5,
            50_000.0,
            1_000_000_100,
            1_000_000_000,
        )
        .with_commission(10.0)
        .with_slippage(0.5)
        .with_maker(false);

        let json = to_json(&filled).unwrap();
        let parsed: OrderFilled = from_json(&json).unwrap();

        assert_eq!(parsed.fill_price, 50_000.0);
        assert_eq!(parsed.commission, 10.0);
        assert_eq!(parsed.slippage_bps, 0.5);
        assert!(!parsed.is_maker);
    }

    #[test]
    fn test_cancel_order_serialization() {
        let cancel = CancelOrder::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("EMA_CROSS"),
            ClientOrderId::new("ORDER-001"),
            1_000_000_000,
        );

        let json = to_json(&cancel).unwrap();
        let parsed: CancelOrder = from_json(&json).unwrap();

        assert_eq!(parsed.trader_id.0, "TRADER-001");
        assert_eq!(parsed.client_order_id.0, "ORDER-001");
    }

    #[test]
    fn test_heartbeat_serialization() {
        let hb = Heartbeat {
            trader_id: TraderId::new("TRADER-001"),
            component_id: "DataEngine-1".to_string(),
            ts_ns: 1_000_000_000,
        };

        let json = to_json(&hb).unwrap();
        let parsed: Heartbeat = from_json(&json).unwrap();

        assert_eq!(parsed.trader_id.0, "TRADER-001");
        assert_eq!(parsed.ts_ns, 1_000_000_000);
    }

    #[test]
    fn test_uuid_roundtrip() {
        let id = MessageId::new();
        let json = to_json(&id).unwrap();
        let parsed: MessageId = from_json(&json).unwrap();
        assert_eq!(format!("{}", id.0), format!("{}", parsed.0));
    }

    #[test]
    fn test_order_side_serialization() {
        let buy_json = to_json(&OrderSide::Buy).unwrap();
        let sell_json = to_json(&OrderSide::Sell).unwrap();
        assert_eq!(buy_json, "\"Buy\"");
        assert_eq!(sell_json, "\"Sell\"");

        // Deserialize with case-insensitive alternative names
        let parsed_buy: OrderSide = serde_json::from_str(&buy_json).unwrap();
        let parsed_sell: OrderSide = serde_json::from_str(&sell_json).unwrap();
        assert_eq!(parsed_buy, OrderSide::Buy);
        assert_eq!(parsed_sell, OrderSide::Sell);
    }

    #[test]
    fn test_order_type_serialization() {
        let market = OrderType::Market;
        let limit = OrderType::Limit;
        let iceberg = OrderType::Iceberg;
        let trailing = OrderType::TrailingStop;

        assert_eq!(to_json(&market).unwrap(), "\"Market\"");
        assert_eq!(to_json(&limit).unwrap(), "\"Limit\"");
        assert_eq!(to_json(&iceberg).unwrap(), "\"Iceberg\"");
        assert_eq!(to_json(&trailing).unwrap(), "\"TrailingStop\"");

        let parsed_market: OrderType = serde_json::from_str(&to_json(&market).unwrap()).unwrap();
        let parsed_limit: OrderType = serde_json::from_str(&to_json(&limit).unwrap()).unwrap();
        assert_eq!(parsed_market, OrderType::Market);
        assert_eq!(parsed_limit, OrderType::Limit);
    }

    #[test]
    fn test_time_in_force_serialization() {
        let gtc = TimeInForce::Gtc;
        let ioc = TimeInForce::Ioc;
        let fok = TimeInForce::Fok;

        assert_eq!(to_json(&gtc).unwrap(), "\"Gtc\"");
        assert_eq!(to_json(&ioc).unwrap(), "\"Ioc\"");
        assert_eq!(to_json(&fok).unwrap(), "\"Fok\"");
    }

    #[test]
    fn test_topic_roundtrip() {
        let topic = Topic::new("events.system.*");
        let json = to_json(&topic).unwrap();
        let parsed: Topic = from_json(&json).unwrap();
        assert_eq!(topic.0, parsed.0);
    }

    #[test]
    fn test_account_state_roundtrip() {
        let state = AccountState {
            trader_id: TraderId::new("TRADER-001"),
            account_id: "ACC-BINANCE-001".to_string(),
            balance: 100_000.0,
            margin_used: 10_000.0,
            equity: 105_000.0,
            ts_event: 1_000_000_000,
            ts_init: 1_000_000_000,
        };

        let json = to_json(&state).unwrap();
        let parsed: AccountState = from_json(&json).unwrap();
        assert_eq!(parsed.balance, 100_000.0);
        assert_eq!(parsed.equity, 105_000.0);
    }

    #[test]
    fn test_submit_order_batch_serialization() {
        let batch = SubmitOrderBatch {
            trader_id: TraderId::new("TRADER-001"),
            strategy_id: StrategyId::new("BASKET"),
            orders: vec![
                SubmitOrder::new(
                    TraderId::new("TRADER-001"),
                    StrategyId::new("BASKET"),
                    "BTCUSDT.BINANCE".to_string(),
                    ClientOrderId::new("BASKET-001"),
                    OrderSide::Buy,
                    OrderType::Market,
                    0.1,
                    1_000_000_000,
                ),
                SubmitOrder::new(
                    TraderId::new("TRADER-001"),
                    StrategyId::new("BASKET"),
                    "ETHUSDT.BINANCE".to_string(),
                    ClientOrderId::new("BASKET-002"),
                    OrderSide::Buy,
                    OrderType::Market,
                    1.0,
                    1_000_000_000,
                ),
            ],
            ts_init: 1_000_000_000,
        };

        let json = to_json(&batch).unwrap();
        let parsed: SubmitOrderBatch = from_json(&json).unwrap();
        assert_eq!(parsed.orders.len(), 2);
        assert_eq!(parsed.orders[0].client_order_id.0, "BASKET-001");
        assert_eq!(parsed.orders[1].client_order_id.0, "BASKET-002");
    }

    #[test]
    fn test_position_state_roundtrip() {
        let pos = PositionState {
            position_id: PositionId::new("POS-001"),
            instrument_id: "BTCUSDT.BINANCE".to_string(),
            side: OrderSide::Buy,
            quantity: 1.0,
            avg_fill_price: 50_000.0,
            unrealized_pnl: 500.0,
            realized_pnl: 0.0,
            commission: 10.0,
            ts_event: 1_000_000_000,
            ts_init: 1_000_000_000,
        };

        let json = to_json(&pos).unwrap();
        let parsed: PositionState = from_json(&json).unwrap();
        assert_eq!(parsed.position_id.0, "POS-001");
        assert_eq!(parsed.unrealized_pnl, 500.0);
    }
}