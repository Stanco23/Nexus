//! OKX public market data WebSocket adapter.
//!
//! Handles trade streams from OKX (wss://ws.okx.com:8443/ws/v5/public).
//! Implements `MarketDataAdapter` for normalized trade data.
//!
//! # OKX WebSocket Details
//! - Public market data: `wss://ws.okx.com:8443/ws/v5/public`
//! - Ping: `{"op":"ping"}` (text frame) -> respond with `{"op":"pong"}`
//! - Subscribe: `{"op":"subscribe","args":[{"channel":"trades","instId":"BTC-USDT"}]}`
//! - Symbol format: "BTC-USDT" (hyphen separator)
//!
//! # Reconnection
//! Automatically reconnects with exponential backoff (1s, 2s, 4s... capped at 60s).
//! Active subscriptions are persisted across reconnects.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::live::exchange::{
    MarketDataAdapter, MarketDataMessage, NormalizedTrade, WsError,
};
use crate::instrument::instrument_id::fnv1a_hash;

// ============================================================================
// Adapter command for internal communication
// ============================================================================

/// Internal commands for the adapter background loop.
enum AdapterCommand {
    /// Subscribe to a trade channel.
    Subscribe { channel: String, inst_id: String },
    /// Unsubscribe from a trade channel.
    Unsubscribe { channel: String, inst_id: String },
    /// Close the connection and shutdown the loop.
    Close,
}

// ============================================================================
// OKX Trade Message Types
// ============================================================================

/// OKX trade message from WebSocket.
#[derive(Debug, Deserialize)]
struct OkxTradeMsg {
    /// Always "trades" for trade messages.
    arg: OkxArg,
    /// Array of trade data.
    data: Vec<OkxTradeData>,
}

#[derive(Debug, Deserialize)]
struct OkxArg {
    /// Channel name (e.g., "trades").
    channel: String,
    /// Instrument ID (e.g., "BTC-USDT").
    #[serde(rename = "instId")]
    inst_id: String,
}

#[derive(Debug, Deserialize)]
struct OkxTradeData {
    /// Instrument ID (e.g., "BTC-USDT").
    #[serde(rename = "instId")]
    inst_id: String,
    /// Trade price as string.
    price: String,
    /// Trade size as string.
    size: String,
    /// "buy" or "sell".
    side: String,
    /// Trade ID.
    #[serde(rename = "tradeId")]
    trade_id: String,
    /// Timestamp in milliseconds (as string).
    ts: String,
}

// ============================================================================
// OkxMarketDataAdapter
// ============================================================================

/// OKX public market data WebSocket adapter.
///
/// Spawns a background task that maintains the WebSocket connection,
/// handles ping/pong, and forwards parsed trade messages via mpsc channel.
///
/// # Example
/// ```ignore
/// let mut adapter = OkxMarketDataAdapter::new("BTC-USDT");
/// adapter.connect().await?;
/// adapter.subscribe_trades("BTC-USDT").await?;
/// while let Some(msg) = adapter.recv().await? {
///     // handle MarketDataMessage::Trade(NormalizedTrade)
/// }
/// ```
pub struct OkxMarketDataAdapter {
    /// WebSocket URL.
    ws_url: String,
    /// Sender for internal commands.
    cmd_tx: Option<mpsc::Sender<AdapterCommand>>,
    /// Receiver for parsed market data messages.
    recv_rx: Option<mpsc::Receiver<MarketDataMessage>>,
    /// Whether the adapter is currently connected.
    connected: bool,
    /// Active subscriptions persisted across reconnects.
    active_subscriptions: Vec<(String, String)>, // (channel, inst_id)
}

impl OkxMarketDataAdapter {
    /// Create a new adapter.
    ///
    /// `symbol` is the OKX instrument ID (e.g., "BTC-USDT").
    pub fn new(symbol: &str) -> Self {
        Self {
            ws_url: "wss://ws.okx.com:8443/ws/v5/public".to_string(),
            cmd_tx: None,
            recv_rx: None,
            connected: false,
            active_subscriptions: vec![("trades".to_string(), symbol.to_string())],
        }
    }

