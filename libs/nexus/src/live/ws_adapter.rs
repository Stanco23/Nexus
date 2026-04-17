//! Binance WebSocket adapter for user data streams.
//!
//! Handles execution reports (fills), balance updates, order cancellations.
//! Auto-reconnect with exponential backoff (1s, 2s, 4s, max 60s).
//!
//! Nautilus source: `adapters/binance/websocket/user.py`

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_tungstenite::{connect_async, tungstenite::Message};

use crate::messages::OrderSide;

// =============================================================================
// WebSocket Message Types
// =============================================================================

/// Incoming WebSocket message from Binance.
#[derive(Debug, Clone)]
pub enum WsMessage {
    /// Order execution report (fill, partial fill, accepted, cancelled, rejected).
    ExecutionReport(ExecutionReport),
    /// Balance update event.
    BalanceUpdate(BalanceUpdate),
    /// Account position update (outboundAccountPosition).
    AccountUpdate(AccountUpdate),
    /// Order list status update.
    ListStatus(ListStatus),
    /// Listen key expired — need to reconnect.
    ListenKeyExpired,
    /// Unknown or unhandled event type.
    Unknown(String),
}

/// Binance execution report (executionReport event).
#[derive(Debug, Clone)]
pub struct ExecutionReport {
    /// Event type: "executionReport"
    pub event_type: String,
    /// Event time (milliseconds).
    pub event_time: u64,
    /// Symbol (e.g., "BTCUSDT").
    pub symbol: String,
    /// Client order ID assigned by the client.
    pub client_order_id: String,
    /// Binance-assigned order ID.
    pub venue_order_id: u64,
    /// Order side: "BUY" or "SELL".
    pub side: OrderSide,
    /// Order type: "MARKET", "LIMIT", etc.
    pub order_type: String,
    /// Time in force: "GTC", "IOC", "FOK", etc.
    pub time_in_force: String,
    /// Original quantity.
    pub quantity: String,
    /// Original price.
    pub price: String,
    /// Stop price.
    pub stop_price: String,
    /// Iceberg quantity.
    pub iceberg_qty: String,
    /// Last filled quantity.
    pub last_fill_qty: String,
    /// Accumulated filled quantity.
    pub accumulated_qty: String,
    /// Last filled price.
    pub last_fill_price: String,
    /// Commission amount.
    pub commission: Option<String>,
    /// Commission asset.
    pub commission_asset: Option<String>,
    /// Trade time (milliseconds).
    pub trade_time: u64,
    /// Trade ID.
    pub trade_id: u64,
    /// Is order on the book?
    pub is_on_book: bool,
    /// Is trade the maker side?
    pub is_maker: bool,
    /// Order status: "NEW", "PARTIALLY_FILLED", "FILLED", "CANCELED", "REJECTED".
    pub order_status: String,
    /// Reject reason code (if rejected).
    pub reject_reason: String,
    /// Order ID assigned by the exchange.
    pub exchange_order_id: u64,
}

/// Balance update event.
#[derive(Debug, Clone)]
pub struct BalanceUpdate {
    /// Asset (e.g., "USDT").
    pub asset: String,
    /// Balance after deduction (encoded as string).
    pub balance: String,
    /// Whether this is a deposit (true) or withdrawal (false).
    pub is_deposit: bool,
}

/// Account position update (outboundAccountPosition event).
#[derive(Debug, Clone)]
pub struct AccountUpdate {
    /// Event time (milliseconds).
    pub event_time: u64,
    /// Balances updated.
    pub balances: Vec<BalanceInfo>,
}

/// A single balance entry.
#[derive(Debug, Clone)]
pub struct BalanceInfo {
    pub asset: String,
    pub free: String,
    pub locked: String,
}

/// Order list status update.
#[derive(Debug, Clone)]
pub struct ListStatus {
    pub list_status_type: String,
    pub list_order_status: String,
    pub list_client_order_id: String,
    pub list_id: String,
    pub symbol: String,
    pub orders: Vec<ListOrder>,
}

#[derive(Debug, Clone)]
pub struct ListOrder {
    pub symbol: String,
    pub order_id: u64,
    pub client_order_id: String,
}

// =============================================================================
// WebSocket Adapter
// =============================================================================

/// Binance WebSocket adapter for user data streams.
/// Connects to `wss://stream.binance.com:9443/ws/<listen_key>`.
pub struct BinanceWsAdapter {
    #[allow(dead_code)]
    listen_key: String,
    ws_url: String,
    /// Channel to send outgoing messages (subscribe/unsubscribe).
    send_tx: Option<mpsc::Sender<WsOutgoing>>,
    /// Channel to receive incoming messages.
    recv_rx: Option<mpsc::Receiver<WsMessage>>,
    /// Flag indicating if connected.
    connected: bool,
}

