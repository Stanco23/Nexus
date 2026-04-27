//! Coinbase HTTP adapter — REST API for Coinbase Advanced Trade.
//!
//! # Status: Stub Implementation
//! This adapter is a placeholder. Full implementation requires:
//! - OAuth2 / API key authentication headers (CB-ACCESS-KEY, CB-ACCESS-SIGN, CB-ACCESS-TIMESTAMP)
//! - Product ID format: "BTC-USD" (hyphen-separated, not dot-separated)
//! - Order book depth fetch for L2 order book data
//!
//! Nautilus source: `adapters/coinbase/`

use crate::live::exchange::{Exchange, ExchangeError, AccountInfoResponse};
use crate::messages::{
    CancelOrder, ClientOrderId, OrderSide, SubmitOrder, VenueOrderId,
};
use async_trait::async_trait;

/// Coinbase HTTP adapter.
/// Communicates with Coinbase Advanced Trade REST API.
#[derive(Debug)]
pub struct CoinbaseHttpAdapter {
    _api_key: secrecy::Secret<String>,
    _secret_key: secrecy::Secret<String>,
}

impl CoinbaseHttpAdapter {
    pub fn new(api_key: secrecy::Secret<String>, secret_key: secrecy::Secret<String>) -> Self {
        Self {
            _api_key: api_key,
            _secret_key: secret_key,
        }
    }
}

#[async_trait]
impl Exchange for CoinbaseHttpAdapter {
    async fn place_order(&self, _order: &SubmitOrder) -> Result<VenueOrderId, ExchangeError> {
        Err(ExchangeError::Unknown("Coinbase adapter not fully implemented".to_string()))
    }

    async fn cancel_order(&self, _cancel: &CancelOrder) -> Result<bool, ExchangeError> {
        Err(ExchangeError::Unknown("Coinbase adapter not fully implemented".to_string()))
    }

    async fn modify_order(
        &self,
        _client_order_id: &ClientOrderId,
        _venue_order_id: Option<&VenueOrderId>,
        _side: OrderSide,
        _new_price: Option<f64>,
        _new_quantity: Option<f64>,
        _symbol: &str,
    ) -> Result<VenueOrderId, ExchangeError> {
        Err(ExchangeError::Unknown("Coinbase adapter not fully implemented".to_string()))
    }

    async fn get_order_status(
        &self,
        _client_order_id: &ClientOrderId,
        _symbol: &str,
    ) -> Result<crate::live::http_adapter::OrderStatusResponse, ExchangeError> {
        Err(ExchangeError::Unknown("Coinbase adapter not fully implemented".to_string()))
    }

    async fn get_open_orders(&self) -> Result<Vec<crate::live::http_adapter::OrderInfoResponse>, ExchangeError> {
        Err(ExchangeError::Unknown("Coinbase adapter not fully implemented".to_string()))
    }

    async fn get_account_info(&self) -> Result<AccountInfoResponse, ExchangeError> {
        Err(ExchangeError::Unknown("Coinbase adapter not fully implemented".to_string()))
    }

    async fn place_order_list(&self, _orders: &[SubmitOrder]) -> Result<Vec<VenueOrderId>, ExchangeError> {
        Err(ExchangeError::Unknown("Coinbase adapter not fully implemented".to_string()))
    }
}
