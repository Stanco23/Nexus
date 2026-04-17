//! OKX WebSocket adapter for user data stream.
//!
//! Implements `ExchangeWs` for OKX unified margin / spot account.
//! Authentication via signed login message after connect.
//! Subscribes to order and account channels.
//!
//! Nautilus source: `adapters/okx/v5/ws.py`

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use secrecy::{ExposeSecret, Secret};
use serde::Deserialize;
use sha2::Sha256;
use tokio::sync::{mpsc, Mutex as TokioMutex};
use std::sync::Arc as StdArc;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use hmac::{Hmac, Mac};

use crate::live::exchange::{
    OkxBalanceUpdate as BalanceUpdate, OkxExecutionReport,
    ExchangeWs, WsError as ExchangeWsError,
    WsMessage as ExchangeWsMessage,
};
use crate::live::normalizer::OkxSymbolNormalizer;

fn current_timestamp_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

// Shared state between the struct and the receive loop task.
struct WsState {
    #[allow(dead_code)]
    ws_stream: Option<futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::Connector,
        >,
        Message,
    >>,
    shutdown: bool,
}

/// OKX WebSocket adapter for unified margin / spot account.
pub struct OkxWsAdapter {
    ws_url: String,
    api_key: Secret<String>,
    secret_key: Secret<String>,
    passphrase: Secret<String>,
    #[allow(dead_code)]
    normalizer: OkxSymbolNormalizer,
    /// Channel to receive parsed messages from the background loop.
    recv_rx: Option<mpsc::Receiver<ExchangeWsMessage>>,
    /// Shared WS state for signaling shutdown.
    state: Option<StdArc<TokioMutex<WsState>>>,
}

impl OkxWsAdapter {
    /// Create a new OKX WS adapter.
    ///
    /// `ws_url` is typically `wss://ws.okx.com:8443/ws/v5/private`.
    pub fn new(
        api_key: Secret<String>,
        secret_key: Secret<String>,
        passphrase: Secret<String>,
        ws_url: &str,
    ) -> Self {
        Self {
            ws_url: ws_url.to_string(),
            api_key,
            secret_key,
            passphrase,
            normalizer: OkxSymbolNormalizer,
            recv_rx: None,
            state: None,
        }
    }

