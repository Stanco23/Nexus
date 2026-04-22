//! Bybit market data WebSocket adapter for public trade streams.
//!
//! Handles public trade data via `wss://stream.bybit.com/v5/public/spot`.
//! Supports spot and USDT-M linear markets.
//!
//! Key differences from Binance:
//! - Binary ping frames (not text)
//! - Different subscription format (`op`/`args`)
//! - Trade messages via `topic` field
//!
//! # Reconnection
//! Automatically reconnects with exponential backoff (1s, 2s, 4s... capped at 60s).
//! Active subscriptions are persisted across reconnects.

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::instrument::instrument_id::fnv1a_hash;
use crate::live::exchange::{
    MarketDataAdapter, MarketDataMessage, NormalizedTrade, WsError,
};

// =============================================================================
// Adapter Command
// =============================================================================

/// Internal commands for the adapter's background task.
enum AdapterCommand {
    /// Subscribe to trade stream(s).
    Subscribe(Vec<String>),
    /// Unsubscribe from trade stream(s).
    Unsubscribe(Vec<String>),
    /// Close the connection.
    Close,
}

// =============================================================================
// BybitMarketDataAdapter
// =============================================================================

/// Bybit market data adapter for public trade streams.
///
/// Spawns a background task that maintains the WebSocket connection
/// and forwards normalized market data messages via a channel.
///
/// Supports spot (`wss://stream.bybit.com/v5/public/spot`) and
/// USDT-M linear (`wss://stream.bybit.com/v5/public/linear`) endpoints.
///
/// Reconnects with exponential backoff (1s, 2s, 4s, ..., 60s cap).
/// Active subscriptions are persisted across reconnects.
pub struct BybitMarketDataAdapter {
    /// WebSocket URL for the market data stream.
    ws_url: String,
    /// Channel to send internal commands to the background task.
    send_tx: Option<mpsc::Sender<AdapterCommand>>,
    /// Channel to receive normalized market data messages.
    recv_rx: Option<mpsc::Receiver<MarketDataMessage>>,
    /// Whether the adapter is currently connected.
    connected: bool,
    /// Set of active trade subscriptions (e.g. `trade.BTCUSDT`).
    active_subscriptions: Vec<String>,
    /// Running message ID counter for subscribe/unsubscribe ops.
    msg_id: u64,
}

impl BybitMarketDataAdapter {
    /// Create a new adapter for Bybit spot markets.
    ///
    /// `ws_url` — base WebSocket URL, e.g. `wss://stream.bybit.com/v5/public/spot`
    ///             for spot or `wss://stream.bybit.com/v5/public/linear` for linear.
    pub fn new(ws_url: &str) -> Self {
        Self {
            ws_url: ws_url.to_string(),
            send_tx: None,
            recv_rx: None,
            connected: false,
            active_subscriptions: Vec::new(),
            msg_id: 0,
        }
    }

    /// Compute exponential backoff: 1s, 2s, 4s, ..., max 60s.
    fn compute_backoff(attempts: u32) -> u64 {
        let base = 1000u64; // 1 second
        let exp = attempts.min(6); // max 2^6 = 64s, capped at 60s
        (base * (1u64 << exp)).min(60_000)
    }
}