    /// Compute exponential backoff: 1s, 2s, 4s, ..., max 60s.
    fn compute_backoff(attempts: u32) -> u64 {
        let base = 1000u64;
        let exp = attempts.min(6);
        (base * (1u64 << exp)).min(60_000)
    }
}

// ---------------------------------------------------------------------------
// MarketDataAdapter trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl MarketDataAdapter for OkxMarketDataAdapter {
    /// Connect to OKX WebSocket endpoint and spawn the receive loop with reconnection.
    async fn connect(&mut self) -> Result<(), WsError> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<AdapterCommand>(32);
        let (recv_tx, recv_rx) = mpsc::channel::<MarketDataMessage>(64);
        let ws_url = self.ws_url.clone();
        let active_subscriptions: Vec<(String, String)> =
            std::mem::take(&mut self.active_subscriptions);

        tokio::spawn(async move {
            Self::receive_loop(ws_url, cmd_rx, recv_tx, active_subscriptions).await;
        });

        self.cmd_tx = Some(cmd_tx);
        self.recv_rx = Some(recv_rx);
        self.connected = true;
        Ok(())
    }

    /// Gracefully close the connection.
    async fn close(&mut self) -> Result<(), WsError> {
        if let Some(tx) = self.cmd_tx.take() {
            let _ = tx.send(AdapterCommand::Close).await;
        }
        self.recv_rx = None;
        self.connected = false;
        Ok(())
    }

    /// Receive the next normalized market data message.
    async fn recv(&mut self) -> Result<MarketDataMessage, WsError> {
        if let Some(rx) = &mut self.recv_rx {
            rx.recv().await.ok_or(WsError::ConnectionClosed)
        } else {
            Err(WsError::NotConnected)
        }
    }

    /// Subscribe to trade stream for a given instrument ID (e.g. "BTC-USDT").
    async fn subscribe_trades(&mut self, inst_id: &str) -> Result<(), WsError> {
        let channel = "trades".to_string();
        let inst_id_upper = inst_id.to_uppercase().replace("-USDT", "-USDT");
        if let Some(tx) = &self.cmd_tx {
            let _ = tx
                .send(AdapterCommand::Subscribe {
                    channel: channel.clone(),
                    inst_id: inst_id_upper.clone(),
                })
                .await;
            self.active_subscriptions.push((channel, inst_id_upper));
        }
        Ok(())
    }

    /// Unsubscribe from trade stream for a given instrument ID.
    async fn unsubscribe_trades(&mut self, inst_id: &str) -> Result<(), WsError> {
        let inst_id_upper = inst_id.to_uppercase().replace("-USDT", "-USDT");
        if let Some(tx) = &self.cmd_tx {
            let _ = tx
                .send(AdapterCommand::Unsubscribe {
                    channel: "trades".to_string(),
                    inst_id: inst_id_upper.clone(),
                })
                .await;
        }
        self.active_subscriptions
            .retain(|(c, i)| !(c == "trades" && i == &inst_id_upper));
        Ok(())
    }

    /// Reconnect with exponential backoff, re-subscribing to all active streams.
    async fn reconnect(&mut self) -> Result<(), WsError> {
        self.close().await?;
        self.connect().await
    }

    fn is_connected(&self) -> bool {
        self.connected
    }
}

// ---------------------------------------------------------------------------
// Internal receive loop with reconnection
// ---------------------------------------------------------------------------

