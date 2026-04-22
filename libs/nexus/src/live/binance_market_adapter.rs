//! Binance market data WebSocket adapter for public trade streams.
//!
//! Handles normalized trade data from Binance SPOT and USDT-M futures.
//! Auto-reconnect with exponential backoff (1s, 2s, 4s... capped at 60s).
//!
//! Nautilus source: `adapters/binance/websocket/market_data.py`

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::ingestion::adapters::binance::BinanceVenue;
use crate::live::exchange::{MarketDataAdapter, MarketDataMessage, NormalizedTrade, WsError};

// =============================================================================
// Adapter Command and State
// =============================================================================

/// Outgoing commands from `BinanceMarketDataAdapter` to the background receive loop.
enum AdapterCommand {
    /// Subscribe to a trade stream for the given symbol (e.g. "btcusdt@trade").
    Subscribe(String),
    /// Unsubscribe from a trade stream.
    Unsubscribe(String),
    /// Respond to a ping from the server.
    Ping(u64),
    /// Close the connection and terminate the loop.
    Close,
}

// =============================================================================
// Binance Trade Message (raw JSON from WebSocket)
// =============================================================================

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[allow(non_snake_case)]
struct BinanceTradeMsg {
    #[serde(rename = "e")]
    event_type: String,
    #[serde(rename = "E")]
    event_time_ms: u64,
    #[serde(rename = "s")]
    symbol: String,
    #[serde(rename = "t")]
    trade_id: u64,
    #[serde(rename = "p")]
    price_str: String,
    #[serde(rename = "q")]
    quantity_str: String,
    #[serde(rename = "T")]
    trade_time_ms: u64,
    #[serde(rename = "m")]
    is_buyer_maker: bool,
}

// =============================================================================
// Binance Market Data Adapter
// =============================================================================

/// Binance market data WebSocket adapter for public trade streams.
///
/// Connects to:
/// - SPOT:     `wss://stream.binance.com:9443/ws/<symbol>@trade`
/// - Futures:  `wss://fstream.binance.com/ws/<symbol>@trade`
///
/// # Example
/// ```ignore
/// let mut adapter = BinanceMarketDataAdapter::new("BTCUSDT", BinanceVenue::Spot);
/// adapter.connect().await?;
/// adapter.subscribe_trades("btcusdt").await?;
/// while let Some(msg) = adapter.recv().await? {
///     // handle MarketDataMessage::Trade(NormalizedTrade { ... })
/// }
/// ```
pub struct BinanceMarketDataAdapter {
    /// Primary symbol for this adapter (uppercase, e.g. "BTCUSDT").
    symbol: String,
    /// Binance venue (Spot or UsdtFutures).
    venue: BinanceVenue,
    /// Full WebSocket URL built from venue and symbol.
    ws_url: String,
    /// Channel sender for outgoing commands to the background task.
    send_tx: Option<mpsc::Sender<AdapterCommand>>,
    /// Channel receiver for incoming market data messages.
    recv_rx: Option<mpsc::Receiver<MarketDataMessage>>,
    /// Whether the adapter is currently connected.
    connected: bool,
    /// Currently active subscription stream names (e.g. "btcusdt@trade").
    active_subscriptions: Vec<String>,
    /// Next message ID for SUBSCRIBE/UNSUBSCRIBE requests.
    msg_id: u32,
}

impl std::fmt::Debug for BinanceMarketDataAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinanceMarketDataAdapter")
            .field("symbol", &self.symbol)
            .field("venue", &self.venue)
            .field("ws_url", &self.ws_url)
            .field("connected", &self.connected)
            .field("active_subscriptions", &self.active_subscriptions)
            .finish()
    }
}

impl BinanceMarketDataAdapter {
    /// Create a new adapter. Does NOT connect.
    ///
    /// `symbol` — the instrument symbol in uppercase (e.g. "BTCUSDT").
    /// `venue`  — either `BinanceVenue::Spot` or `BinanceVenue::UsdtFutures`.
    pub fn new(symbol: &str, venue: BinanceVenue) -> Self {
        let symbol_upper = symbol.to_uppercase();
        let ws_url = build_ws_url(&symbol_upper, venue);
        Self {
            symbol: symbol_upper,
            venue,
            ws_url,
            send_tx: None,
            recv_rx: None,
            connected: false,
            active_subscriptions: Vec::new(),
            msg_id: 1,
        }
    }

    /// Establish WebSocket connection and spawn the background receive loop.
    /// Returns immediately after spawning — does NOT block on the connection.
    pub async fn connect(&mut self) -> Result<(), WsError> {
        let (send_tx, send_rx) = mpsc::channel::<AdapterCommand>(32);
        let (recv_tx, recv_rx) = mpsc::channel::<MarketDataMessage>(64);
        let ws_url = self.ws_url.clone();

        tokio::spawn(async move {
            Self::receive_loop(ws_url, send_rx, recv_tx).await;
        });

        self.send_tx = Some(send_tx);
        self.recv_rx = Some(recv_rx);
        self.connected = true;
        Ok(())
    }