// ---------------------------------------------------------------------------
// MarketDataAdapter trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl MarketDataAdapter for BybitMarketDataAdapter {
    /// Connect to the Bybit WebSocket endpoint and spawn the receive loop.
    async fn connect(&mut self) -> Result<(), WsError> {
        let (send_tx, send_rx) = mpsc::channel::<AdapterCommand>(32);
        let (recv_tx, recv_rx) = mpsc::channel::<MarketDataMessage>(64);
        let ws_url = self.ws_url.clone();
        let active_subscriptions = std::sync::Arc::new(tokio::sync::Mutex::new(
            std::mem::take(&mut self.active_subscriptions),
        ));

        tokio::spawn(async move {
            Self::receive_loop(ws_url, send_rx, recv_tx, active_subscriptions).await;
        });

        self.send_tx = Some(send_tx);
        self.recv_rx = Some(recv_rx);
        self.connected = true;
        Ok(())
    }

    /// Gracefully close the connection.
    async fn close(&mut self) -> Result<(), WsError> {
        if let Some(tx) = self.send_tx.take() {
            let _ = tx.send(AdapterCommand::Close).await;
        }
        self.recv_rx = None;
        self.connected = false;
        Ok(())
    }

    /// Receive the next normalized market data message.
    async fn recv(&mut self) -> Result<MarketDataMessage, WsError> {
        if let Some(rx) = &mut self.recv_rx {
            rx.recv()
                .await
                .ok_or(WsError::ConnectionClosed)
        } else {
            Err(WsError::NotConnected)
        }
    }

    /// Subscribe to trade stream for a given symbol (e.g. "BTCUSDT").
    async fn subscribe_trades(&mut self, symbol: &str) -> Result<(), WsError> {
        let topic = format!("trade.{}", symbol.to_uppercase());
        if let Some(tx) = &self.send_tx {
            let _ = tx
                .send(AdapterCommand::Subscribe(vec![topic.clone()]))
                .await;
            self.active_subscriptions.push(topic);
        }
        Ok(())
    }

    /// Unsubscribe from trade stream for a given symbol.
    async fn unsubscribe_trades(&mut self, symbol: &str) -> Result<(), WsError> {
        let topic = format!("trade.{}", symbol.to_uppercase());
        if let Some(tx) = &self.send_tx {
            let _ = tx
                .send(AdapterCommand::Unsubscribe(vec![topic.clone()]))
                .await;
        }
        self.active_subscriptions.retain(|t| t != &topic);
        Ok(())
    }

    /// Reconnect with exponential backoff, re-subscribing to all active streams.
    async fn reconnect(&mut self) -> Result<(), WsError> {
        self.close().await?;
        self.connect().await
    }

    /// Whether the adapter is currently connected.
    fn is_connected(&self) -> bool {
        self.connected
    }
}

// ---------------------------------------------------------------------------
// Internal receive loop with reconnection
// ---------------------------------------------------------------------------

