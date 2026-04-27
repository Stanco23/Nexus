//! Coinbase HTTP adapter — REST API for Coinbase Advanced Trade.
//!
//! Implements the `Exchange` trait for Coinbase Advanced Trade API.
//! Uses HMAC-SHA256 signing with CB-ACCESS-KEY, CB-ACCESS-SIGN, CB-ACCESS-TIMESTAMP, CB-ACCESS-PASSPHRASE.
//!
//! Product ID format: "BTC-USD" (hyphen-separated, not dot-separated)
//! Internal instrument_id format: "BTCUSDT.COINBASE" → converted to "BTC-USD"
//!
//! Nautilus source: `adapters/coinbase/`

use crate::live::exchange::{AccountInfoResponse, AssetBalance, Exchange, ExchangeError};
use crate::live::http_adapter::{OrderInfoResponse, OrderStatusResponse};
use crate::messages::{CancelOrder, ClientOrderId, OrderSide, SubmitOrder, TimeInForce, VenueOrderId};
use async_trait::async_trait;
use secrecy::ExposeSecret;
use reqwest::Client;
use std::time::{SystemTime, UNIX_EPOCH};

/// Coinbase HTTP adapter.
/// Communicates with Coinbase Advanced Trade REST API.
#[derive(Debug)]
pub struct CoinbaseHttpAdapter {
    api_key: secrecy::Secret<String>,
    secret_key: secrecy::Secret<String>,
    passphrase: secrecy::Secret<String>,
    base_url: String,
    client: Client,
}

impl CoinbaseHttpAdapter {
    pub fn new(
        api_key: secrecy::Secret<String>,
        secret_key: secrecy::Secret<String>,
        passphrase: secrecy::Secret<String>,
    ) -> Self {
        Self {
            api_key,
            secret_key,
            passphrase,
            base_url: "https://api.coinbase.com".to_string(),
            client: Client::new(),
        }
    }

    /// Convert internal instrument_id (e.g. "BTCUSDT.COINBASE") to Coinbase product ID (e.g. "BTC-USD").
    fn to_product_id(instrument_id: &str) -> String {
        let symbol = instrument_id
            .split('.')
            .next()
            .unwrap_or(instrument_id);
        // Most pairs: XXX-USD or XXX-USDT → normalize USDT → USD for Coinbase
        if symbol.ends_with("USDT") {
            format!("{}-USD", &symbol[..symbol.len() - 4])
        } else if symbol.ends_with("USD") && !symbol.contains('-') {
            format!("{}-USD", &symbol[..symbol.len() - 3])
        } else if symbol.ends_with("BTC") {
            format!("{}-BTC", &symbol[..symbol.len() - 3])
        } else {
            format!("{}-USD", symbol)
        }
    }

    /// Get current timestamp in seconds.
    fn timestamp() -> String {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .to_string()
    }

    /// Compute CB-ACCESS-SIGN: HMAC-SHA256(timestamp + method + path + body)
    fn compute_signature(&self, timestamp: &str, method: &str, path: &str, body: &str) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        type HmacSha256 = Hmac<Sha256>;