/// Outgoing WebSocket messages (subscribe/unsubscribe).
enum WsOutgoing {
    #[allow(dead_code)]
    Subscribe(Vec<String>),
    #[allow(dead_code)]
    Unsubscribe(Vec<String>),
    #[allow(dead_code)]
    Ping,
}

impl BinanceWsAdapter {
    /// Create a new adapter. Does NOT connect.
    ///
    /// `listen_key` — the user data stream listen key from `POST /api/v3/userDataStream`.
    /// `ws_url` — base WS URL, typically `wss://stream.binance.com:9443/ws`.
    pub fn new(listen_key: String, ws_url: &str) -> Self {
        let ws_url = format!("{}/{}", ws_url.trim_end_matches('/'), listen_key);
        Self {
            listen_key,
            ws_url,
            send_tx: None,
            recv_rx: None,
            connected: false,
        }
    }

    /// Establish WebSocket connection and subscribe to user data streams.
    /// Spawns receive_loop which handles reconnection with exponential backoff.
    pub async fn connect(&mut self) -> Result<(), WsError> {
        let (send_tx, send_rx) = mpsc::channel::<WsOutgoing>(32);
        let (recv_tx, recv_rx) = mpsc::channel::<WsMessage>(64);
        let ws_url = self.ws_url.clone();

        tokio::spawn(async move {
            Self::receive_loop(ws_url, send_rx, recv_tx).await;
        });

        self.send_tx = Some(send_tx);
        self.recv_rx = Some(recv_rx);
        self.connected = true;
        Ok(())
    }

