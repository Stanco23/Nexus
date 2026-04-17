//! OKX HTTP REST adapter for authenticated API calls.
//!
//! Handles order submission, cancellation, and account queries.
//! HMAC-SHA256 signed requests with OKX-specific header format.
//!
//! Nautilus source: `adapters/okx/http.py`, `adapters/okx/v5/http.py`

use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use reqwest::Client;
use secrecy::{ExposeSecret, Secret};
use serde::Deserialize;
use sha2::Sha256;
use tokio::time::Duration;

use crate::live::exchange::{
    AccountInfoResponse, AssetBalance, Exchange, ExchangeError,
};
use crate::live::http_adapter::{OrderInfoResponse, OrderStatusResponse};
use crate::live::normalizer::{OkxSymbolNormalizer, SymbolNormalizer};
use crate::messages::{CancelOrder, ClientOrderId, OrderSide, SubmitOrder, VenueOrderId};

#[allow(dead_code)]
fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// OKX HTTP REST adapter for spot and unified margin API.
/// All methods are async and must be called from within a tokio runtime.
pub struct OkxHttpAdapter {
    api_key: Secret<String>,
    secret_key: Secret<String>,
    passphrase: Secret<String>,
    base_url: String,
    client: Client,
    normalizer: OkxSymbolNormalizer,
}

impl OkxHttpAdapter {
    /// Create a new OKX HTTP adapter.
    ///
    /// `base_url` is typically `https://www.okx.com/api/v5`.
    pub fn new(
        api_key: Secret<String>,
        secret_key: Secret<String>,
        passphrase: Secret<String>,
        base_url: &str,
    ) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("HTTP client builder should not fail with valid settings");