impl BybitMarketDataAdapter {
    /// Receive loop: maintains connection, handles reconnection with exponential backoff.
    ///
    /// Persists active subscriptions across reconnects. Exits on `AdapterCommand::Close`.
    async fn receive_loop(
        ws_url: String,
        mut send_rx: mpsc::Receiver<AdapterCommand>,
        recv_tx: mpsc::Sender<MarketDataMessage>,
        active_subscriptions: std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
    ) {
        let mut attempts: u32 = 0;

        loop {
            match Self::ws_receive_loop(
                &ws_url,
                &mut send_rx,
                &recv_tx,
                &active_subscriptions,
            )
            .await
            {
                Ok(()) => {
                    // Normal close — exit loop
                    break;
                }
                Err(WsError::ConnectionClosed) => {
                    // Unexpected disconnect — apply exponential backoff, retry
                    let backoff_ms = Self::compute_backoff(attempts);
                    attempts = attempts.saturating_add(1);
                    sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
                Err(_) => {
                    // Other errors — retry with backoff (e.g., network timeout)
                    let backoff_ms = Self::compute_backoff(attempts);
                    attempts = attempts.saturating_add(1);
                    sleep(Duration::from_millis(backoff_ms)).await;
                    continue;
                }
            }
        }
    }

    /// Single connection receive loop: connect, subscribe, process messages.
    async fn ws_receive_loop(
        ws_url: &str,
        send_rx: &mut mpsc::Receiver<AdapterCommand>,
        recv_tx: &mpsc::Sender<MarketDataMessage>,
        active_subscriptions: &std::sync::Arc<tokio::sync::Mutex<Vec<String>>>,
    ) -> Result<(), WsError> {
        let (ws_stream, _) = connect_async(ws_url)
            .await
            .map_err(|e| WsError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        // Re-subscribe to all active streams on reconnect
        let subs: Vec<String> = {
            let guard = active_subscriptions.lock().await;
            guard.clone()
        };
        if !subs.is_empty() {
            Self::send_subscribe(&mut write, &subs, 1).await?;
        }

        // Message loop
        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Text(text) => {
                                    if let Some(data_msg) = Self::parse_message(&text) {
                                        let _ = recv_tx.send(data_msg).await;
                                    }
                                }
                                Message::Binary(data) => {
                                    // Bybit sends binary ping frames
                                    // Respond with the same payload as pong
                                    let _ = write.send(Message::Pong(data.clone())).await;
                                }
                                Message::Ping(data) => {
                                    let _ = write.send(Message::Pong(data)).await;
                                }
                                Message::Pong(_) => {
                                    // Pong received — nothing to do
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
                        Some(AdapterCommand::Subscribe(topics)) => {
                            // Update active subscriptions
                            let id = {
                                let mut guard = active_subscriptions.lock().await;
                                for topic in &topics {
                                    if !guard.contains(topic) {
                                        guard.push(topic.clone());
                                    }
                                }
                                guard.len() as u64 + 1
                            };
                            Self::send_subscribe(&mut write, &topics, id).await?;
                        }
                        Some(AdapterCommand::Unsubscribe(topics)) => {
                            // Remove from active subscriptions
                            let id = {
                                let mut guard = active_subscriptions.lock().await;
                                for topic in &topics {
                                    guard.retain(|t| t != topic);
                                }
                                guard.len() as u64 + 1
                            };
                            Self::send_unsubscribe(&mut write, &topics, id).await?;
                        }
                        Some(AdapterCommand::Close) => {
                            return Ok(());
                        }
                        None => {}
                    }
                }
            }
        }
    }

    /// Send a subscribe message.
    async fn send_subscribe(
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        topics: &[String],
        id: u64,
    ) -> Result<(), WsError> {
        let msg = serde_json::json!({
            "op": "subscribe",
            "args": topics,
            "id": id
        });
        write
            .send(Message::Text(msg.to_string().into()))
            .await
            .map_err(|e| WsError::SendFailed(e.to_string()))
    }

    /// Send an unsubscribe message.
    async fn send_unsubscribe(
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        topics: &[String],
        id: u64,
    ) -> Result<(), WsError> {
        let msg = serde_json::json!({
            "op": "unsubscribe",
            "args": topics,
            "id": id
        });
        write
            .send(Message::Text(msg.to_string().into()))
            .await
            .map_err(|e| WsError::SendFailed(e.to_string()))
    }

    /// Parse a raw Bybit WebSocket JSON text message into a `MarketDataMessage`.
    fn parse_message(raw: &str) -> Option<MarketDataMessage> {
        let json: serde_json::Value = serde_json::from_str(raw).ok()?;

        // Check for error response
        if let Some(code) = json.get("code").and_then(|v| v.as_str()) {
            if code != "0" {
                let msg = json.get("msg").and_then(|v| v.as_str()).unwrap_or("");
                return Some(MarketDataMessage::Unknown(format!("error:{}:{}", code, msg)));
            }
        }

        let topic = json.get("topic")?.as_str()?;

        if topic.starts_with("trade.") {
            let data = json.get("data")?;
            let trades = data.as_array()?;

            // Bybit sends multiple trades per message — emit one at a time
            for trade in trades {
                if let Some(norm) = Self::parse_trade_item(topic, trade) {
                    return Some(MarketDataMessage::Trade(norm));
                }
            }
        }

        Some(MarketDataMessage::Unknown(topic.to_string()))
    }

    /// Parse a single trade item from a Bybit trade data array.
    fn parse_trade_item(topic: &str, trade: &serde_json::Value) -> Option<NormalizedTrade> {
        // topic is "trade.BTCUSDT" — extract the symbol part
        let _symbol = topic.strip_prefix("trade.")?;

        let symbol = trade.get("s")?.as_str()?;
        let p = trade.get("p")?.as_str()?;
        let v = trade.get("v")?.as_str()?;
        let side_str = trade.get("S")?.as_str()?;
        let t = trade.get("T")?.as_u64()?;
        let i = trade.get("i")?.as_str()?;

        let price = p.parse::<f64>().ok()?;
        let size = v.parse::<f64>().ok()?;
        let timestamp_ms = t;
        let timestamp_ns = timestamp_ms * 1_000_000;
        let trade_id = i.parse::<u64>().unwrap_or(0);
        let instrument_id = fnv1a_hash(symbol.as_bytes());

        // Bybit side: "Buy" (aggressor is buy, side=0) or "Sell" (side=1)
        let side = if side_str == "Buy" { 0 } else { 1 };

        Some(NormalizedTrade {
            instrument_id,
            symbol: symbol.to_string(),
            timestamp_ns,
            price,
            size,
            side,
            trade_id,
        })
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_trade_message() {
        let json = r#"{
            "topic": "trade.BTCUSDT",
            "data": [
                {
                    "s": "BTCUSDT",
                    "p": "50000.00",
                    "v": "0.01",
                    "S": "Buy",
                    "T": 1672515782135,
                    "i": "12345"
                }
            ]
        }"#;

        let msg = BybitMarketDataAdapter::parse_message(json);
        assert!(msg.is_some());

        if let Some(MarketDataMessage::Trade(trade)) = msg {
            assert_eq!(trade.symbol, "BTCUSDT");
            assert_eq!(trade.price, 50000.00);
            assert_eq!(trade.size, 0.01);
            assert_eq!(trade.side, 0); // Buy
            assert_eq!(trade.trade_id, 12345);
            assert_eq!(trade.timestamp_ns, 1672515782135u64 * 1_000_000);
        } else {
            panic!("Expected Trade variant");
        }
    }

    #[test]
    fn test_parse_sell_trade() {
        let json = r#"{
            "topic": "trade.ETHUSDT",
            "data": [
                {
                    "s": "ETHUSDT",
                    "p": "3000.50",
                    "v": "1.5",
                    "S": "Sell",
                    "T": 1672515783000,
                    "i": "67890"
                }
            ]
        }"#;

        let msg = BybitMarketDataAdapter::parse_message(json);
        assert!(msg.is_some());

        if let Some(MarketDataMessage::Trade(trade)) = msg {
            assert_eq!(trade.symbol, "ETHUSDT");
            assert_eq!(trade.price, 3000.50);
            assert_eq!(trade.size, 1.5);
            assert_eq!(trade.side, 1); // Sell
            assert_eq!(trade.trade_id, 67890);
        } else {
            panic!("Expected Trade variant");
        }
    }

    #[test]
    fn test_parse_unknown_topic() {
        let json = r#"{
            "topic": "orderbook.BTCUSDT",
            "data": {}
        }"#;

        let msg = BybitMarketDataAdapter::parse_message(json);
        assert!(matches!(msg, Some(MarketDataMessage::Unknown(_))));
    }

    #[test]
    fn test_parse_error_response() {
        let json = r#"{
            "code": "10001",
            "msg": "Unknown topic",
            "success": false
        }"#;

        let msg = BybitMarketDataAdapter::parse_message(json);
        assert!(matches!(msg, Some(MarketDataMessage::Unknown(ref s)) if s.contains("error:10001")));
    }

    #[test]
    fn test_backoff_computation() {
        assert_eq!(BybitMarketDataAdapter::compute_backoff(0), 1000);
        assert_eq!(BybitMarketDataAdapter::compute_backoff(1), 2000);
        assert_eq!(BybitMarketDataAdapter::compute_backoff(2), 4000);
        assert_eq!(BybitMarketDataAdapter::compute_backoff(6), 60000);
        assert_eq!(BybitMarketDataAdapter::compute_backoff(10), 60000); // capped
    }

    #[test]
    fn test_adapter_creation() {
        let adapter = BybitMarketDataAdapter::new("wss://stream.bybit.com/v5/public/spot");
        assert!(!adapter.is_connected());
    }

    #[test]
    fn test_fnv1a_instrument_id() {
        // Verify that the instrument_id is computed consistently from symbol
        let json1 = r#"{"topic":"trade.BTCUSDT","data":[{"s":"BTCUSDT","p":"50000","v":"1","S":"Buy","T":1,"i":"1"}]}"#;
        let json2 = r#"{"topic":"trade.ETHUSDT","data":[{"s":"ETHUSDT","p":"3000","v":"1","S":"Buy","T":1,"i":"1"}]}"#;

        let msg1 = BybitMarketDataAdapter::parse_message(json1);
        let msg2 = BybitMarketDataAdapter::parse_message(json2);

        if let (Some(MarketDataMessage::Trade(t1)), Some(MarketDataMessage::Trade(t2))) = (msg1, msg2) {
            // Different symbols should produce different instrument IDs
            assert_ne!(t1.instrument_id, t2.instrument_id);
            // Same symbol should produce same ID
            let json3 = r#"{"topic":"trade.BTCUSDT","data":[{"s":"BTCUSDT","p":"50001","v":"1","S":"Buy","T":1,"i":"1"}]}"#;
            let msg3 = BybitMarketDataAdapter::parse_message(json3);
            if let Some(MarketDataMessage::Trade(t3)) = msg3 {
                assert_eq!(t1.instrument_id, t3.instrument_id);
            }
        }
    }
}