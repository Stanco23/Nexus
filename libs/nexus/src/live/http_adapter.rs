//! Binance HTTP REST adapter for authenticated API calls.
//!
//! Handles order submission, cancellation, and account queries.
//! HMAC-SHA256 signed requests. Rate limiting.
//!
//! Nautilus source: `adapters/binance/http/client.py`, `adapters/binance/spot/http/account.py`

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use reqwest::Client;
use secrecy::{ExposeSecret, Secret};
use serde::Deserialize;
use sha2::Sha256;
use tokio::sync::RwLock;
use tokio::time::Duration;

use crate::messages::{
    CancelOrder, ClientOrderId, OrderSide, SubmitOrder, TimeInForce, VenueOrderId,
};

// =============================================================================
// Error Types
// =============================================================================

/// Binance API error types.
#[derive(Debug, Clone)]
pub enum BinanceApiError {
    /// Network connection failure or timeout.
    NetworkError(String),
    /// Rate limit exceeded (HTTP 429).
    RateLimited { retry_after_ms: u64 },
    /// Authentication failed (invalid API key).
    AuthFailed,
    /// Invalid HMAC signature.
    InvalidSignature,
    /// Insufficient balance for order.
    InsufficientBalance,
    /// Order not found.
    OrderNotFound,
    /// Unhandled error with raw message.
    Unknown(String),
}

impl std::fmt::Display for BinanceApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BinanceApiError::NetworkError(msg) => write!(f, "NetworkError: {}", msg),
            BinanceApiError::RateLimited { retry_after_ms } => {
                write!(f, "RateLimited: retry after {}ms", retry_after_ms)
            }
            BinanceApiError::AuthFailed => write!(f, "AuthFailed: invalid API key"),
            BinanceApiError::InvalidSignature => write!(f, "InvalidSignature: HMAC error"),
            BinanceApiError::InsufficientBalance => write!(f, "InsufficientBalance"),
            BinanceApiError::OrderNotFound => write!(f, "OrderNotFound"),
            BinanceApiError::Unknown(msg) => write!(f, "Unknown: {}", msg),
        }
    }
}

impl std::error::Error for BinanceApiError {}

// =============================================================================
// Binance HTTP Adapter
// =============================================================================

/// Binance HTTP REST adapter for spot trading API.
/// All methods are async and must be called from within a tokio runtime.
pub struct BinanceHttpAdapter {
    api_key: Secret<String>,
    secret_key: Secret<String>,
    base_url: String,
    client: Client,
    /// Rate limiter: tracks remaining request weight.
    rate_limiter: Arc<RwLock<RateLimiter>>,
}

/// Simple token-bucket rate limiter for Binance API request weights.
struct RateLimiter {
    /// Remaining request weight.
    tokens: f64,
    /// Max tokens (bucket capacity).
    max_tokens: f64,
    /// Refill rate per millisecond (1200 per minute = 20/s = 0.02/ms).
    refill_per_ms: f64,
    /// Last refill timestamp (millis).
    last_refill: u64,
}

impl RateLimiter {
    fn new(max_tokens: f64, refill_per_minute: f64) -> Self {
        let refill_per_ms = refill_per_minute / 60_000.0;
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_per_ms,
            last_refill: current_timestamp_ms(),
        }
    }

    /// Try to acquire `weight` tokens. Returns Ok(()) if acquired.
    async fn acquire(&mut self, weight: u32) -> Result<(), u64> {
        self.refill();
        let weight = weight as f64;
        if self.tokens >= weight {
            self.tokens -= weight;
            Ok(())
        } else {
            let needed = weight - self.tokens;
            let wait_ms = (needed / self.refill_per_ms).ceil() as u64;
            Err(wait_ms)
        }
    }

    fn refill(&mut self) {
        let now = current_timestamp_ms();
        let elapsed = now.saturating_sub(self.last_refill);
        if elapsed > 0 {
            let refill = elapsed as f64 * self.refill_per_ms;
            self.tokens = (self.tokens + refill).min(self.max_tokens);
            self.last_refill = now;
        }
    }
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

impl BinanceHttpAdapter {
    /// Create a new Binance HTTP adapter.
    ///
    /// `base_url` is typically `https://api.binance.com` or `https://testnet.binance.vision`.
    pub fn new(api_key: Secret<String>, secret_key: Secret<String>, base_url: &str) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("HTTP client builder should not fail with valid settings");

