//! Bybit HTTP REST adapter for authenticated API calls.
//!
//! Handles order submission, cancellation, and account queries.
//! HMAC-SHA256 signed requests with body-signing (not query string).
//!
//! Nautilus source: `adapters/bybit/http.py`, `adapters/bybit/v5/http.py`

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
use crate::live::normalizer::{BybitSymbolNormalizer, SymbolNormalizer};
use crate::messages::{CancelOrder, ClientOrderId, OrderSide, SubmitOrder, VenueOrderId};

// =============================================================================
// Bybit HTTP Adapter
// =============================================================================

/// Bybit HTTP REST adapter for spot and unified margin API.
/// All methods are async and must be called from within a tokio runtime.
pub struct BybitHttpAdapter {
    api_key: Secret<String>,
    secret_key: Secret<String>,
    base_url: String,
    client: Client,
    normalizer: BybitSymbolNormalizer,
}

fn current_timestamp_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

impl BybitHttpAdapter {
    /// Create a new Bybit HTTP adapter.
    ///
    /// `base_url` is typically `https://api.bybit.com` for production.
    pub fn new(
        api_key: Secret<String>,
        secret_key: Secret<String>,
        base_url: &str,
    ) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("HTTP client builder should not fail with valid settings");

        Self {
            api_key,
            secret_key,
            base_url: base_url.to_string(),
            client,
            normalizer: BybitSymbolNormalizer,
        }
    }

    /// Place a market or limit order.
    pub async fn place_order_impl(
        &self,
        order: &SubmitOrder,
    ) -> Result<VenueOrderId, ExchangeError> {
        let symbol = self.normalizer.to_exchange_symbol(&order.instrument_id);
        let order_type = match order.order_type {
            crate::messages::OrderType::Market => "Market",
            crate::messages::OrderType::Limit => "Limit",
            _ => "Limit",
        };
        let side = match order.order_side {
            OrderSide::Buy => "Buy",
            OrderSide::Sell => "Sell",
        };

        let params = serde_json::json!({
            "category": "spot",
            "symbol": symbol,
            "side": side,
            "orderType": order_type,
            "qty": order.quantity.to_string(),
            "price": order.price.map(|p| p.to_string()),
            "orderLinkId": order.client_order_id.to_string(),
        });

        let response: OrderCreateResponse = self
            .signed_post("/v5/order/create", &params)
            .await?;

        Ok(VenueOrderId::new(&response.result.order_id))
    }

    /// Cancel an order.
    pub async fn cancel_order_impl(
        &self,
        cancel: &CancelOrder,
    ) -> Result<bool, ExchangeError> {
        // Bybit cancel by orderLinkId (client_order_id) — symbol is optional
        let params = serde_json::json!({
            "category": "spot",
            "orderLinkId": cancel.client_order_id.to_string(),
        });

        let _: OrderCancelResponse = self.signed_post("/v5/order/cancel", &params).await?;
        Ok(true)
    }

    /// Modify an existing order's price and/or quantity.
    pub async fn modify_order_impl(
        &self,
        client_order_id: &ClientOrderId,
        venue_order_id: Option<&VenueOrderId>,
        side: OrderSide,
        new_price: Option<f64>,
        new_quantity: Option<f64>,
        symbol: &str,
    ) -> Result<VenueOrderId, ExchangeError> {
        let symbol = self.normalizer.to_exchange_symbol(symbol);
        let side_str = match side {
            OrderSide::Buy => "Buy",
            OrderSide::Sell => "Sell",
        };

        let mut params = serde_json::json!({
            "category": "spot",
            "symbol": symbol,
            "side": side_str,
            "orderLinkId": client_order_id.to_string(),
        });

        if let Some(p) = new_price {
            params["price"] = serde_json::json!(p.to_string());
        }
        if let Some(q) = new_quantity {
            params["qty"] = serde_json::json!(q.to_string());
        }
        if let Some(vid) = venue_order_id {
            params["orderId"] = serde_json::json!(vid.to_string());
        }

        let response: OrderAmendResponse = self.signed_post("/v5/order/amend", &params).await?;
        Ok(VenueOrderId::new(&response.result.order_id))
    }

    /// Get order status by client order ID.
    pub async fn get_order_status_impl(
        &self,
        client_order_id: &ClientOrderId,
        symbol: &str,
    ) -> Result<OrderStatusResponse, ExchangeError> {
        let symbol = self.normalizer.to_exchange_symbol(symbol);
        let params = serde_json::json!({
            "category": "spot",
            "symbol": symbol,
            "orderLinkId": client_order_id.to_string(),
        });

        #[derive(Deserialize)]
        struct BybitOrderResponse {
            #[serde(rename = "result")]
            result: BybitOrderDetail,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct BybitOrderDetail {
            #[serde(rename = "orderId")]
            order_id: String,
            #[serde(rename = "orderLinkId")]
            order_link_id: String,
            #[serde(rename = "symbol")]
            symbol: String,
            #[serde(rename = "price")]
            price: String,
            #[serde(rename = "qty")]
            qty: String,
            #[serde(rename = "side")]
            side: String,
            #[serde(rename = "orderType")]
            order_type: String,
            #[serde(rename = "orderStatus")]
            order_status: String,
        }

        let response: BybitOrderResponse = self.signed_get("/v5/order/realtime", &params).await?;

        let detail = response.result;
        Ok(OrderStatusResponse {
            order_id: detail.order_id.parse().unwrap_or(0),
            client_order_id: detail.order_link_id,
            symbol: detail.symbol,
            price: detail.price,
            orig_qty: detail.qty,
            executed_qty: "0".to_string(),
            status: detail.order_status,
            order_type: detail.order_type,
            side: detail.side,
        })
    }

    /// Get all open orders.
    pub async fn get_open_orders_impl(
        &self,
    ) -> Result<Vec<OrderInfoResponse>, ExchangeError> {
        #[derive(Deserialize)]
        struct BybitOpenOrdersResponse {
            #[serde(rename = "result")]
            result: BybitOpenOrdersResult,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct BybitOpenOrdersResult {
            #[serde(rename = "list")]
            list: Vec<BybitOrderDetail2>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct BybitOrderDetail2 {
            #[serde(rename = "orderId")]
            order_id: String,
            #[serde(rename = "orderLinkId")]
            order_link_id: String,
            #[serde(rename = "symbol")]
            symbol: String,
            #[serde(rename = "price")]
            price: String,
            #[serde(rename = "qty")]
            qty: String,
            #[serde(rename = "side")]
            side: String,
            #[serde(rename = "orderType")]
            order_type: String,
            #[serde(rename = "orderStatus")]
            order_status: String,
        }

        let params = serde_json::json!({ "category": "spot" });
        let response: BybitOpenOrdersResponse = self
            .signed_get("/v5/order/realtime", &params)
            .await?;

        let orders = response
            .result
            .list
            .into_iter()
            .map(|o| OrderInfoResponse {
                order_id: o.order_id.parse().unwrap_or(0),
                client_order_id: o.order_link_id,
                symbol: o.symbol,
                price: o.price,
                orig_qty: o.qty,
                executed_qty: "0".to_string(),
                status: o.order_status,
                order_type: o.order_type,
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
        struct BybitBalanceResponse {
            #[serde(rename = "result")]
            result: BybitBalanceResult,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct BybitBalanceResult {
            #[serde(rename = "balances")]
            balances: Vec<BybitBalanceDetail>,
        }
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct BybitBalanceDetail {
            #[serde(rename = "coinName")]
            coin_name: String,
            #[serde(rename = "availableToWithdraw")]
            available: String,
            #[serde(rename = "walletBalance")]
            total: String,
        }

        let params = serde_json::json!({ "accountType": "UNIFIED" });
        let response: BybitBalanceResponse = self
            .signed_get("/v5/account/wallet-balance", &params)
            .await?;

        let balances = response
            .result
            .balances
            .into_iter()
            .map(|b| AssetBalance {
                asset: b.coin_name,
                free: b.available,
                locked: "0".to_string(),
            })
            .collect();

        Ok(AccountInfoResponse { balances })
    }

    // -------------------------------------------------------------------------
    // Private helpers
    // -------------------------------------------------------------------------

    /// Sign a POST request with Bybit HMAC-SHA256 body signing.
    async fn signed_post<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        params: &serde_json::Value,
    ) -> Result<T, ExchangeError> {
        let url = format!("{}{}", self.base_url, path);
        let timestamp = current_timestamp_ms().to_string();
        let json_str = params.to_string();

        // Bybit signature: timestamp + HTTP_METHOD + path + body
        let sign_str = format!("{}{}{}{}", timestamp, "POST", path, json_str);
        let signature = self.sign(&sign_str);

        let response = self
            .client
            .post(&url)
            .header("X-BAPI-API-KEY", self.api_key.expose_secret())
            .header("X-BAPI-SIGN", signature)
            .header("X-BAPI-SIGN-TYPE", "2")
            .header("X-BAPI-TIMESTAMP", &timestamp)
            .header("X-BAPI-RECV-WINDOW", "5000")
            .header("Content-Type", "application/json")
            .body(json_str)
            .send()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ExchangeError::Unknown(format!(
                "Bybit API error {}: {}",
                status, body
            )));
        }

        response
            .json()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))
    }

    /// Sign a GET request with Bybit HMAC-SHA256 (query string signed).
    async fn signed_get<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        params: &serde_json::Value,
    ) -> Result<T, ExchangeError> {
        let timestamp = current_timestamp_ms().to_string();
        let query = serde_json::to_string(params).unwrap_or_default();
        let url = if query.is_empty() {
            format!("{}{}", self.base_url, path)
        } else {
            format!("{}{}?{}", self.base_url, path, query)
        };

        // For GET: timestamp + method + path + queryString
        let sign_str = format!("{}{}{}{}", timestamp, "GET", path, query);
        let signature = self.sign(&sign_str);

        let response = self
            .client
            .get(&url)
            .header("X-BAPI-API-KEY", self.api_key.expose_secret())
            .header("X-BAPI-SIGN", signature)
            .header("X-BAPI-SIGN-TYPE", "2")
            .header("X-BAPI-TIMESTAMP", &timestamp)
            .header("X-BAPI-RECV-WINDOW", "5000")
            .send()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ExchangeError::Unknown(format!(
                "Bybit API error {}: {}",
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
        let mut mac =
            HmacSha256::new_from_slice(self.secret_key.expose_secret().as_bytes())
                .expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }
}

// =============================================================================
// Response types
// =============================================================================

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderCreateResponse {
    #[serde(rename = "result")]
    result: OrderCreateResult,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderCreateResult {
    #[serde(rename = "orderId")]
    order_id: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderCancelResponse {
    #[serde(rename = "result")]
    result: OrderCancelResult,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderCancelResult {
    #[serde(rename = "orderId")]
    order_id: String,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderAmendResponse {
    #[serde(rename = "result")]
    result: OrderAmendResult,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct OrderAmendResult {
    #[serde(rename = "orderId")]
    order_id: String,
}

// =============================================================================
// Exchange trait implementation
// =============================================================================

#[async_trait]
impl Exchange for BybitHttpAdapter {
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