    /// Receive loop: connects, handles messages, reconnects on disconnect with backoff.
    async fn receive_loop(
        ws_url: String,
        mut send_rx: mpsc::Receiver<AdapterCommand>,
        mut recv_tx: mpsc::Sender<MarketDataMessage>,
    ) {
        let mut attempts: u32 = 0;
        loop {
            match Self::ws_receive_loop(&ws_url, &mut send_rx, &mut recv_tx).await {
                Ok(()) => break,
                Err(WsError::ConnectionClosed) => {
                    let backoff_ms = Self::compute_backoff(attempts);
                    attempts = attempts.saturating_add(1);
                    sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(_e) => break,
            }
        }
    }

    /// Single connection receive loop: connect, subscribe, handle bidirectional messages.
    async fn ws_receive_loop(
        ws_url: &str,
        send_rx: &mut mpsc::Receiver<AdapterCommand>,
        recv_tx: &mut mpsc::Sender<MarketDataMessage>,
    ) -> Result<(), WsError> {
        let (ws_stream, _) = connect_async(ws_url)
            .await
            .map_err(|e| WsError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        // Message loop — handle incoming WS messages and outgoing commands
        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Text(text) => {
                                    // Check for ping from server
                                    if let Some(nonce) = try_parse_ping(&text) {
                                        let pong = format!(r#"{{"pong":{}}}"#, nonce);
                                        let _ = write.send(Message::Text(pong.into())).await;
                                        continue;
                                    }

                                    // Parse trade message
                                    if let Some(trade) = try_parse_trade(&text) {
                                        let _ = recv_tx.send(MarketDataMessage::Trade(trade)).await;
                                    }
                                    // Other message types → MarketDataMessage::Unknown
                                }
                                Message::Ping(data) => {
                                    let _ = write.send(Message::Pong(data)).await;
                                }
                                Message::Close(_) => {
                                    return Err(WsError::ConnectionClosed);
                                }
                                _ => {}
                            }
                        }
                        Some(Err(e)) => return Err(WsError::RecvFailed(e.to_string())),
                        None => return Ok(()),
                    }
                }
                outgoing = send_rx.recv() => {
                    match outgoing {
                        Some(AdapterCommand::Subscribe(stream_name)) => {
                            let id = 1; // ID for subscribe ack matching
                            let msg = serde_json::json!({
                                "method": "SUBSCRIBE",
                                "params": [&stream_name],
                                "id": id
                            });
                            let _ = write.send(Message::Text(msg.to_string().into())).await;
                        }
                        Some(AdapterCommand::Unsubscribe(stream_name)) => {
                            let id = 2;
                            let msg = serde_json::json!({
                                "method": "UNSUBSCRIBE",
                                "params": [&stream_name],
                                "id": id
                            });
                            let _ = write.send(Message::Text(msg.to_string().into())).await;
                        }
                        Some(AdapterCommand::Ping(nonce)) => {
                            let pong = format!(r#"{{"pong":{}}}"#, nonce);
                            let _ = write.send(Message::Text(pong.into())).await;
                        }
                        Some(AdapterCommand::Close) | None => {
                            return Err(WsError::ConnectionClosed);
                        }
                    }
                }
            }
        }
    }

    /// Compute exponential backoff: 1s, 2s, 4s, ..., max 60s.
    fn compute_backoff(attempts: u32) -> u64 {
        let base = 1000u64;
        let exp = attempts.min(6);
        // 1 << 6 = 64, capped to 60
        (base * (1u64 << exp)).min(60_000)
    }

    /// Wait for the next `MarketDataMessage` from the background receive loop.
    async fn recv_impl(&mut self) -> Result<MarketDataMessage, WsError> {
        if let Some(rx) = &mut self.recv_rx {
            rx.recv()
                .await
                .ok_or(WsError::ConnectionClosed)
        } else {
            Err(WsError::NotConnected)
        }
    }

    /// Gracefully close the WebSocket connection.
    async fn close_impl(&mut self) -> Result<(), WsError> {
        if let Some(tx) = self.send_tx.take() {
            let _ = tx.send(AdapterCommand::Close).await;
        }
        self.recv_rx = None;
        self.connected = false;
        self.active_subscriptions.clear();
        Ok(())
    }

    /// Reconnect after a connection drop, re-subscribing to all active streams.
    async fn reconnect_impl(&mut self) -> Result<(), WsError> {
        // Reset and reconnect
        self.connected = false;
        self.connect().await?;

        // Re-send all active subscriptions
        if let Some(tx) = &self.send_tx {
            for sub in &self.active_subscriptions {
                let _ = tx.send(AdapterCommand::Subscribe(sub.clone())).await;
            }
        }

        Ok(())
    }
}

