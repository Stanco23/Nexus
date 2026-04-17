//! Bybit WebSocket adapter for user data stream.
//!
//! Implements `ExchangeWs` for Bybit unified margin / spot account.
//! Authentication via signed auth message after connect.
//! Subscribes to order and wallet topics.
//!
//! Nautilus source: `adapters/bybit/v5/ws.py`

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
    BybitBalanceUpdate as BalanceUpdate, BybitExecutionReport, ExchangeWs, WsError as ExchangeWsError,
    WsMessage as ExchangeWsMessage,
};
use crate::live::normalizer::BybitSymbolNormalizer;

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
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

/// Bybit WebSocket adapter for unified margin / spot account.
pub struct BybitWsAdapter {
    ws_url: String,
    api_key: Secret<String>,
    secret_key: Secret<String>,
    #[allow(dead_code)]
    normalizer: BybitSymbolNormalizer,
    /// Channel to receive parsed messages from the background loop.
    recv_rx: Option<MpscReceiver<ExchangeWsMessage>>,
    /// Shared WS state for signaling shutdown.
    state: Option<StdArc<TokioMutex<WsState>>>,
}

type MpscReceiver<T> = mpsc::Receiver<T>;

impl BybitWsAdapter {
    /// Create a new Bybit WS adapter.
    ///
    /// `ws_url` is typically `wss://stream.bybit.com/v5/private`.
    pub fn new(api_key: Secret<String>, secret_key: Secret<String>, ws_url: &str) -> Self {
        Self {
            ws_url: ws_url.to_string(),
            api_key,
            secret_key,
            normalizer: BybitSymbolNormalizer,
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
        hex::encode(result.into_bytes())
    }

    /// Single connection receive loop: connect, auth, subscribe, recv messages.
    async fn receive_loop(
        ws_url: String,
        api_key: Secret<String>,
        secret_key: Secret<String>,
        recv_tx: mpsc::Sender<ExchangeWsMessage>,
        state: StdArc<TokioMutex<WsState>>,
    ) {
        loop {
            // Check shutdown flag before connecting
            {
                let s = state.lock().await;
                if s.shutdown {
                    break;
                }
            }

            match Self::ws_session(&ws_url, &api_key, &secret_key, &recv_tx, &state).await {
                Ok(()) => break,
                Err(_e) => {
                    // Connection failed — retry after short delay
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            }
        }
    }

    /// Single WS session: connect, authenticate, subscribe, recv messages until disconnect.
    async fn ws_session(
        ws_url: &str,
        api_key: &Secret<String>,
        secret_key: &Secret<String>,
        recv_tx: &mpsc::Sender<ExchangeWsMessage>,
        state: &StdArc<TokioMutex<WsState>>,
    ) -> Result<(), ExchangeWsError> {
        let (ws, _) = connect_async(ws_url)
            .await
            .map_err(|e| ExchangeWsError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws.split();

        // Authenticate
        let timestamp = current_timestamp_ms();
        let expires = (timestamp + 10000).to_string();
        let sign_str = format!("{}{}{}{}", timestamp, "GET", "/v5/user/auth", expires);

        // Build signature using HMAC-SHA256
        let sign_result = {
            type HmacSha256 = Hmac<Sha256>;
            let mut mac = HmacSha256::new_from_slice(secret_key.expose_secret().as_bytes())
                .expect("HMAC can take key of any size");
            mac.update(sign_str.as_bytes());
            hex::encode(mac.finalize().into_bytes())
        };

        let auth_params = serde_json::json!({
            "req_id": "auth",
            "op": "auth",
            "args": [
                api_key.expose_secret(),
                timestamp.to_string(),
                expires,
                sign_result,
            ]
        });
        let auth_msg =
            serde_json::to_string(&auth_params).map_err(|e| ExchangeWsError::ParseError(e.to_string()))?;
        write
            .send(Message::Text(auth_msg.into()))
            .await
            .map_err(|e| ExchangeWsError::SendFailed(e.to_string()))?;

        // Wait for auth response
        if let Some(msg) = read.next().await {
            let text = msg.map_err(|e| ExchangeWsError::RecvFailed(e.to_string()))?;
            let text_str = text.into_text().map_err(|e| ExchangeWsError::RecvFailed(e.to_string()))?;
            let resp: AuthResponse = match serde_json::from_str(&text_str) {
                Ok(r) => r,
                Err(_) => {
                    return Err(ExchangeWsError::ParseError(format!(
                        "auth response parse failed: {}",
                        text_str
                    )));
                }
            };
            if resp.op != "auth" || resp.ret_msg.as_deref() != Some("success") {
                return Err(ExchangeWsError::ConnectionFailed(format!(
                    "auth failed: {:?}",
                    resp.ret_msg
                )));
            }
        }

        // Subscribe to order and wallet topics
        let subs = serde_json::json!([
            {"op": "subscribe", "args": ["order"]},
            {"op": "subscribe", "args": ["wallet"]},
        ]);
        let sub_msg =
            serde_json::to_string(&subs).map_err(|e| ExchangeWsError::ParseError(e.to_string()))?;
        write
            .send(Message::Text(sub_msg.into()))
            .await
            .map_err(|e| ExchangeWsError::SendFailed(e.to_string()))?;

        // Read subscribe ack
        if let Some(msg) = read.next().await {
            let text = msg.map_err(|e| ExchangeWsError::RecvFailed(e.to_string()))?;
            let text_str = text.into_text().map_err(|e| ExchangeWsError::RecvFailed(e.to_string()))?;
            #[derive(Deserialize)]
            #[allow(dead_code)]
            struct SubAck {
                op: String,
                #[serde(rename = "ret_msg")]
                ret_msg: Option<String>,
                success: Option<bool>,
            }
            if let Ok(ack) = serde_json::from_str::<SubAck>(&text_str) {
                if ack.ret_msg.as_deref() != Some("success")
                    && ack.success != Some(true)
                {
                    return Err(ExchangeWsError::SubscribeFailed(text_str.to_string()));
                }
            }
        }

        // Message loop
        loop {
            // Check shutdown flag
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

    /// Parse a raw Bybit WS message into an exchange-level `WsMessage`.
    fn parse_message(raw: &str) -> Option<ExchangeWsMessage> {
        #[derive(Deserialize)]
        #[serde(tag = "op")]
        struct OpMsg {
            op: String,
            #[serde(rename = "topic")]
            topic: Option<String>,
            #[serde(rename = "data")]
            data: Option<serde_json::Value>,
            #[serde(rename = "req_id")]
            #[allow(dead_code)]
            req_id: Option<String>,
            #[serde(rename = "ret_msg")]
            ret_msg: Option<String>,
            #[serde(rename = "success")]
            success: Option<bool>,
        }

        let msg: OpMsg = serde_json::from_str(raw).ok()?;

        match msg.op.as_str() {
            "auth" => {
                if msg.success == Some(true) || msg.ret_msg.as_deref() == Some("success") {
                    Some(ExchangeWsMessage::Unknown("auth_success".to_string()))
                } else {
                    None
                }
            }
            "subscribe" => {
                if msg.success == Some(true) || msg.ret_msg.as_deref() == Some("success") {
                    Some(ExchangeWsMessage::Unknown("subscribe_success".to_string()))
                } else {
                    Some(ExchangeWsMessage::Unknown(raw.to_string()))
                }
            }
            "notify" => {
                let topic = msg.topic.as_deref().unwrap_or("");
                let data = msg.data?;
                if topic.starts_with("order") {
                    let rep: BybitOrderNotify = serde_json::from_value(data).ok()?;
                    Some(ExchangeWsMessage::BybitExec(BybitExecutionReport {
                        event_time: rep.updated_time / 1_000_000,
                        symbol: rep.symbol,
                        client_order_id: rep.order_link_id,
                        exchange_order_id: rep.order_id,
                        side: if rep.side == "Buy" {
                            crate::messages::OrderSide::Buy
                        } else {
                            crate::messages::OrderSide::Sell
                        },
                        order_type: rep.order_type,
                        order_status: rep.order_status,
                        reject_reason: String::new(),
                        price: rep.price,
                        quantity: rep.qty,
                        last_fill_qty: rep.last_exec_qty,
                        last_fill_price: rep.last_exec_price,
                        trade_time: rep.exec_time / 1_000_000,
                        trade_id: rep.exec_id,
                        is_maker: false,
                        commission: Some(rep.exec_fee),
                        commission_asset: None,
                    }))
                } else if topic.starts_with("wallet") {
                    let bal: BybitWalletNotify = serde_json::from_value(data).ok()?;
                    Some(ExchangeWsMessage::BybitBalance(BalanceUpdate {
                        asset: bal.coin,
                        balance: bal.wallet_balance,
                        is_deposit: false,
                    }))
                } else {
                    Some(ExchangeWsMessage::Unknown(raw.to_string()))
                }
            }
            _ => Some(ExchangeWsMessage::Unknown(raw.to_string())),
        }
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct AuthResponse {
    op: String,
    #[serde(rename = "ret_msg")]
    ret_msg: Option<String>,
    success: Option<bool>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct BybitOrderNotify {
    #[serde(rename = "orderId")]
    order_id: String,
    #[serde(rename = "orderLinkId")]
    order_link_id: String,
    #[serde(rename = "symbol")]
    symbol: String,
    #[serde(rename = "side")]
    side: String,
    #[serde(rename = "orderType")]
    order_type: String,
    #[serde(rename = "orderStatus")]
    order_status: String,
    #[serde(rename = "price")]
    price: String,
    #[serde(rename = "qty")]
    qty: String,
    #[serde(rename = "lastExecPrice")]
    last_exec_price: String,
    #[serde(rename = "lastExecQty")]
    last_exec_qty: String,
    #[serde(rename = "execFee")]
    exec_fee: String,
    #[serde(rename = "execId")]
    exec_id: String,
    #[serde(rename = "execTime")]
    exec_time: u64,
    #[serde(rename = "updatedTime")]
    updated_time: u64,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct BybitWalletNotify {
    #[serde(rename = "coin")]
    coin: String,
    #[serde(rename = "availableToWithdraw")]
    available_to_withdraw: String,
    #[serde(rename = "walletBalance")]
    wallet_balance: String,
}

// =============================================================================
// ExchangeWs trait implementation
// =============================================================================

#[async_trait]
impl ExchangeWs for BybitWsAdapter {
    async fn connect(&mut self) -> Result<(), ExchangeWsError> {
        let (recv_tx, recv_rx) = mpsc::channel::<ExchangeWsMessage>(64);
        let state: StdArc<TokioMutex<WsState>> = StdArc::new(TokioMutex::new(WsState {
            ws_stream: None,
            shutdown: false,
        }));

        let ws_url = self.ws_url.clone();
        let api_key = self.api_key.clone();
        let secret_key = self.secret_key.clone();
        let state_clone = StdArc::clone(&state);

        tokio::spawn(async move {
            Self::receive_loop(ws_url, api_key, secret_key, recv_tx, state_clone).await;
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