        Self {
            api_key,
            secret_key,
            base_url: base_url.to_string(),
            client,
            rate_limiter: Arc::new(RwLock::new(RateLimiter::new(1200.0, 1200.0))),
        }
    }

    /// Place a market or limit order.
    /// Returns the venue-assigned order ID on success.
    pub async fn place_order(&self, order: &SubmitOrder) -> Result<VenueOrderId, BinanceApiError> {
        let weight = match order.order_type {
            crate::messages::OrderType::Market => 1,
            crate::messages::OrderType::Limit => 1,
            _ => 1,
        };

        {
            let mut limiter = self.rate_limiter.write().await;
            if let Err(retry_ms) = limiter.acquire(weight).await {
                return Err(BinanceApiError::RateLimited {
                    retry_after_ms: retry_ms,
                });
            }
        }

        let params = self.build_order_params(order);
        let response: OrderResponse = self
            .signed_post("/api/v3/order", params)
            .await
            .map_err(|e| e)?;

        Ok(VenueOrderId::new(&response.order_id.to_string()))
    }

    /// Cancel an order.
    /// Returns `true` if the order was successfully cancelled.
    pub async fn cancel_order(&self, cancel: &CancelOrder) -> Result<bool, BinanceApiError> {
        {
            let mut limiter = self.rate_limiter.write().await;
            limiter.acquire(1).await.map_err(|retry_ms| BinanceApiError::RateLimited {
                retry_after_ms: retry_ms,
            })?;
        }

        let params = vec![
            (
                "symbol".to_string(),
                cancel
                    .client_order_id
                    .to_string()
                    .split('.')
                    .next()
                    .unwrap_or("BTCUSDT")
                    .to_uppercase(),
            ),
            (
                "orderId".to_string(),
                cancel
                    .venue_order_id
                    .as_ref()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
            ),
            (
                "origClientOrderId".to_string(),
                cancel.client_order_id.to_string(),
            ),
        ];

        let _: CancelResponse = self
            .signed_delete("/api/v3/order", params)
            .await
            .map_err(|e| e)?;

        Ok(true)
    }

    /// Get order status by client order ID.
    pub async fn get_order_status(
        &self,
        client_order_id: &ClientOrderId,
        symbol: &str,
    ) -> Result<OrderStatusResponse, BinanceApiError> {
        {
            let mut limiter = self.rate_limiter.write().await;
            limiter.acquire(2).await.map_err(|retry_ms| BinanceApiError::RateLimited {
                retry_after_ms: retry_ms,
            })?;
        }

        let params = vec![
            ("symbol".to_string(), symbol.to_uppercase()),
            (
                "origClientOrderId".to_string(),
                client_order_id.to_string(),
            ),
        ];

        let response: OrderStatusResponse = self
            .signed_get("/api/v3/order", params)
            .await
            .map_err(|e| e)?;

        Ok(response)
    }

    /// Get account balance information.
    pub async fn get_account_info(&self) -> Result<AccountInfoResponse, BinanceApiError> {
        {
            let mut limiter = self.rate_limiter.write().await;
            limiter.acquire(10).await.map_err(|retry_ms| BinanceApiError::RateLimited {
                retry_after_ms: retry_ms,
            })?;
        }

        let response: AccountInfoResponse = self.signed_get("/api/v3/account", vec![]).await?;
        Ok(response)
    }

    /// Get all open orders.
    pub async fn get_open_orders(&self) -> Result<Vec<OrderInfoResponse>, BinanceApiError> {
        {
            let mut limiter = self.rate_limiter.write().await;
            limiter.acquire(3).await.map_err(|retry_ms| BinanceApiError::RateLimited {
                retry_after_ms: retry_ms,
            })?;
        }

        let response: Vec<OrderInfoResponse> = self
            .signed_get("/api/v3/openOrders", vec![])
            .await?;
        Ok(response)
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    fn build_order_params(&self, order: &SubmitOrder) -> Vec<(String, String)> {
        let symbol = self.symbol_from_instrument(&order.instrument_id);
        let mut params = vec![
            ("symbol".to_string(), symbol),
            ("side".to_string(), side_to_binance(order.order_side).to_string()),
            (
                "type".to_string(),
                order_type_to_binance(order.order_type).to_string(),
            ),
            ("quantity".to_string(), format_order_qty(order.quantity)),
            (
                "newClientOrderId".to_string(),
                order.client_order_id.to_string(),
            ),
        ];

        if let Some(price) = order.price {
            params.push(("price".to_string(), format!("{}", price)));
        }

        if let Some(tif) = order.time_in_force {
            params.push(("timeInForce".to_string(), tif_to_binance(tif).to_string()));
        }

        params.push(("recvWindow".to_string(), "5000".to_string()));
        params.push((
            "timestamp".to_string(),
            current_timestamp_ms().to_string(),
        ));

        params
    }

    fn symbol_from_instrument(&self, instrument_id: &str) -> String {
        instrument_id
            .split('.')
            .next()
            .unwrap_or(instrument_id)
            .to_uppercase()
    }

    async fn signed_get<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        mut params: Vec<(String, String)>,
    ) -> Result<T, BinanceApiError> {
        params.push((
            "timestamp".to_string(),
            current_timestamp_ms().to_string(),
        ));

        let query_string = build_query_string(&params);
        let signature = self.compute_hmac_signature(&query_string);
        let url = format!(
            "{}{}?{}&signature={}",
            self.base_url,
            path,
            query_string,
            signature
        );

        self.do_get(&url).await
    }

    async fn signed_delete<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        mut params: Vec<(String, String)>,
    ) -> Result<T, BinanceApiError> {
        params.push((
            "timestamp".to_string(),
            current_timestamp_ms().to_string(),
        ));
        params.push(("recvWindow".to_string(), "5000".to_string()));

        let query_string = build_query_string(&params);
        let signature = self.compute_hmac_signature(&query_string);
        let url = format!(
            "{}{}?{}&signature={}",
            self.base_url,
            path,
            query_string,
            signature
        );

        self.do_delete(&url).await
    }

    async fn signed_post<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        params: Vec<(String, String)>,
    ) -> Result<T, BinanceApiError> {
        let query_string = build_query_string(&params);
        let signature = self.compute_hmac_signature(&query_string);
        let url = format!(
            "{}{}?{}&signature={}",
            self.base_url, path, query_string, signature
        );

        self.do_post(&url).await
    }

    fn compute_hmac_signature(&self, query_string: &str) -> String {
        use hmac::{Hmac, Mac};
        type HmacSha256 = Hmac<Sha256>;

        let mut mac = HmacSha256::new_from_slice(
            self.secret_key.expose_secret().as_bytes(),
        )
        .expect("HMAC can take key of any size");
        mac.update(query_string.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    async fn do_get<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
    ) -> Result<T, BinanceApiError> {
        let response = self
            .client
            .get(url)
            .header("X-MBX-APIKEY", self.api_key.expose_secret())
            .send()
            .await
            .map_err(|e| BinanceApiError::NetworkError(e.to_string()))?;

        self.parse_response(response).await
    }

    async fn do_post<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
    ) -> Result<T, BinanceApiError> {
        let response = self
            .client
            .post(url)
            .header("X-MBX-APIKEY", self.api_key.expose_secret())
            .header("Content-Type", "application/x-www-form-urlencoded")
            .send()
            .await
            .map_err(|e| BinanceApiError::NetworkError(e.to_string()))?;

        self.parse_response(response).await
    }

    async fn do_delete<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
    ) -> Result<T, BinanceApiError> {
        let response = self
            .client
            .delete(url)
            .header("X-MBX-APIKEY", self.api_key.expose_secret())
            .send()
            .await
            .map_err(|e| BinanceApiError::NetworkError(e.to_string()))?;

        self.parse_response(response).await
    }

    async fn parse_response<T: for<'de> Deserialize<'de>>(
        &self,
        response: reqwest::Response,
    ) -> Result<T, BinanceApiError> {
        let status = response.status();

        if status.as_u16() == 429 {
            let retry_after = response
                .headers()
                .get("Retry-After")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse().ok())
                .unwrap_or(1);
            return Err(BinanceApiError::RateLimited {
                retry_after_ms: retry_after * 1000,
            });
        }

        if status.as_u16() == 401 || status.as_u16() == 403 {
            return Err(BinanceApiError::AuthFailed);
        }

        if status.as_u16() >= 400 {
            let body = response.text().await.unwrap_or_default();
            if let Some(code) = extract_error_code(&body) {
                match code {
                    -2015 => return Err(BinanceApiError::InsufficientBalance),
                    -2013 => return Err(BinanceApiError::OrderNotFound),
                    _ => {}
                }
            }
            return Err(BinanceApiError::Unknown(body));
        }

        response
            .json()
            .await
            .map_err(|e| BinanceApiError::Unknown(e.to_string()))
    }
}