        let message = format!("{}{}{}", timestamp, method, path);
        let mut mac = HmacSha256::new_from_slice(self.secret_key.expose_secret().as_bytes())
            .expect("HMAC can take key of any size");
        mac.update(message.as_bytes());
        mac.update(body.as_bytes());
        let result = mac.finalize();
        hex::encode(result.into_bytes())
    }

    /// Make an authenticated GET request.
    async fn signed_get(&self, path: &str) -> Result<String, ExchangeError> {
        let timestamp = Self::timestamp();
        let body = "";
        let signature = self.compute_signature(&timestamp, "GET", path, body);

        let response = self
            .client
            .get(format!("{}{}", self.base_url, path))
            .header("CB-ACCESS-KEY", self.api_key.expose_secret().as_str())
            .header("CB-ACCESS-SIGN", &signature)
            .header("CB-ACCESS-TIMESTAMP", &timestamp)
            .header("CB-ACCESS-PASSPHRASE", self.passphrase.expose_secret().as_str())
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            return Err(ExchangeError::Unknown(format!(
                "Coinbase GET {} failed: {}",
                path, status
            )));
        }

        response
            .text()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))
    }

    /// Make an authenticated POST request.
    async fn signed_post<T: serde::de::DeserializeOwned>(&self, path: &str, body: &str) -> Result<T, ExchangeError> {
        let timestamp = Self::timestamp();
        let signature = self.compute_signature(&timestamp, "POST", path, body);

        let response = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("CB-ACCESS-KEY", self.api_key.expose_secret().as_str())
            .header("CB-ACCESS-SIGN", &signature)
            .header("CB-ACCESS-TIMESTAMP", &timestamp)
            .header("CB-ACCESS-PASSPHRASE", self.passphrase.expose_secret().as_str())
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ExchangeError::Unknown(format!(
                "Coinbase POST {} failed: {} - {}",
                path, status, text
            )));
        }

        response
            .json()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))
    }

    /// Make an authenticated DELETE request.
    async fn signed_delete(&self, path: &str) -> Result<String, ExchangeError> {
        let timestamp = Self::timestamp();
        let body = "";
        let signature = self.compute_signature(&timestamp, "DELETE", path, body);

        let response = self
            .client
            .delete(format!("{}{}", self.base_url, path))
            .header("CB-ACCESS-KEY", self.api_key.expose_secret().as_str())
            .header("CB-ACCESS-SIGN", &signature)
            .header("CB-ACCESS-TIMESTAMP", &timestamp)
            .header("CB-ACCESS-PASSPHRASE", self.passphrase.expose_secret().as_str())
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(ExchangeError::Unknown(format!(
                "Coinbase DELETE {} failed: {} - {}",
                path, status, text
            )));
        }

        response
            .text()
            .await
            .map_err(|e| ExchangeError::NetworkError(e.to_string()))
    }
}

#[async_trait]
impl Exchange for CoinbaseHttpAdapter {
    async fn place_order(&self, order: &SubmitOrder) -> Result<VenueOrderId, ExchangeError> {
        let product_id = Self::to_product_id(&order.instrument_id);
        let side = match order.order_side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        };
        let order_type = match order.order_type {
            crate::messages::OrderType::Market => "market",
            crate::messages::OrderType::Limit => "limit",
            _ => "limit", // Coinbase doesn't support all order types
        };
        let time_in_force = match order.time_in_force {
            Some(TimeInForce::Gtc) => "GTC",
            Some(TimeInForce::Ioc) => "IOC",
            Some(TimeInForce::Fok) => "FOK",
            _ => "GTC",
        };

