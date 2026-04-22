//! Unified Exchange traits for multi-exchange support.
//!
//! All exchanges implement `Exchange` (REST HTTP) and `ExchangeWs` (WebSocket user data).
//!
//! ## Async Trait Note
//! All trait methods use `#[async_trait]` because Rust's native async fn in traits
//! requires the `async-trait` crate. `async-trait = "0.1"` is in Cargo.toml.
//! Do NOT use `impl std::future::Future` directly in trait signatures -- it does not
//! work with dynamic dispatch (`Box<dyn>`) without `async-trait`.

use async_trait::async_trait;
use crate::messages::{
    CancelOrder, ClientOrderId, SubmitOrder, VenueOrderId,
};
use crate::live::http_adapter::{OrderStatusResponse, OrderInfoResponse};

// =============================================================================
// Error Types
// =============================================================================

/// Unified exchange error -- each adapter returns its own variant.
#[derive(Debug, Clone)]
pub enum ExchangeError {
    NetworkError(String),
    RateLimited { retry_after_ms: u64 },
    AuthFailed,
    InvalidSignature,
    InsufficientBalance,
    OrderNotFound,
    RiskRejected(&'static str),
    Unknown(String),
}

impl std::fmt::Display for ExchangeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExchangeError::NetworkError(msg) => write!(f, "NetworkError: {}", msg),
            ExchangeError::RateLimited { retry_after_ms } => {
                write!(f, "RateLimited: retry after {}ms", retry_after_ms)
            }
            ExchangeError::AuthFailed => write!(f, "AuthFailed"),
            ExchangeError::InvalidSignature => write!(f, "InvalidSignature"),
            ExchangeError::InsufficientBalance => write!(f, "InsufficientBalance"),
            ExchangeError::OrderNotFound => write!(f, "OrderNotFound"),
            ExchangeError::RiskRejected(reason) => write!(f, "RiskRejected: {}", reason),
            ExchangeError::Unknown(msg) => write!(f, "Unknown: {}", msg),
        }
    }
}

impl std::error::Error for ExchangeError {}

// =============================================================================
// Shared Response Types
// =============================================================================

/// Account balance info -- shared across all exchanges.
#[derive(Debug, Clone)]
pub struct AccountInfoResponse {
    pub balances: Vec<AssetBalance>,
}

#[derive(Debug, Clone)]
pub struct AssetBalance {
    pub asset: String,
    pub free: String,
    pub locked: String,
}

// =============================================================================
// Exchange Trait (REST HTTP)
// =============================================================================

/// Unified REST HTTP exchange trait.
/// All exchange adapters implement this trait.
#[async_trait]
pub trait Exchange: Send + Sync {
    /// Place a market or limit order. Returns the venue-assigned order ID.
    async fn place_order(&self, order: &SubmitOrder) -> Result<VenueOrderId, ExchangeError>;

    /// Cancel an order.
    async fn cancel_order(&self, cancel: &CancelOrder) -> Result<bool, ExchangeError>;

    /// Modify an existing order's price and/or quantity.
    async fn modify_order(
        &self,
        client_order_id: &ClientOrderId,
        venue_order_id: Option<&VenueOrderId>,
        side: crate::messages::OrderSide,
        new_price: Option<f64>,
        new_quantity: Option<f64>,
        symbol: &str,
    ) -> Result<VenueOrderId, ExchangeError>;

    /// Get order status by client order ID and symbol.
    async fn get_order_status(
        &self,
        client_order_id: &ClientOrderId,
        symbol: &str,
    ) -> Result<OrderStatusResponse, ExchangeError>;

    /// Get all open orders.
    async fn get_open_orders(&self) -> Result<Vec<OrderInfoResponse>, ExchangeError>;

    /// Get account balance information.
    async fn get_account_info(&self) -> Result<AccountInfoResponse, ExchangeError>;

    /// Place an order list (OCO/OTO/bracket). Returns list of venue-assigned order IDs.
    async fn place_order_list(
        &self,
        orders: &[SubmitOrder],
    ) -> Result<Vec<VenueOrderId>, ExchangeError>;

    /// Get a fresh listen-key for WebSocket user data stream.
    /// Binance: POST /api/v3/userDataStream -> {"listenKey": "..."}
    /// Bybit/OKX: Not supported via this interface (they use token-based auth).
    async fn fetch_listen_key(&self) -> Result<String, ExchangeError> {
        Err(ExchangeError::Unknown("fetch_listen_key not supported on this exchange".to_string()))
    }
}

// =============================================================================
// ExchangeWs Trait (WebSocket User Data)
// =============================================================================