    /// Receive loop: connects, subscribes, handles messages, reconnects on disconnect.
    async fn receive_loop(
        ws_url: String,
        mut send_rx: mpsc::Receiver<WsOutgoing>,
        mut recv_tx: mpsc::Sender<WsMessage>,
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

    /// Single connection receive loop: connects, subscribes, handles bidirectional messages.
    async fn ws_receive_loop(
        ws_url: &str,
        send_rx: &mut mpsc::Receiver<WsOutgoing>,
        recv_tx: &mut mpsc::Sender<WsMessage>,
    ) -> Result<(), WsError> {
        let (ws_stream, _) = connect_async(ws_url)
            .await
            .map_err(|e| WsError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        // Subscribe to user data stream
        let subscribe_msg = serde_json::json!({
            "method": "SUBSCRIBE",
            "params": [
                "executionReport",
                "balanceUpdate",
                "outboundAccountPosition"
            ],
            "id": 1
        });
        write
            .send(Message::Text(subscribe_msg.to_string().into()))
            .await
            .map_err(|e| WsError::SendFailed(e.to_string()))?;

        // Wait for subscribe ack
        if let Some(msg) = read.next().await {
            let text = msg.map_err(|e| WsError::RecvFailed(e.to_string()))?;
            let text = text.into_text().map_err(|e| WsError::RecvFailed(e.to_string()))?;
            if !text.contains("\"result\":null") && !text.contains("\"result\": null") {
                if text.contains("\"error\"") {
                    return Err(WsError::SubscribeFailed(text.to_string()));
                }
            }
        }

        // Message loop — handle incoming WS messages and outgoing commands
        loop {
            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Text(text) => {
                                    if text.contains("eventStreamTerminated") {
                                        return Err(WsError::ConnectionClosed);
                                    }
                                    if let Some(ws_msg) = Self::parse_message(&text) {
                                        let _ = recv_tx.send(ws_msg).await;
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
                outgoing = send_rx.recv() => {
                    match outgoing {
                        Some(WsOutgoing::Unsubscribe(streams)) => {
                            let msg = serde_json::json!({
                                "method": "UNSUBSCRIBE",
                                "params": streams,
                                "id": 2
                            });
                            let _ = write.send(Message::Text(msg.to_string().into())).await;
                        }
                        Some(WsOutgoing::Ping) => {
                            // Keepalive ping — flush the write side
                            let _ = write.flush().await;
                        }
                        Some(WsOutgoing::Subscribe(_)) => {
                            // Subscriptions handled at connect time
                        }
                        None => {}
                    }
                }
            }
        }
    }

    /// Compute exponential backoff: 1s, 2s, 4s, ..., max 60s.
    fn compute_backoff(attempts: u32) -> u64 {
        let base = 1000u64; // 1 second
        let exp = attempts.min(6); // max 2^6 = 64s, cap at 60s
        base * (1 << exp).min(60)
    }

    /// Receive the next WebSocket message.
    /// Returns `None` if the connection is closed.
    pub async fn recv(&mut self) -> Result<WsMessage, WsError> {
        if let Some(rx) = &mut self.recv_rx {
            rx.recv()
                .await
                .ok_or(WsError::ConnectionClosed)
        } else {
            Err(WsError::NotConnected)
        }
    }

    /// Gracefully close the WebSocket connection.
    pub async fn close(&mut self) -> Result<(), WsError> {
        if let Some(tx) = self.send_tx.take() {
            let _ = tx.send(WsOutgoing::Unsubscribe(vec![
                "executionReport".to_string(),
                "balanceUpdate".to_string(),
                "outboundAccountPosition".to_string(),
            ])).await;
        }
        self.connected = false;
        Ok(())
    }

    /// Parse a raw Binance WebSocket JSON message into a `WsMessage`.
    fn parse_message(raw: &str) -> Option<WsMessage> {
        // Check which event type
        let event_type = serde_json::from_str::<serde_json::Value>(raw)
            .ok()?
            .get("e")
            .and_then(|v| v.as_str())?
            .to_string();

        match event_type.as_str() {
            "executionReport" => {
                let report = serde_json::from_str::<ExecutionReportJson>(raw).ok()?;
                Some(WsMessage::ExecutionReport(ExecutionReport {
                    event_type: report.e,
                    event_time: report.E,
                    symbol: report.s,
                    client_order_id: report.c,
                    venue_order_id: report.i,
                    side: if report.S == "BUY" {
                        OrderSide::Buy
                    } else {
                        OrderSide::Sell
                    },
                    order_type: report.o,
                    time_in_force: report.f,
                    quantity: report.q,
                    price: report.p,
                    stop_price: report.P,
                    iceberg_qty: report.F,
                    last_fill_qty: report.l,
                    accumulated_qty: report.z,
                    last_fill_price: report.L,
                    commission: report.n,
                    commission_asset: report.N,
                    trade_time: report.T,
                    trade_id: report.t,
                    is_on_book: report.w,
                    is_maker: report.m,
                    order_status: report.x,
                    reject_reason: report.r,
                    exchange_order_id: report.i,
                }))
            }
            "balanceUpdate" => {
                let update = serde_json::from_str::<BalanceUpdateJson>(raw).ok()?;
                Some(WsMessage::BalanceUpdate(BalanceUpdate {
                    asset: update.a,
                    balance: update.d,
                    is_deposit: update.D,
                }))
            }
            "outboundAccountPosition" => {
                let update = serde_json::from_str::<AccountUpdateJson>(raw).ok()?;
                Some(WsMessage::AccountUpdate(AccountUpdate {
                    event_time: update.E,
                    balances: update
                        .B
                        .into_iter()
                        .map(|b| BalanceInfo {
                            asset: b.a,
                            free: b.f,
                            locked: b.l,
                        })
                        .collect(),
                }))
            }
            "listStatus" => {
                let status = serde_json::from_str::<ListStatusJson>(raw).ok()?;
                Some(WsMessage::ListStatus(ListStatus {
                    list_status_type: status.s,
                    list_order_status: status.l,
                    list_client_order_id: status.c,
                    list_id: status.i,
                    symbol: status.Symbol,
                    orders: status
                        .o
                        .into_iter()
                        .map(|o| ListOrder {
                            symbol: o.s,
                            order_id: o.i,
                            client_order_id: o.c,
                        })
                        .collect(),
                }))
            }
            "listenKeyExpired" => Some(WsMessage::ListenKeyExpired),
            _ => Some(WsMessage::Unknown(event_type)),
        }
    }
}

// =============================================================================
// JSON Parsing Types (Binance format)
// =============================================================================

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[allow(non_snake_case)]
struct ExecutionReportJson {
    #[serde(rename = "e")]
    e: String,
    #[serde(rename = "E")]
    E: u64,
    #[serde(rename = "s")]
    s: String,
    #[serde(rename = "c")]
    c: String,
    #[serde(rename = "S")]
    S: String,
    #[serde(rename = "o")]
    o: String,
    #[serde(rename = "f")]
    f: String,
    #[serde(rename = "q")]
    q: String,
    #[serde(rename = "p")]
    p: String,
    #[serde(rename = "P")]
    P: String,
    #[serde(rename = "F")]
    F: String,
    #[serde(rename = "g")]
    g: i64,
    #[serde(rename = "C")]
    C: String,
    #[serde(rename = "x")]
    x: String,
    #[serde(rename = "X")]
    X: String,
    #[serde(rename = "r")]
    r: String,
    #[serde(rename = "i")]
    i: u64,
    #[serde(rename = "l")]
    l: String,
    #[serde(rename = "z")]
    z: String,
    #[serde(rename = "L")]
    L: String,
    #[serde(rename = "n")]
    n: Option<String>,
    #[serde(rename = "N")]
    N: Option<String>,
    #[serde(rename = "T")]
    T: u64,
    #[serde(rename = "t")]
    t: u64,
    #[serde(rename = "I")]
    I: i64,
    #[serde(rename = "w")]
    w: bool,
    #[serde(rename = "m")]
    m: bool,
    #[serde(rename = "R")]
    R: bool,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[allow(non_snake_case)]
struct BalanceUpdateJson {
    #[serde(rename = "e")]
    e: String,
    #[serde(rename = "E")]
    E: u64,
    #[serde(rename = "a")]
    a: String,
    #[serde(rename = "d")]
    d: String,
    #[serde(rename = "T")]
    T: u64,
    #[serde(rename = "D")]
    D: bool,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[allow(non_snake_case)]
struct AccountUpdateJson {
    #[serde(rename = "e")]
    e: String,
    #[serde(rename = "E")]
    E: u64,
    #[serde(rename = "u")]
    u: u64,
    #[serde(rename = "B")]
    B: Vec<BalanceJson>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BalanceJson {
    #[serde(rename = "a")]
    a: String,
    #[serde(rename = "f")]
    f: String,
    #[serde(rename = "l")]
    l: String,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
#[allow(non_snake_case)]
struct ListStatusJson {
    #[serde(rename = "e")]
    e: String,
    #[serde(rename = "E")]
    E: u64,
    #[serde(rename = "s")]
    s: String,
    #[serde(rename = "g")]
    g: i64,
    #[serde(rename = "c")]
    c: String,
    #[serde(rename = "i")]
    i: String,
    #[serde(rename = "l")]
    l: String,
    #[serde(rename = "o")]
    o: Vec<ListOrderJson>,
    #[serde(rename = "Symbol")]
    Symbol: String,
}

#[derive(Debug, Deserialize)]
struct ListOrderJson {
    #[serde(rename = "s")]
    s: String,
    #[serde(rename = "i")]
    i: u64,
    #[serde(rename = "c")]
    c: String,
}

// =============================================================================
// Error Types
// =============================================================================

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

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_execution_report_json() {
        let json = r#"{
            "e": "executionReport",
            "E": 1700000000000,
            "s": "BTCUSDT",
            "c": "clientOrderId123",
            "S": "BUY",
            "o": "MARKET",
            "f": "GTC",
            "q": "0.10000000",
            "p": "50000.00000000",
            "P": "0.00000000",
            "F": "0.00000000",
            "g": 1,
            "C": "origClientOrderId",
            "x": "NEW",
            "X": "NEW",
            "r": "NONE",
            "i": 98765,
            "l": "0.10000000",
            "z": "0.10000000",
            "L": "50000.00000000",
            "n": "2.50000000",
            "N": "USDT",
            "T": 1700000000000,
            "t": 12345,
            "I": 0,
            "w": true,
            "m": false,
            "R": false
        }"#;

        let msg = BinanceWsAdapter::parse_message(json);
        assert!(msg.is_some());

        if let Some(WsMessage::ExecutionReport(report)) = msg {
            assert_eq!(report.symbol, "BTCUSDT");
            assert_eq!(report.client_order_id, "clientOrderId123");
            assert!(matches!(report.side, OrderSide::Buy));
            assert_eq!(report.order_status, "NEW");
            assert_eq!(report.trade_id, 12345);
        } else {
            panic!("Expected ExecutionReport variant");
        }
    }

    #[tokio::test]
    async fn test_ws_adapter_creation() {
        let adapter = BinanceWsAdapter::new(
            "test_listen_key".to_string(),
            "wss://stream.binance.com:9443/ws",
        );
        assert!(!adapter.connected);
    }

    #[test]
    fn test_backoff_computation() {
        assert_eq!(BinanceWsAdapter::compute_backoff(0), 1000);
        assert_eq!(BinanceWsAdapter::compute_backoff(1), 2000);
        assert_eq!(BinanceWsAdapter::compute_backoff(2), 4000);
        assert_eq!(BinanceWsAdapter::compute_backoff(6), 60000);
        assert_eq!(BinanceWsAdapter::compute_backoff(10), 60000); // capped
    }
}