        Self {
            api_key,
            secret_key,
            passphrase,
            base_url: base_url.to_string(),
            client,
            normalizer: OkxSymbolNormalizer,
        }
    }

    /// Place a market or limit order.
    pub async fn place_order_impl(
        &self,
        order: &SubmitOrder,
    ) -> Result<VenueOrderId, ExchangeError> {
        let symbol = self.normalizer.to_exchange_symbol(&order.instrument_id);
        let order_type = match order.order_type {
            crate::messages::OrderType::Market => "market",
            crate::messages::OrderType::Limit => "limit",
            _ => "limit",
        };
        let side = match order.order_side {
            OrderSide::Buy => "buy",
            OrderSide::Sell => "sell",
        };

        let inst_id = symbol.clone();

        let params = serde_json::json!({
            "instId": inst_id,
            "tdMode": "cash",
            "side": side,
            "ordType": order_type,
            "sz": order.quantity.to_string(),
            "px": order.price.map(|p| p.to_string()),
            "clOrdId": order.client_order_id.to_string(),
        });

        let response: OrderCreateResponse = self
            .signed_post("/v5/trade/order", &params)
            .await?;

        Ok(VenueOrderId::new(&response.data.order_id))
    }

    /// Cancel an order.
    pub async fn cancel_order_impl(
        &self,
        cancel: &CancelOrder,
    ) -> Result<bool, ExchangeError> {
        // OKX cancel by clOrdId (client_order_id) — instId is optional
        let params = serde_json::json!({
            "clOrdId": cancel.client_order_id.to_string(),
        });

        let _: OrderCancelResponse = self.signed_post("/v5/trade/cancel-order", &params).await?;
        Ok(true)
    }

    /// Modify an existing order's price and/or quantity.
    pub async fn modify_order_impl(
        &self,
        client_order_id: &ClientOrderId,
        _venue_order_id: Option<&VenueOrderId>,
        side: OrderSide,
        new_price: Option<f64>,
        new_quantity: Option<f64>,
        symbol: &str,
    ) -> Result<VenueOrderId, ExchangeError> {
        let symbol = self.normalizer.to_exchange_symbol(symbol);
        let side_str = match side {
            OrderSide::Buy => "buy",
            OrderSide::Sell => "sell",
        };

        let inst_id = symbol.clone();

        let mut params = serde_json::json!({
            "instId": inst_id,
            "side": side_str,
            "clOrdId": client_order_id.to_string(),
        });

        if let Some(p) = new_price {
            params["px"] = serde_json::json!(p.to_string());
        }
        if let Some(q) = new_quantity {
            params["sz"] = serde_json::json!(q.to_string());
        }

        let response: OrderAmendResponse = self.signed_post("/v5/trade/amend-order", &params).await?;
        let order_id = response.data.first()
            .map(|r| r.order_id.clone())
            .unwrap_or_default();
        Ok(VenueOrderId::new(&order_id))
    }

    /// Get order status by client order ID.
    pub async fn get_order_status_impl(
        &self,
        client_order_id: &ClientOrderId,
        symbol: &str,
    ) -> Result<OrderStatusResponse, ExchangeError> {
        let symbol = self.normalizer.to_exchange_symbol(symbol);
        let params = serde_json::json!({
            "instId": symbol,
            "clOrdId": client_order_id.to_string(),
        });

        #[derive(Deserialize)]
        struct OkxOrderResponse {
            #[serde(rename = "data")]
            data: Vec<OkxOrderDetail>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OkxOrderDetail {
            #[serde(rename = "ordId")]
            ord_id: String,
            #[serde(rename = "clOrdId")]
            cl_ord_id: String,
            #[serde(rename = "instId")]
            inst_id: String,
            #[serde(rename = "px")]
            price: String,
            #[serde(rename = "sz")]
            qty: String,
            #[serde(rename = "side")]
            side: String,
            #[serde(rename = "ordType")]
            ord_type: String,
            #[serde(rename = "state")]
            state: String,
        }

        let response: OkxOrderResponse = self.signed_get("/v5/trade/order", &params).await?;

        let detail = response.data.into_iter().next().ok_or_else(|| {
            ExchangeError::Unknown("order not found".to_string())
        })?;

        let order_status = match detail.state.as_str() {
            "live" => "NEW",
            "partially_filled" => "PARTIALLY_FILLED",
            "filled" => "FILLED",
            "canceled" => "CANCELED",
            _ => "UNKNOWN",
        };

        Ok(OrderStatusResponse {
            order_id: detail.ord_id.parse().unwrap_or(0),
            client_order_id: detail.cl_ord_id,
            symbol: detail.inst_id,
            price: detail.price,
            orig_qty: detail.qty,
            executed_qty: "0".to_string(),
            status: order_status.to_string(),
            order_type: detail.ord_type,
            side: detail.side,
        })
    }

    /// Get all open orders.
    pub async fn get_open_orders_impl(
        &self,
    ) -> Result<Vec<OrderInfoResponse>, ExchangeError> {
        #[derive(Deserialize)]
        struct OkxOpenOrdersResponse {
            #[serde(rename = "data")]
            data: Vec<OkxOpenOrderDetail>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OkxOpenOrderDetail {
            #[serde(rename = "ordId")]
            ord_id: String,
            #[serde(rename = "clOrdId")]
            cl_ord_id: String,
            #[serde(rename = "instId")]
            inst_id: String,
            #[serde(rename = "px")]
            price: String,
            #[serde(rename = "sz")]
            qty: String,
            #[serde(rename = "side")]
            side: String,
            #[serde(rename = "ordType")]
            ord_type: String,
            #[serde(rename = "state")]
            state: String,
        }

        let params = serde_json::json!({ "state": "live" });
        let response: OkxOpenOrdersResponse = self
            .signed_get("/v5/trade/orders-pending", &params)
            .await?;

        let orders = response
            .data
            .into_iter()
            .map(|o| OrderInfoResponse {
                order_id: o.ord_id.parse().unwrap_or(0),
                client_order_id: o.cl_ord_id,
                symbol: o.inst_id,
                price: o.price,
                orig_qty: o.qty,
                executed_qty: "0".to_string(),
                status: o.state,
                order_type: o.ord_type,
                side: o.side,
                time: None,
                update_time: None,
            })
            .collect();

        Ok(orders)
    }

    /// Get account balance information.
    pub async fn get_account_info_impl(
        &self,
    ) -> Result<AccountInfoResponse, ExchangeError> {
        #[derive(Deserialize)]
        struct OkxBalanceResponse {
            #[serde(rename = "data")]
            data: Vec<OkxBalanceDetail>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OkxBalanceDetail {
            #[serde(rename = "details")]
            details: Vec<OkxBalanceCoin>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct OkxBalanceCoin {
            #[serde(rename = "ccy")]
            coin: String,
            #[serde(rename = "availBal")]
            available: String,
            #[serde(rename = "bal")]
            total: String,
        }

        let params = serde_json::json!({});
        let response: OkxBalanceResponse = self
            .signed_get("/v5/account/balance", &params)
            .await?;

        let balances = response
            .data
            .into_iter()
            .flat_map(|d| d.details)
            .map(|b| AssetBalance {
                asset: b.coin,
                free: b.available,
                locked: "0".to_string(),
            })
            .collect();

        Ok(AccountInfoResponse { balances })
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    /// Sign a POST request with OKX HMAC-SHA256 signature.
    async fn signed_post<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        params: &serde_json::Value,
    ) -> Result<T, ExchangeError> {
        let url = format!("{}{}", self.base_url, path);
        let timestamp = format_timestamp();
        let json_str = params.to_string();
        let sign_str = format!("{}{}{}{}", timestamp, "POST", path, json_str);
        let signature = self.sign(&sign_str);

        let response = self
            .client
            .post(&url)
            .header("OK-ACCESS-KEY", self.api_key.expose_secret())
            .header("OK-ACCESS-SIGN", signature)
            .header("OK-ACCESS-TIMESTAMP", &timestamp)
            .header("OK-ACCESS-PASSPHRASE", self.passphrase.expose_secret())
            .header("Content-Type", "application/json")
            .body(json_str)
            .send()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ExchangeError::Unknown(format!(
                "OKX API error {}: {}",
                status, body
            )));
        }

        response
            .json()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))
    }

    /// Sign a GET request with OKX HMAC-SHA256.
    async fn signed_get<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        params: &serde_json::Value,
    ) -> Result<T, ExchangeError> {
        let timestamp = format_timestamp();
        let query = serde_json::to_string(params).unwrap_or_default();
        let url = if query.is_empty() {
            format!("{}{}", self.base_url, path)
        } else {
            format!("{}{}?{}", self.base_url, path, query)
        };

        let sign_str = format!("{}{}{}{}", timestamp, "GET", path, query);
        let signature = self.sign(&sign_str);

        let response = self
            .client
            .get(&url)
            .header("OK-ACCESS-KEY", self.api_key.expose_secret())
            .header("OK-ACCESS-SIGN", signature)
            .header("OK-ACCESS-TIMESTAMP", &timestamp)
            .header("OK-ACCESS-PASSPHRASE", self.passphrase.expose_secret())
            .send()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ExchangeError::Unknown(format!(
                "OKX API error {}: {}",
                status, body
            )));
        }

        response
            .json()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))
    }

    fn sign(&self, message: &str) -> String {
        use hmac::{Hmac, Mac};
        type HmacSha256 = Hmac<Sha256>;
        let mut mac = HmacSha256::new_from_slice(self.secret_key.expose_secret().as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        let result = mac.finalize();
        let bytes = result.into_bytes();
        // OKX expects base64 encoding of the raw HMAC output
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
    }
}