    #[allow(dead_code)]
    fn sign(&self, message: &str) -> String {
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(self.secret_key.expose_secret().as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        let result = mac.finalize();
        let bytes = result.into_bytes();
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
    }

    async fn receive_loop(
        ws_url: String,
        api_key: Secret<String>,
        secret_key: Secret<String>,
        passphrase: Secret<String>,
        recv_tx: mpsc::Sender<ExchangeWsMessage>,
        state: StdArc<TokioMutex<WsState>>,
    ) {
        loop {
            {
                let s = state.lock().await;
                if s.shutdown {
                    break;
                }
            }

            match Self::ws_session(&ws_url, &api_key, &secret_key, &passphrase, &recv_tx, &state).await {
                Ok(()) => break,
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            }
        }
    }

    async fn ws_session(
        ws_url: &str,
        api_key: &Secret<String>,
        secret_key: &Secret<String>,
        passphrase: &Secret<String>,
        recv_tx: &mpsc::Sender<ExchangeWsMessage>,
        state: &StdArc<TokioMutex<WsState>>,
    ) -> Result<(), ExchangeWsError> {
        let (ws, _) = connect_async(ws_url)
            .await
            .map_err(|e| ExchangeWsError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws.split();

        // Login with signature
        let timestamp = format!("{:.3}", current_timestamp_secs());
        let login_args = serde_json::json!([
            api_key.expose_secret(),
            passphrase.expose_secret(),
            timestamp,
        ]);
        let login_str = serde_json::to_string(&login_args).unwrap_or_default();
        let sign_str = format!("{}{}{}", timestamp, "LOGIN", login_str);
        let signature = {
            type HmacSha256 = Hmac<Sha256>;
            let mut mac = HmacSha256::new_from_slice(secret_key.expose_secret().as_bytes())
                .expect("HMAC can take key of any size");
            mac.update(sign_str.as_bytes());
            let result = mac.finalize();
            let bytes = result.into_bytes();
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
        };

        let login = serde_json::json!({
            "op": "login",
            "args": [
                {
                    "apiKey": api_key.expose_secret(),
                    "passphrase": passphrase.expose_secret(),
                    "timestamp": timestamp,
                    "sign": signature,
                }
            ]
        });
        let login_msg = serde_json::to_string(&login).map_err(|e| ExchangeWsError::ParseError(e.to_string()))?;
        write
            .send(Message::Text(login_msg.into()))
            .await
            .map_err(|e| ExchangeWsError::SendFailed(e.to_string()))?;

        // Wait for login ack
        if let Some(msg) = read.next().await {
            let text = msg.map_err(|e| ExchangeWsError::RecvFailed(e.to_string()))?;
            let text_str = text.into_text().map_err(|e| ExchangeWsError::RecvFailed(e.to_string()))?;
            if !text_str.contains("\"success\":true") && !text_str.contains("\"success\": true") {
                return Err(ExchangeWsError::ConnectionFailed(format!("login failed: {}", text_str)));
            }
        }

        // Subscribe to orders and account channels
        let subs = serde_json::json!([
            {"op": "subscribe", "args": [{"channel": "orders", "instType": "SPOT"}]},
            {"op": "subscribe", "args": [{"channel": "account"}]},
        ]);
        let sub_msg = serde_json::to_string(&subs).map_err(|e| ExchangeWsError::ParseError(e.to_string()))?;
        write
            .send(Message::Text(sub_msg.into()))
            .await
            .map_err(|e| ExchangeWsError::SendFailed(e.to_string()))?;

        // Message loop
        loop {
            {
                let s = state.lock().await;
                if s.shutdown {
                    return Ok(());
                }
            }

            tokio::select! {
                msg = read.next() => {
                    match msg {
                        Some(Ok(msg)) => {
                            match msg {
                                Message::Text(text) => {
                                    if let Some(ws_msg) = Self::parse_message(&text) {
                                        let _ = recv_tx.send(ws_msg).await;
                                    }
                                }
                                Message::Ping(data) => {
                                    let _ = write.send(Message::Pong(data)).await;
                                }
                                Message::Close(_) => {
                                    return Err(ExchangeWsError::ConnectionClosed);
                                }
                                _ => {}
                            }
                        }
                        Some(Err(e)) => {
                            return Err(ExchangeWsError::RecvFailed(e.to_string()));
                        }
                        None => return Ok(()),
                    }
                }
            }
        }
    }

    fn parse_message(raw: &str) -> Option<ExchangeWsMessage> {
        #[derive(Deserialize)]
        struct OkxGenericMsg {
            #[serde(rename = "event")]
            event: Option<String>,
            #[serde(rename = "arg")]
            #[allow(dead_code)]
            arg: Option<serde_json::Value>,
            #[serde(rename = "data")]
            data: Option<serde_json::Value>,
        }

        // Check for event-based messages first
        if let Ok(event_msg) = serde_json::from_str::<OkxGenericMsg>(raw) {
            if event_msg.event.as_deref() == Some("subscribe") && event_msg.data.is_some() {
                    return Some(ExchangeWsMessage::Unknown("subscribe_success".to_string()));
            }
        }

        // Parse as data message
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OrderData {
            #[serde(rename = "instId")]
            inst_id: String,
            #[serde(rename = "ordId")]
            ord_id: String,
            #[serde(rename = "clOrdId")]
            cl_ord_id: String,
            #[serde(rename = "side")]
            side: String,
            #[serde(rename = "ordType")]
            ord_type: String,
            #[serde(rename = "sz")]
            sz: String,
            #[serde(rename = "px")]
            px: String,
            #[serde(rename = "fillPx")]
            fill_px: Option<String>,
            #[serde(rename = "fillSz")]
            fill_sz: Option<String>,
            #[serde(rename = "state")]
            state: String,
            #[serde(rename = "avgPx")]
            avg_px: Option<String>,
            #[serde(rename = "tradeId")]
            trade_id: Option<String>,
            #[serde(rename = "uTime")]
            u_time: Option<String>,
        }

        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct AccountData {
            #[serde(rename = "ccy")]
            ccy: String,
            #[serde(rename = "bal")]
            bal: String,
        }

        if let Ok(msg) = serde_json::from_str::<serde_json::Value>(raw) {
            if let Some(data_arr) = msg.get("data").and_then(|v| v.as_array()) {
                if data_arr.is_empty() {
                    return Some(ExchangeWsMessage::Unknown(raw.to_string()));
                }
                let first = &data_arr[0];
                if let Some(arg) = msg.get("arg") {
                    let channel = arg.get("channel").and_then(|v| v.as_str()).unwrap_or("");
                    if channel == "orders" {
                        if let Ok(order) = serde_json::from_value::<OrderData>(first.clone()) {
                            let state = match order.state.as_str() {
                                "live" => "NEW",
                                "partially_filled" => "PARTIALLY_FILLED",
                                "filled" => "FILLED",
                                "canceled" => "CANCELED",
                                _ => "UNKNOWN",
                            };
                            return Some(ExchangeWsMessage::OkxExec(OkxExecutionReport {
                                event_time: order.u_time.as_ref().and_then(|t| t.parse::<u64>().ok()).unwrap_or(0) / 1_000_000,
                                symbol: order.inst_id,
                                client_order_id: order.cl_ord_id,
                                exchange_order_id: order.ord_id,
                                side: if order.side == "buy" {
                                    crate::messages::OrderSide::Buy
                                } else {
                                    crate::messages::OrderSide::Sell
                                },
                                order_type: order.ord_type,
                                order_status: state.to_string(),
                                reject_reason: String::new(),
                                price: order.px,
                                quantity: order.sz,
                                last_fill_qty: order.fill_sz.unwrap_or_default(),
                                last_fill_price: order.fill_px.unwrap_or_default(),
                                trade_time: 0,
                                trade_id: order.trade_id.unwrap_or_default(),
                                is_maker: false,
                                commission: None,
                                commission_asset: None,
                            }));
                        }
                    } else if channel == "account" {
                        if let Ok(acc) = serde_json::from_value::<AccountData>(first.clone()) {
                            return Some(ExchangeWsMessage::OkxBalance(BalanceUpdate {
                                asset: acc.ccy,
                                balance: acc.bal,
                                is_deposit: false,
                            }));
                        }
                    }
                }
            }
        }

        Some(ExchangeWsMessage::Unknown(raw.to_string()))
    }
}

// =============================================================================
// ExchangeWs trait implementation
// =============================================================================

#[async_trait]
impl ExchangeWs for OkxWsAdapter {
    async fn connect(&mut self) -> Result<(), ExchangeWsError> {
        let (recv_tx, recv_rx) = mpsc::channel::<ExchangeWsMessage>(64);
        let state: StdArc<TokioMutex<WsState>> = StdArc::new(TokioMutex::new(WsState {
            ws_stream: None,
            shutdown: false,
        }));

        let ws_url = self.ws_url.clone();
        let api_key = self.api_key.clone();
        let secret_key = self.secret_key.clone();
        let passphrase = self.passphrase.clone();
        let state_clone = StdArc::clone(&state);

        tokio::spawn(async move {
            Self::receive_loop(ws_url, api_key, secret_key, passphrase, recv_tx, state_clone).await;
        });

        self.recv_rx = Some(recv_rx);
        self.state = Some(state);
        Ok(())
    }

    async fn close(&mut self) -> Result<(), ExchangeWsError> {
        if let Some(state) = &self.state {
            let s = state.clone();
            {
                let mut s = s.lock().await;
                s.shutdown = true;
            }
        }
        self.recv_rx.take();
        self.state.take();
        Ok(())
    }

    async fn reconnect(&mut self, _listen_key: &str) -> Result<(), ExchangeWsError> {
        self.close().await?;
        self.connect().await
    }

    async fn recv(&mut self) -> Result<ExchangeWsMessage, ExchangeWsError> {
        if let Some(rx) = &mut self.recv_rx {
            rx.recv()
                .await
                .ok_or(ExchangeWsError::ConnectionClosed)
        } else {
            Err(ExchangeWsError::NotConnected)
        }
    }
}