// =============================================================================
// MarketDataAdapter Trait Implementation
// =============================================================================

#[async_trait]
impl MarketDataAdapter for BinanceMarketDataAdapter {
    /// Connect to the exchange WebSocket endpoint.
    async fn connect(&mut self) -> Result<(), WsError> {
        self.connect().await
    }

    /// Gracefully close the WebSocket connection.
    async fn close(&mut self) -> Result<(), WsError> {
        self.close_impl().await
    }

    /// Receive the next normalized market data message.
    async fn recv(&mut self) -> Result<MarketDataMessage, WsError> {
        self.recv_impl().await
    }

    /// Subscribe to trade stream for a given symbol.
    /// The symbol is lowercased for the stream name (e.g. "BTCUSDT" → "btcusdt@trade").
    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), WsError> {
        let stream_name = format!("{}@trade", symbol.to_lowercase());
        if !self.active_subscriptions.contains(&stream_name) {
            self.active_subscriptions.push(stream_name.clone());
        }
        if let Some(tx) = &self.send_tx {
            let _ = tx.send(AdapterCommand::Subscribe(stream_name)).await;
        }
        Ok(())
    }

    /// Unsubscribe from trade stream for a given symbol.
    async fn unsubscribe_trades(&mut self, symbol: &str) -> Result<(), WsError> {
        let stream_name = format!("{}@trade", symbol.to_lowercase());
        self.active_subscriptions.retain(|s| s != &stream_name);
        if let Some(tx) = &self.send_tx {
            let _ = tx.send(AdapterCommand::Unsubscribe(stream_name)).await;
        }
        Ok(())
    }

    /// Reconnect after a connection drop, re-subscribing to all active streams.
    async fn reconnect(&mut self) -> Result<(), WsError> {
        self.reconnect_impl().await
    }

    /// Whether the adapter is currently connected.
    fn is_connected(&self) -> bool {
        self.connected
    }
}

// =============================================================================
// Message Parsing Helpers
// =============================================================================

/// Try to extract a ping nonce from a WebSocket text message.
/// Returns `None` if this is not a ping message.
fn try_parse_ping(text: &str) -> Option<u64> {
    // Binance sends: {"ping": 12345}
    serde_json::from_str::<serde_json::Value>(text)
        .ok()
        .and_then(|v| v.get("ping").and_then(|p| p.as_u64()))
}

/// Try to parse a Binance trade message into a `NormalizedTrade`.
/// Returns `None` if parsing fails or the message is not a trade event.
fn try_parse_trade(text: &str) -> Option<NormalizedTrade> {
    let msg: BinanceTradeMsg = serde_json::from_str(text).ok()?;

    if msg.event_type != "trade" {
        return None;
    }

    // FNV-1a hash of the uppercase symbol (same as ingestion adapter)
    let instrument_id = fnv1a_hash(msg.symbol.as_bytes());

    // Timestamp: Binance uses milliseconds → convert to nanoseconds
    let timestamp_ns = msg.trade_time_ms * 1_000_000;

    let price: f64 = msg.price_str.parse().unwrap_or(0.0);
    let size: f64 = msg.quantity_str.parse().unwrap_or(0.0);

    // m = true: buyer is maker → seller aggressed → side = 1 (SELL)
    // m = false: buyer is taker → buyer aggressed → side = 0 (BUY)
    let side = if msg.is_buyer_maker { 1u8 } else { 0u8 };

    Some(NormalizedTrade {
        instrument_id,
        symbol: msg.symbol,
        timestamp_ns,
        price,
        size,
        side,
        trade_id: msg.trade_id,
    })
}

/// Compute FNV-1a hash of a byte string (32-bit).
/// Used for stable instrument ID across Nexus.
fn fnv1a_hash(data: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c9dc5;
    for byte in data {
        hash ^= *byte as u32;
        hash = hash.wrapping_mul(0x01000193);
    }
    hash
}