/// Unified WebSocket user data stream trait.
/// All exchange WS adapters implement this trait.
/// Incoming WebSocket message from any exchange -- outer enum with exchange-specific variants.
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum WsMessage {
    /// Binance execution report (executionReport event).
    BinanceExec(BinanceExecutionReport),
    /// Bybit execution report (orderReport event).
    BybitExec(BybitExecutionReport),
    /// OKX execution report (orders channel).
    OkxExec(OkxExecutionReport),
    /// Binance balance update.
    BinanceBalance(BinanceBalanceUpdate),
    /// Bybit balance update.
    BybitBalance(BybitBalanceUpdate),
    /// OKX balance update.
    OkxBalance(OkxBalanceUpdate),
    /// Binance account position update.
    BinanceAccount(BinanceAccountUpdate),
    /// Bybit account update.
    BybitAccount(BybitAccountUpdate),
    /// OKX account update.
    OkxAccount(OkxAccountUpdate),
    /// Binance list status.
    BinanceListStatus(BinanceListStatus),
    /// OKX list status.
    OkxListStatus(OkxListStatus),
    /// Listen key expired -- all exchanges use this signal to reconnect.
    ListenKeyExpired,
    /// Unknown or unhandled event type with raw event type tag.
    Unknown(String),
}

// ---------------------------------------------------------------------------
// Binance WS types (parsed from Binance JSON field names)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BinanceExecutionReport {
    pub event_time: u64,
    pub symbol: String,
    pub client_order_id: String,
    pub exchange_order_id: u64,
    pub side: crate::messages::OrderSide,
    pub order_type: String,
    pub time_in_force: String,
    pub quantity: String,
    pub price: String,
    pub stop_price: String,
    pub last_fill_qty: String,
    pub accumulated_qty: String,
    pub last_fill_price: String,
    pub commission: Option<String>,
    pub commission_asset: Option<String>,
    pub trade_time: u64,
    pub trade_id: u64,
    pub is_on_book: bool,
    pub is_maker: bool,
    pub order_status: String,
    pub reject_reason: String,
}

#[derive(Debug, Clone)]
pub struct BinanceBalanceUpdate {
    pub asset: String,
    pub balance: String,
    pub is_deposit: bool,
}

#[derive(Debug, Clone)]
pub struct BinanceAccountUpdate {
    pub event_time: u64,
    pub balances: Vec<BinanceBalanceInfo>,
}

#[derive(Debug, Clone)]
pub struct BinanceBalanceInfo {
    pub asset: String,
    pub free: String,
    pub locked: String,
}

#[derive(Debug, Clone)]
pub struct BinanceListStatus {
    pub list_status_type: String,
    pub list_order_status: String,
    pub list_client_order_id: String,
    pub list_id: String,
    pub symbol: String,
    pub orders: Vec<BinanceListOrder>,
}

#[derive(Debug, Clone)]
pub struct BinanceListOrder {
    pub symbol: String,
    pub order_id: u64,
    pub client_order_id: String,
}

// ---------------------------------------------------------------------------
// Bybit WS types (parsed from Bybit JSON field names -- differs from Binance)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct BybitExecutionReport {
    pub event_time: u64,
    pub symbol: String,
    pub client_order_id: String,
    pub exchange_order_id: String,
    pub side: crate::messages::OrderSide,
    pub order_type: String,
    pub order_status: String,
    pub reject_reason: String,
    pub price: String,
    pub quantity: String,
    pub last_fill_qty: String,
    pub last_fill_price: String,
    pub trade_time: u64,
    pub trade_id: String,
    pub is_maker: bool,
    pub commission: Option<String>,
    pub commission_asset: Option<String>,
}

#[derive(Debug, Clone)]
pub struct BybitBalanceUpdate {
    pub asset: String,
    pub balance: String,
    pub is_deposit: bool,
}

#[derive(Debug, Clone)]
pub struct BybitAccountUpdate {
    pub event_time: u64,
    pub balances: Vec<BybitBalanceInfo>,
}

#[derive(Debug, Clone)]
pub struct BybitBalanceInfo {
    pub asset: String,
    pub free: String,
    pub locked: String,
}

// ---------------------------------------------------------------------------
// OKX WS types (parsed from OKX JSON field names)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OkxExecutionReport {
    pub event_time: u64,
    pub symbol: String,
    pub client_order_id: String,
    pub exchange_order_id: String,
    pub side: crate::messages::OrderSide,
    pub order_type: String,
    pub order_status: String,
    pub reject_reason: String,
    pub price: String,
    pub quantity: String,
    pub last_fill_qty: String,
    pub last_fill_price: String,
    pub trade_time: u64,
    pub trade_id: String,
    pub is_maker: bool,
    pub commission: Option<String>,
    pub commission_asset: Option<String>,
}

#[derive(Debug, Clone)]
pub struct OkxBalanceUpdate {
    pub asset: String,
    pub balance: String,
    pub is_deposit: bool,
}

#[derive(Debug, Clone)]
pub struct OkxAccountUpdate {
    pub event_time: u64,
    pub balances: Vec<OkxBalanceInfo>,
}

#[derive(Debug, Clone)]
pub struct OkxBalanceInfo {
    pub asset: String,
    pub free: String,
    pub locked: String,
}

#[derive(Debug, Clone)]
pub struct OkxListStatus {
    pub list_status_type: String,
    pub list_order_status: String,
    pub list_client_order_id: String,
    pub list_id: String,
    pub symbol: String,
    pub orders: Vec<OkxListOrder>,
}