// =============================================================================
// Binance JSON Response Types
// =============================================================================

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct OrderResponse {
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
    #[serde(rename = "transactTime")]
    transact_time: Option<u64>,
    #[serde(rename = "status")]
    status: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct CancelResponse {
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
    #[serde(rename = "status")]
    status: String,
}

/// Response type for order status queries.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct OrderStatusResponse {
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
    #[serde(rename = "symbol")]
    symbol: String,
    #[serde(rename = "price")]
    price: String,
    #[serde(rename = "origQty")]
    orig_qty: String,
    #[serde(rename = "executedQty")]
    executed_qty: String,
    #[serde(rename = "status")]
    status: String,
    #[serde(rename = "type")]
    order_type: String,
    #[serde(rename = "side")]
    side: String,
}

/// Response type for account info queries.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct AccountInfoResponse {
    #[serde(rename = "makerCommission")]
    maker_commission: i32,
    #[serde(rename = "takerCommission")]
    taker_commission: i32,
    #[serde(rename = "buyerCommission")]
    buyer_commission: i32,
    #[serde(rename = "sellerCommission")]
    seller_commission: i32,
    #[serde(rename = "canTrade")]
    can_trade: bool,
    #[serde(rename = "canWithdraw")]
    can_withdraw: bool,
    #[serde(rename = "canDeposit")]
    can_deposit: bool,
    #[serde(rename = "updateTime")]
    update_time: u64,
    #[serde(rename = "balances")]
    balances: Vec<BalanceInfo>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct BalanceInfo {
    #[serde(rename = "asset")]
    asset: String,
    #[serde(rename = "free")]
    free: String,
    #[serde(rename = "locked")]
    locked: String,
}