fn format_timestamp() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    format!("{:.3}", now)
}

// =============================================================================
// Response types
// =============================================================================

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderCreateResponse {
    #[serde(rename = "data")]
    data: OrderCreateResult,
    #[serde(rename = "msg")]
    msg: Option<String>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderCreateResult {
    #[serde(rename = "ordId")]
    order_id: String,
    #[serde(rename = "clOrdId")]
    cl_ord_id: String,
    #[serde(rename = "sCode")]
    s_code: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderCancelResponse {
    #[serde(rename = "data")]
    data: Vec<OrderCancelResult>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderCancelResult {
    #[serde(rename = "ordId")]
    order_id: String,
    #[serde(rename = "clOrdId")]
    cl_ord_id: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderAmendResponse {
    #[serde(rename = "data")]
    data: Vec<OrderAmendResult>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderAmendResult {
    #[serde(rename = "ordId")]
    order_id: String,
}

// =============================================================================
// Exchange trait implementation
// =============================================================================

#[async_trait]
impl Exchange for OkxHttpAdapter {
    async fn place_order(&self, order: &SubmitOrder) -> Result<VenueOrderId, ExchangeError> {
        self.place_order_impl(order).await
    }

    async fn cancel_order(&self, cancel: &CancelOrder) -> Result<bool, ExchangeError> {
        self.cancel_order_impl(cancel).await
    }

    async fn modify_order(
        &self,
        client_order_id: &ClientOrderId,
        venue_order_id: Option<&VenueOrderId>,
        side: crate::messages::OrderSide,
        new_price: Option<f64>,
        new_quantity: Option<f64>,
        symbol: &str,
    ) -> Result<VenueOrderId, ExchangeError> {
        self.modify_order_impl(client_order_id, venue_order_id, side, new_price, new_quantity, symbol)
            .await
    }

    async fn get_order_status(
        &self,
        client_order_id: &ClientOrderId,
        symbol: &str,
    ) -> Result<OrderStatusResponse, ExchangeError> {
        self.get_order_status_impl(client_order_id, symbol).await
    }

    async fn get_open_orders(&self) -> Result<Vec<OrderInfoResponse>, ExchangeError> {
        self.get_open_orders_impl().await
    }

    async fn get_account_info(&self) -> Result<AccountInfoResponse, ExchangeError> {
        self.get_account_info_impl().await
    }

    async fn place_order_list(
        &self,
        orders: &[SubmitOrder],
    ) -> Result<Vec<VenueOrderId>, ExchangeError> {
        let mut venue_order_ids = Vec::with_capacity(orders.len());
        for order in orders {
            let venue_id = self.place_order(order).await?;
            venue_order_ids.push(venue_id);
        }
        Ok(venue_order_ids)
    }
}