#[derive(Debug, Clone)]
pub struct OkxListOrder {
    pub symbol: String,
    pub order_id: String,
    pub client_order_id: String,
}

/// WebSocket errors across all exchanges.
#[derive(Debug)]
pub enum WsError {
    ConnectionFailed(String),
    ConnectionClosed,
    NotConnected,
    RecvFailed(String),
    SendFailed(String),
    SubscribeFailed(String),
    ParseError(String),
}

impl std::fmt::Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WsError::ConnectionFailed(msg) => write!(f, "ConnectionFailed: {}", msg),
            WsError::ConnectionClosed => write!(f, "ConnectionClosed"),
            WsError::NotConnected => write!(f, "NotConnected"),
            WsError::RecvFailed(msg) => write!(f, "RecvFailed: {}", msg),
            WsError::SendFailed(msg) => write!(f, "SendFailed: {}", msg),
            WsError::SubscribeFailed(msg) => write!(f, "SubscribeFailed: {}", msg),
            WsError::ParseError(msg) => write!(f, "ParseError: {}", msg),
        }
    }
}

impl std::error::Error for WsError {}

#[async_trait]
pub trait ExchangeWs: Send + Sync {
    /// Connect to the WebSocket user data stream.
    async fn connect(&mut self) -> Result<(), WsError>;

    /// Gracefully close the WebSocket connection.
    async fn close(&mut self) -> Result<(), WsError>;

    /// Reconnect using a new listen key (called after listen key expiry).
    async fn reconnect(&mut self, listen_key: &str) -> Result<(), WsError>;

    /// Receive the next WebSocket message. Returns the exchange-specific variant.
    async fn recv(&mut self) -> Result<WsMessage, WsError>;
}

// =============================================================================
// Market Data Adapter Trait (public market data streams)
// =============================================================================

/// Normalized trade tick from any exchange.
#[derive(Debug, Clone)]
pub struct NormalizedTrade {
    /// FNV-1a hash of the exchange symbol.
    pub instrument_id: u32,
    /// Raw exchange symbol (e.g., "BTCUSDT", "BTC-USDT").
    pub symbol: String,
    /// Trade timestamp in nanoseconds.
    pub timestamp_ns: u64,
    pub price: f64,
    pub size: f64,
    /// 0 = buy (aggressor is buy), 1 = sell (aggressor is sell).
    pub side: u8,
    pub trade_id: u64,
}

/// Order book delta update.
#[derive(Debug, Clone)]
pub struct OrderBookDelta {
    pub instrument_id: u32,
    pub symbol: String,
    pub timestamp_ns: u64,
    pub bids: Vec<(f64, f64)>,  // (price, size)
    pub asks: Vec<(f64, f64)>,
}

/// Kline/candlestick update.
#[derive(Debug, Clone)]
pub struct KlineUpdate {
    pub instrument_id: u32,
    pub symbol: String,
    pub timestamp_ns: u64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// Ticker / book ticker update.
#[derive(Debug, Clone)]
pub struct TickerUpdate {
    pub instrument_id: u32,
    pub symbol: String,
    pub timestamp_ns: u64,
    pub bid_price: f64,
    pub ask_price: f64,
    pub bid_size: f64,
    pub ask_size: f64,
}

/// Normalized market data message from any exchange.
#[derive(Debug, Clone)]
pub enum MarketDataMessage {
    Trade(NormalizedTrade),
    Depth(OrderBookDelta),
    Kline(KlineUpdate),
    Ticker(TickerUpdate),
    Unknown(String),
}

/// Trait for exchange market data WebSocket adapters (public trade/quote/orderbook streams).
///
/// Implementations: BinanceMarketDataAdapter, BybitMarketDataAdapter, OkxMarketDataAdapter
///
/// Distinct from `ExchangeWs` which handles authenticated user data streams.
#[async_trait]
pub trait MarketDataAdapter: Send + Sync {
    /// Connect to the exchange WebSocket endpoint.
    async fn connect(&mut self) -> Result<(), WsError>;

    /// Gracefully close the WebSocket connection.
    async fn close(&mut self) -> Result<(), WsError>;

    /// Receive the next normalized market data message.
    async fn recv(&mut self) -> Result<MarketDataMessage, WsError>;

    /// Subscribe to trade stream for a given symbol.
    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), WsError>;

    /// Unsubscribe from trade stream for a given symbol.
    async fn unsubscribe_trades(&mut self, symbol: &str) -> Result<(), WsError>;

    /// Reconnect after a connection drop, re-subscribing to all active streams.
    async fn reconnect(&mut self) -> Result<(), WsError>;

    /// Whether the adapter is currently connected.
    fn is_connected(&self) -> bool;
}

// =============================================================================
// Exchange Type Enum (for runtime selection)
// =============================================================================

/// Supported exchange types.
#[derive(Debug, Clone, Copy)]
pub enum ExchangeType {
    Binance,
    Bybit,
    Okx,
}

// =============================================================================
// Message trait implementation for WsMessage
// =============================================================================

impl crate::actor::Message for WsMessage {}