/// Response type for open orders queries.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct OrderInfoResponse {
    #[serde(rename = "symbol")]
    symbol: String,
    #[serde(rename = "orderId")]
    order_id: u64,
    #[serde(rename = "clientOrderId")]
    client_order_id: String,
    #[serde(rename = "price")]
    price: String,
    #[serde(rename = "origQty")]
    orig_qty: String,
    #[serde(rename = "executedQty")]
    executed_qty: String,
    #[serde(rename = "status")]
    status: String,
    #[serde(rename = "type")]
    order_type: String,
    #[serde(rename = "side")]
    side: String,
    #[serde(rename = "time")]
    time: Option<u64>,
    #[serde(rename = "updateTime")]
    update_time: Option<u64>,
}

// =============================================================================
// Serialization Helpers
// =============================================================================

fn side_to_binance(side: OrderSide) -> &'static str {
    match side {
        OrderSide::Buy => "BUY",
        OrderSide::Sell => "SELL",
    }
}

fn order_type_to_binance(order_type: crate::messages::OrderType) -> &'static str {
    match order_type {
        crate::messages::OrderType::Market => "MARKET",
        crate::messages::OrderType::Limit => "LIMIT",
        crate::messages::OrderType::Stop => "STOP_LOSS",
        crate::messages::OrderType::StopLimit => "STOP_LOSS_LIMIT",
        crate::messages::OrderType::Iceberg => "ICEBERG",
        crate::messages::OrderType::Twap => "TWAP",
        crate::messages::OrderType::Vwap => "VWAP",
        crate::messages::OrderType::TrailingStop => "TRAILING_STOP_MARKET",
    }
}

fn tif_to_binance(tif: TimeInForce) -> &'static str {
    match tif {
        TimeInForce::Gtc => "GTC",
        TimeInForce::Gtd => "GTD",
        TimeInForce::Ioc => "IOC",
        TimeInForce::Fok => "FOK",
        TimeInForce::Gfx => "GEX",
    }
}

fn format_order_qty(qty: f64) -> String {
    let s = format!("{}", qty);
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Build a URL query string from params. Simple implementation.
fn build_query_string(params: &[(String, String)]) -> String {
    params
        .iter()
        .map(|(k, v)| format!("{}={}", urlencoding_encode(k), urlencoding_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Simple URL encoding for query strings.
fn urlencoding_encode(s: &str) -> String {
    let mut result = String::new();
    for c in s.chars() {
        match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => {
                result.push(c);
            }
            _ => {
                for byte in c.to_string().as_bytes() {
                    result.push_str(&format!("%{:02X}", byte));
                }
            }
        }
    }
    result
}

fn extract_error_code(body: &str) -> Option<i32> {
    body.split("\"code\":")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse().ok())
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_order_qty() {
        assert_eq!(format_order_qty(1.0), "1");
        assert_eq!(format_order_qty(1.5), "1.5");
        assert_eq!(format_order_qty(0.001), "0.001");
    }

    #[test]
    fn test_url_encoding() {
        assert_eq!(urlencoding_encode("BTCUSDT"), "BTCUSDT");
        assert_eq!(urlencoding_encode("a b"), "a%20b");
    }

    #[test]
    fn test_error_display() {
        let err = BinanceApiError::InsufficientBalance;
        assert_eq!(format!("{}", err), "InsufficientBalance");
    }
}