        #[derive(serde::Serialize)]
        struct PlaceOrderBody<'a> {
            client_order_id: &'a str,
            product_id: &'a str,
            side: &'a str,
            order_type: &'a str,
            size: String,
            price: Option<String>,
            time_in_force: &'a str,
        }

        let body = serde_json::to_string(&PlaceOrderBody {
            client_order_id: &order.client_order_id.to_string(),
            product_id: &product_id,
            side,
            order_type,
            size: order.quantity.to_string(),
            price: order.price.map(|p| p.to_string()),
            time_in_force,
        })
        .map_err(|e| ExchangeError::Unknown(e.to_string()))?;

        #[derive(serde::Deserialize)]
        struct PlaceOrderResponse {
            order_id: String,
        }

        let resp: PlaceOrderResponse = self.signed_post("/api/v3/brokerage/orders", &body).await?;
        Ok(VenueOrderId::new(&resp.order_id))
    }

    async fn cancel_order(&self, cancel: &CancelOrder) -> Result<bool, ExchangeError> {
        let order_id = cancel
            .venue_order_id
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| cancel.client_order_id.to_string());

        let path = format!("/api/v3/brokerage/orders/batch_cancel?order_ids={}", order_id);
        let _ = self.signed_delete(&path).await?;
        Ok(true)
    }

    async fn modify_order(
        &self,
        _client_order_id: &ClientOrderId,
        venue_order_id: Option<&VenueOrderId>,
        _side: OrderSide,
        new_price: Option<f64>,
        new_quantity: Option<f64>,
        _symbol: &str,
    ) -> Result<VenueOrderId, ExchangeError> {
        // Coinbase uses cancel-replace for modifications
        let order_id = venue_order_id
            .ok_or_else(|| ExchangeError::Unknown("modify requires venue_order_id".to_string()))?;
        Err(ExchangeError::Unknown(format!(
            "modify not yet implemented - cancel and re-place for order {}",
            order_id
        )))
    }

    async fn get_order_status(
        &self,
        client_order_id: &ClientOrderId,
        _symbol: &str,
    ) -> Result<crate::live::http_adapter::OrderStatusResponse, ExchangeError> {
        #[derive(serde::Deserialize)]
        struct OrderResponse {
            order_id: String,
            client_order_id: String,
            product_id: String,
            side: String,
            order_type: String,
            status: String,
            size: String,
            filled_size: String,
            price: Option<String>,
        }

        let path = format!(
            "/api/v3/brokerage/orders/histicity/batch?order_id={}",
            client_order_id
        );
        let _text = self.signed_get(&path).await?;
        // Note: Response parsing requires careful handling of Coinbase's response format
        // Return a stub response for now
        Ok(OrderStatusResponse {
            order_id: 0,
            client_order_id: client_order_id.to_string(),
            symbol: _symbol.to_string(),
            price: "0".to_string(),
            orig_qty: "0".to_string(),
            executed_qty: "0".to_string(),
            status: "UNKNOWN".to_string(),
            order_type: "UNKNOWN".to_string(),
            side: "UNKNOWN".to_string(),
        })
    }

    async fn get_open_orders(&self) -> Result<Vec<OrderInfoResponse>, ExchangeError> {
        #[derive(serde::Deserialize)]
        struct OrdersResponse {
            orders: Vec<OrderInfo>,
        }
        #[derive(serde::Deserialize)]
        struct OrderInfo {
            order_id: String,
            client_order_id: String,
            product_id: String,
            side: String,
            order_type: String,
            status: String,
            size: String,
            filled_size: String,
            price: Option<String>,
        }

        let text = self
            .signed_get("/api/v3/brokerage/orders/histicity?order_type=_LIMIT&status=OPEN")
            .await?;

        let resp: OrdersResponse = serde_json::from_str(&text)
            .map_err(|e| ExchangeError::Unknown(format!("parse error: {}", e)))?;

        let orders: Vec<OrderInfoResponse> = resp
            .orders
            .into_iter()
            .map(|o| OrderInfoResponse {
                order_id: o.order_id.parse().unwrap_or(0),
                client_order_id: o.client_order_id,
                symbol: o.product_id,
                price: o.price.clone().unwrap_or_default(),
                orig_qty: o.size,
                executed_qty: o.filled_size,
                status: o.status,
                order_type: o.order_type,
                side: o.side,
                time: None,
                update_time: None,
            })
            .collect();

        Ok(orders)
    }

    async fn get_account_info(&self) -> Result<AccountInfoResponse, ExchangeError> {
        #[derive(serde::Deserialize)]
        struct AccountsResponse {
            accounts: Vec<AccountInfo>,
        }
        #[derive(serde::Deserialize)]
        struct AccountInfo {
            uuid: String,
            currency: String,
            available: String,
            hold: String,
        }

        let text = self.signed_get("/api/v3/brokerage/accounts").await?;

        let resp: AccountsResponse = serde_json::from_str(&text)
            .map_err(|e| ExchangeError::Unknown(format!("parse error: {}", e)))?;

        let balances: Vec<AssetBalance> = resp
            .accounts
            .into_iter()
            .map(|a| AssetBalance {
                asset: a.currency,
                free: a.available,
                locked: a.hold,
            })
            .collect();

        Ok(AccountInfoResponse { balances })
    }

    async fn place_order_list(&self, orders: &[SubmitOrder]) -> Result<Vec<VenueOrderId>, ExchangeError> {
        let mut ids = Vec::new();
        for order in orders {
            let id = self.place_order(order).await?;
            ids.push(id);
        }
        Ok(ids)
    }
}
