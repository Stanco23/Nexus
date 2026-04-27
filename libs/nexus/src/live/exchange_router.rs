//! ExchangeRouter — routes order requests to the correct exchange adapter by venue.
//!
//! Maps Venue → Exchange adapter. When a SubmitOrder arrives, the router parses
//! the venue from `instrument_id` (format "SYMBOL.VENUE") and dispatches to the
//! correct exchange.
//!
//! # Example
//!
//! ```ignore
//! let mut router = ExchangeRouter::new();
//! router.add_exchange(Venue::new("BINANCE"), Arc::new(binance_adapter));
//! router.add_exchange(Venue::new("BYBIT"), Arc::new(bybit_adapter));
//!
//! let venue_order_id = router.place_order(&submit_order).await?;
//! ```

use crate::live::exchange::{Exchange, ExchangeError, AccountInfoResponse as ExchangeAccountInfo};
use crate::live::http_adapter::OrderInfoResponse;
use crate::messages::{CancelOrder, ClientOrderId, OrderSide, SubmitOrder, VenueOrderId};
use std::collections::HashMap;
use std::sync::Arc;

/// Router that maps Venue → Exchange adapter.
/// Used by ExecutionClient to route orders to the correct exchange.
pub struct ExchangeRouter {
    exchanges: HashMap<String, Arc<dyn Exchange>>,
    default: Option<Arc<dyn Exchange>>,
}

impl ExchangeRouter {
    /// Create a new empty router.
    pub fn new() -> Self {
        Self {
            exchanges: HashMap::new(),
            default: None,
        }
    }

    /// Add an exchange adapter for a venue.
    pub fn add_exchange(&mut self, venue: &str, exchange: Arc<dyn Exchange>) {
        self.exchanges.insert(venue.to_string(), exchange);
    }

    /// Set a default exchange (used when venue cannot be determined from instrument_id).
    pub fn set_default(&mut self, exchange: Arc<dyn Exchange>) {
        self.default = Some(exchange);
    }

    /// Get the exchange for a given venue code.
    pub fn get(&self, venue: &str) -> Option<&Arc<dyn Exchange>> {
        self.exchanges.get(venue)
    }

    /// Get the default exchange.
    pub fn default_exchange(&self) -> Option<&Arc<dyn Exchange>> {
        self.default.as_ref().or_else(|| self.exchanges.values().next())
    }

    /// Parse the venue from an instrument_id string.
    /// Format: "SYMBOL.VENUE" e.g. "BTCUSDT.BINANCE"
    fn venue_from_instrument(instrument_id: &str) -> Option<&str> {
        instrument_id.rsplit('.').next()
    }

    /// Place an order — routes to the correct exchange based on instrument_id.
    pub async fn place_order(&self, order: &SubmitOrder) -> Result<VenueOrderId, ExchangeError> {
        let venue = Self::venue_from_instrument(&order.instrument_id);
        
        let exchange = if let Some(v) = venue {
            self.exchanges.get(v).or_else(|| self.default.as_ref())
        } else {
            self.default.as_ref().or_else(|| self.exchanges.values().next())
        };

        exchange
            .ok_or_else(|| ExchangeError::Unknown("no exchanges registered".to_string()))?
            .place_order(order)
            .await
    }

    /// Cancel an order via the default exchange.
    /// Note: cancel_order uses venue_order_id which is exchange-specific,
    /// so routing is implicit in the venue_order_id itself.
    pub async fn cancel_order(&self, cancel: &CancelOrder) -> Result<bool, ExchangeError> {
        let exchange = self.default.as_ref().or_else(|| self.exchanges.values().next());
        exchange
            .ok_or_else(|| ExchangeError::Unknown("no exchanges registered".to_string()))?
            .cancel_order(cancel)
            .await
    }

    /// Modify an order via the default exchange.
    /// Note: modify_order uses client_order_id/venue_order_id which is exchange-specific.
    pub async fn modify_order(
        &self,
        client_order_id: &ClientOrderId,
        venue_order_id: Option<&VenueOrderId>,
        side: OrderSide,
        new_price: Option<f64>,
        new_quantity: Option<f64>,
        symbol: &str,
    ) -> Result<VenueOrderId, ExchangeError> {
        let exchange = self.default.as_ref().or_else(|| self.exchanges.values().next());
        exchange
            .ok_or_else(|| ExchangeError::Unknown("no exchanges registered".to_string()))?
            .modify_order(client_order_id, venue_order_id, side, new_price, new_quantity, symbol)
            .await
    }

    /// Get open orders from the default exchange.
    pub async fn get_open_orders(&self) -> Result<Vec<OrderInfoResponse>, ExchangeError> {
        let exchange = self.default.as_ref().or_else(|| self.exchanges.values().next());
        exchange
            .ok_or_else(|| ExchangeError::Unknown("no exchanges registered".to_string()))?
            .get_open_orders()
            .await
    }

    /// Get account info from the default exchange.
    pub async fn get_account_info(&self) -> Result<ExchangeAccountInfo, ExchangeError> {
        let exchange = self.default.as_ref().or_else(|| self.exchanges.values().next());
        exchange
            .ok_or_else(|| ExchangeError::Unknown("no exchanges registered".to_string()))?
            .get_account_info()
            .await
    }

    /// Get order status from the default exchange.
    pub async fn get_order_status(
        &self,
        client_order_id: &ClientOrderId,
        symbol: &str,
    ) -> Result<crate::live::http_adapter::OrderStatusResponse, ExchangeError> {
        let exchange = self.default.as_ref().or_else(|| self.exchanges.values().next());
        exchange
            .ok_or_else(|| ExchangeError::Unknown("no exchanges registered".to_string()))?
            .get_order_status(client_order_id, symbol)
            .await
    }
}

impl Default for ExchangeRouter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_venue_from_instrument() {
        assert_eq!(ExchangeRouter::venue_from_instrument("BTCUSDT.BINANCE"), Some("BINANCE"));
        assert_eq!(ExchangeRouter::venue_from_instrument("ETHUSDT.BYBIT"), Some("BYBIT"));
        assert_eq!(ExchangeRouter::venue_from_instrument("SOLUSDT.OKX"), Some("OKX"));
        assert_eq!(ExchangeRouter::venue_from_instrument("NO-VENUE"), Some("NO-VENUE"));
    }

    #[test]
    fn test_exchange_router_venue_routing() {
        let mut router = ExchangeRouter::new();
        assert!(router.default_exchange().is_none());
        assert!(router.get("BINANCE").is_none());
    }
}