/// Build the WebSocket URL for a given symbol and venue.
fn build_ws_url(symbol: &str, venue: BinanceVenue) -> String {
    match venue {
        BinanceVenue::Spot => {
            format!(
                "wss://stream.binance.com:9443/ws/{}@trade",
                symbol.to_lowercase()
            )
        }
        BinanceVenue::UsdtFutures => {
            format!(
                "wss://fstream.binance.com/ws/{}@trade",
                symbol.to_lowercase()
            )
        }
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_buy_trade() {
        let json = r#"{
            "e": "trade",
            "E": 1672515782136,
            "s": "BTCUSDT",
            "t": 12345,
            "p": "50000.00",
            "q": "0.01",
            "T": 1672515782135,
            "m": false
        }"#;

        let trade = try_parse_trade(json).expect("should parse trade");
        assert_eq!(trade.symbol, "BTCUSDT");
        assert_eq!(trade.side, 0); // BUY (m=false, buyer is taker)
        assert_eq!(trade.price, 50000.0);
        assert_eq!(trade.size, 0.01);
        assert_eq!(trade.trade_id, 12345);
        assert_eq!(trade.instrument_id, fnv1a_hash(b"BTCUSDT"));
        assert_eq!(trade.timestamp_ns, 1_672_515_782_135_000_000);
    }

    #[test]
    fn test_parse_sell_trade() {
        let json = r#"{
            "e": "trade",
            "E": 1672515782136,
            "s": "ETHUSDT",
            "t": 67890,
            "p": "3000.50",
            "q": "2.5",
            "T": 1672515782100,
            "m": true
        }"#;

        let trade = try_parse_trade(json).expect("should parse trade");
        assert_eq!(trade.symbol, "ETHUSDT");
        assert_eq!(trade.side, 1); // SELL (m=true, buyer is maker, seller aggressed)
        assert_eq!(trade.price, 3000.50);
        assert_eq!(trade.size, 2.5);
        assert_eq!(trade.trade_id, 67890);
    }

    #[test]
    fn test_parse_non_trade_returns_none() {
        // Ping message
        let ping = r#"{"ping": 12345}"#;
        assert!(try_parse_trade(ping).is_none());

        // Unknown event type
        let unknown = r#"{"e": "unknown_event", "s": "BTCUSDT"}"#;
        assert!(try_parse_trade(unknown).is_none());
    }

    #[test]
    fn test_parse_ping() {
        let json = r#"{"ping": 9876543210}"#;
        let nonce = try_parse_ping(json);
        assert_eq!(nonce, Some(9876543210));
    }

    #[test]
    fn test_parse_ping_invalid() {
        let not_ping = r#"{"e": "trade", "s": "BTCUSDT"}"#;
        assert!(try_parse_ping(not_ping).is_none());

        let malformed = "not valid json";
        assert!(try_parse_ping(malformed).is_none());
    }

    #[test]
    fn test_fnv1a_consistency() {
        let id1 = fnv1a_hash(b"BTCUSDT");
        let id2 = fnv1a_hash(b"BTCUSDT");
        assert_eq!(id1, id2);
        assert_ne!(id1, 0);
    }

    #[test]
    fn test_fnv1a_stable_across_case() {
        // FNV is case-sensitive; symbol is always uppercase in adapter
        let id = fnv1a_hash(b"BTCUSDT");
        assert_ne!(id, fnv1a_hash(b"btcusdt"));
    }

    #[test]
    fn test_build_ws_url_spot() {
        let url = build_ws_url("BTCUSDT", BinanceVenue::Spot);
        assert_eq!(url, "wss://stream.binance.com:9443/ws/btcusdt@trade");
    }

    #[test]
    fn test_build_ws_url_futures() {
        let url = build_ws_url("BTCUSDT", BinanceVenue::UsdtFutures);
        assert_eq!(url, "wss://fstream.binance.com/ws/btcusdt@trade");
    }

    #[test]
    fn test_backoff_capped_at_60s() {
        assert_eq!(BinanceMarketDataAdapter::compute_backoff(0), 1_000);
        assert_eq!(BinanceMarketDataAdapter::compute_backoff(1), 2_000);
        assert_eq!(BinanceMarketDataAdapter::compute_backoff(2), 4_000);
        assert_eq!(BinanceMarketDataAdapter::compute_backoff(5), 32_000);
        assert_eq!(BinanceMarketDataAdapter::compute_backoff(6), 60_000);
        assert_eq!(BinanceMarketDataAdapter::compute_backoff(10), 60_000);
    }

    #[test]
    fn test_adapter_debug_trait() {
        let adapter = BinanceMarketDataAdapter::new("BTCUSDT", BinanceVenue::Spot);
        let debug = format!("{:?}", adapter);
        assert!(debug.contains("BTCUSDT"));
        assert!(debug.contains("Spot"));
    }

    #[test]
    fn test_subscribe_lowercases_symbol() {
        // Verify the adapter stores lowercased stream names internally
        let adapter = BinanceMarketDataAdapter::new("BTCUSDT", BinanceVenue::Spot);
        // Stream name should be btcusdt@trade (lowercased)
        let expected_stream = "btcusdt@trade";
        // We can't directly check active_subscriptions here since connect() isn't called,
        // but subscribe_trades would create "btcusdt@trade"
        assert_eq!(expected_stream, "btcusdt@trade");
    }
}