impl OkxMarketDataAdapter {
    /// Receive loop: maintains connection, handles reconnection with exponential backoff.
    ///
    /// Persists active subscriptions across reconnects. Exits on `AdapterCommand::Close`.
    async fn receive_loop(
        ws_url: String,
        mut cmd_rx: mpsc::Receiver<AdapterCommand>,
        recv_tx: mpsc::Sender<MarketDataMessage>,
        active_subscriptions: Vec<(String, String)>,
    ) {
        let subscriptions = std::sync::Arc::new(tokio::sync::Mutex::new(active_subscriptions));
        let mut attempts: u32 = 0;

        loop {
            // Check shutdown flag before connecting
            if cmd_rx.is_closed() {
                break;
            }

            let result = Self::ws_session(&ws_url, &subscriptions, &recv_tx).await;

            match result {
                Ok(()) => {
                    // Normal close — reset backoff, exit loop
                    break;
                }
                Err(WsError::ConnectionClosed) => {
                    // Unexpected disconnect — apply exponential backoff, retry
                    let backoff_ms = Self::compute_backoff(attempts);
                    attempts = attempts.saturating_add(1);
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(_) => {
                    // Other errors (parse, send, etc.) — retry with backoff
                    let backoff_ms = Self::compute_backoff(attempts);
                    attempts = attempts.saturating_add(1);
                    tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
            }
        }
    }

    /// Single WebSocket session: connect, handle messages until disconnect.
    async fn ws_session(
        ws_url: &str,
        subscriptions: &std::sync::Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
        recv_tx: &mpsc::Sender<MarketDataMessage>,
    ) -> Result<(), WsError> {
        let (ws, _) = connect_async(ws_url)
            .await
            .map_err(|e| WsError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws.split();

        // Send initial subscriptions.
        let subs: Vec<(String, String)> = {
            let guard = subscriptions.lock().await;
            guard.clone()
        };
        for (channel, inst_id) in &subs {
            let sub = serde_json::json!({
                "op": "subscribe",
                "args": [{
                    "channel": channel,
                    "instId": inst_id
                }]
            });
            let msg =
                serde_json::to_string(&sub).map_err(|e| WsError::ParseError(e.to_string()))?;
            write
                .send(Message::Text(msg.into()))
                .await
                .map_err(|e| WsError::SendFailed(e.to_string()))?;
        }

        // Message loop.
        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Text(text) => {
                                    // Handle ping.
                                    if Self::handle_ping(&text).is_some() {
                                        let pong = r#"{"op":"pong"}"#.to_string();
                                        if write.send(Message::Text(pong.into())).await.is_err() {
                                            return Err(WsError::ConnectionClosed);
                                        }
                                        continue;
                                    }

                                    // Parse trade messages.
                                    if let Some(market_msg) = Self::parse_message(&text) {
                                        if recv_tx.send(market_msg).await.is_err() {
                                            return Ok(());
                                        }
                                    }
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
            }
        }
    }

    /// Check if a text message is a ping from OKX server.
    /// Returns `Some(())` if it was a ping, `None` otherwise.
    fn handle_ping(text: &str) -> Option<()> {
        let json: serde_json::Value = serde_json::from_str(text).ok()?;
        if json.get("op")?.as_str()? == "ping" {
            Some(())
        } else {
            None
        }
    }

    /// Parse a raw OKX WebSocket JSON text message into a `MarketDataMessage`.
    fn parse_message(raw: &str) -> Option<MarketDataMessage> {
        let json: serde_json::Value = serde_json::from_str(raw).ok()?;

        // Check if this is an error response
        if let Some(op) = json.get("op").and_then(|v| v.as_str()) {
            if op == "error" || op == "subscribe" {
                // Check for subscribe error
                if let Some(args) = json.get("args").and_then(|v| v.as_array()) {
                    if let Some(first) = args.first() {
                        if let Some(code) = first.get("code").and_then(|v| v.as_str()) {
                            if code != "0" {
                                // Subscribe error — log and return Unknown
                                return Some(MarketDataMessage::Unknown(format!(
                                    "subscribe_error:{}",
                                    code
                                )));
                            }
                        }
                    }
                }
            }
        }

        let data = json.get("data")?.as_array()?;
        let arg = json.get("arg")?;
        let channel = arg.get("channel")?.as_str()?;
        if channel != "trades" {
            return Some(MarketDataMessage::Unknown(channel.to_string()));
        }

        let inst_id = arg.get("instId")?.as_str()?;

        // OKX sends multiple trades per message — emit one at a time
        for trade in data {
            if let Some(norm) = Self::parse_trade_item(inst_id, trade) {
                return Some(MarketDataMessage::Trade(norm));
            }
        }

        Some(MarketDataMessage::Unknown(channel.to_string()))
    }

    /// Parse a single trade item from OKX trade data.
    fn parse_trade_item(
        symbol: &str,
        trade: &serde_json::Value,
    ) -> Option<NormalizedTrade> {
        let price = trade.get("price")?.as_str()?.parse::<f64>().ok()?;
        let size = trade.get("size")?.as_str()?.parse::<f64>().ok()?;
        let side_str = trade.get("side")?.as_str()?;
        let ts = trade.get("ts")?.as_str()?.parse::<u64>().ok()?;
        let trade_id = trade.get("tradeId")?.as_str()?;

        let timestamp_ns = ts * 1_000_000;
        let instrument_id = fnv1a_hash(symbol.as_bytes());

        // OKX side: "buy" (aggressor buy) or "sell"
        let side = if side_str == "buy" { 0 } else { 1 };

        Some(NormalizedTrade {
            instrument_id,
            symbol: symbol.to_string(),
            timestamp_ns,
            price,
            size,
            side,
            trade_id: trade_id.parse::<u64>().unwrap_or(0),
        })
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_trade_message() {
        let json = r#"{
            "arg": {"channel": "trades", "instId": "BTC-USDT"},
            "data": [
                {
                    "instId": "BTC-USDT",
                    "price": "50000.00",
                    "size": "0.01",
                    "side": "buy",
                    "tradeId": "12345",
                    "ts": "1672515782135"
                }
            ]
        }"#;

        let msg = OkxMarketDataAdapter::parse_message(json);
        assert!(msg.is_some());

        if let Some(MarketDataMessage::Trade(trade)) = msg {
            assert_eq!(trade.symbol, "BTC-USDT");
            assert_eq!(trade.price, 50000.00);
            assert_eq!(trade.size, 0.01);
            assert_eq!(trade.side, 0); // buy
            assert_eq!(trade.trade_id, 12345);
            assert_eq!(trade.timestamp_ns, 1672515782135u64 * 1_000_000);
        } else {
            panic!("Expected Trade variant");
        }
    }

    #[test]
    fn test_parse_sell_trade() {
        let json = r#"{
            "arg": {"channel": "trades", "instId": "ETH-USDT"},
            "data": [
                {
                    "instId": "ETH-USDT",
                    "price": "3000.50",
                    "size": "1.5",
                    "side": "sell",
                    "tradeId": "67890",
                    "ts": "1672515783000"
                }
            ]
        }"#;

        let msg = OkxMarketDataAdapter::parse_message(json);
        assert!(msg.is_some());

        if let Some(MarketDataMessage::Trade(trade)) = msg {
            assert_eq!(trade.symbol, "ETH-USDT");
            assert_eq!(trade.price, 3000.50);
            assert_eq!(trade.size, 1.5);
            assert_eq!(trade.side, 1); // sell
        } else {
            panic!("Expected Trade variant");
        }
    }

    #[test]
    fn test_handle_ping() {
        assert!(OkxMarketDataAdapter::handle_ping(r#"{"op":"ping"}"#).is_some());
        assert!(OkxMarketDataAdapter::handle_ping(r#"{"op":"subscribe"}"#).is_none());
        assert!(OkxMarketDataAdapter::handle_ping(r#"not json"#).is_none());
    }

    #[test]
    fn test_backoff_computation() {
        assert_eq!(OkxMarketDataAdapter::compute_backoff(0), 1000);
        assert_eq!(OkxMarketDataAdapter::compute_backoff(1), 2000);
        assert_eq!(OkxMarketDataAdapter::compute_backoff(2), 4000);
        assert_eq!(OkxMarketDataAdapter::compute_backoff(6), 60000);
        assert_eq!(OkxMarketDataAdapter::compute_backoff(10), 60000);
    }

    #[test]
    fn test_adapter_creation() {
        let adapter = OkxMarketDataAdapter::new("BTC-USDT");
        assert!(!adapter.is_connected());
    }
}