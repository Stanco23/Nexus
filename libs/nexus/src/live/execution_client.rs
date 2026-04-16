//! ExecutionClient — wires BinanceHttpAdapter + BinanceWsAdapter + Cache + MsgBus.
//!
//! Implements the Actor trait. Publishes order events to MsgBus and updates Cache.
//! Uses Cache as the sole authoritative state store (NO shadow Account).
//!
//! Nautilus source: `execution/engine.pyx`

use std::sync::{Arc, Mutex};

use crate::actor::MessageBus;
use crate::cache::Cache;
use crate::engine::account::{AccountId, OmsType};
use crate::engine::oms::Oms;
use crate::live::http_adapter::{BinanceApiError, BinanceHttpAdapter};
use crate::live::ws_adapter::{BinanceWsAdapter, WsMessage};
use crate::actor::Actor;
use crate::messages::{
    CancelOrder, ClientOrderId, OrderCancelled, OrderFilled,
    OrderSubmitted, PositionId, StrategyId, SubmitOrder, TraderId, VenueOrderId,
};

// =============================================================================
// Execution Client
// =============================================================================

/// Live execution client for Binance.
/// Wires HTTP and WebSocket adapters to the Cache and MsgBus.
pub struct ExecutionClient {
    /// HTTP adapter for order submission and cancellation.
    http: BinanceHttpAdapter,
    /// WebSocket adapter for receiving fills and balance updates.
    ws: BinanceWsAdapter,
    /// Authoritative state store.
    #[allow(dead_code)]
    cache: Arc<Mutex<Cache>>,
    /// Order Management System.
    oms: Oms,
    /// Account ID for this client.
    #[allow(dead_code)]
    account_id: AccountId,
}

impl ExecutionClient {
    /// Create a new ExecutionClient.
    pub fn new(
        api_key: secrecy::Secret<String>,
        secret_key: secrecy::Secret<String>,
        base_url: &str,
        ws_url: &str,
        listen_key: String,
        cache: Arc<Mutex<Cache>>,
        account_id: AccountId,
        msgbus: Arc<MessageBus>,
        oms_type: OmsType,
    ) -> Self {
        let http = BinanceHttpAdapter::new(api_key, secret_key, base_url);
        let ws = BinanceWsAdapter::new(listen_key, ws_url);
        let oms = Oms::new(cache.clone(), msgbus, oms_type);

        Self {
            http,
            ws,
            cache,
            oms,
            account_id,
        }
    }

    /// Connect to the WebSocket and start the receive loop.
    /// Must be called before placing orders.
    pub async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.ws.connect().await?;
        Ok(())
    }

    /// Disconnect and stop the WebSocket receive loop.
    pub async fn disconnect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.ws.close().await?;
        Ok(())
    }

    // -------------------------------------------------------------------------
    // Order handling
    // -------------------------------------------------------------------------

    /// Submit a market or limit order to Binance via HTTP.
    /// Calls oms.submit_order() then http.place_order(), returns (VenueOrderId, PositionId).
    pub async fn submit_order(
        &mut self,
        submit: SubmitOrder,
        _msgbus: &crate::actor::MessageBus,
    ) -> Result<(VenueOrderId, PositionId), BinanceApiError> {
        // Get strategy_id from submit or use default
        let strategy_id = submit.strategy_id.clone();

        // Submit to OMS (generates PositionId, persists to cache, publishes OrderSubmitted)
        let position_id = self.oms.submit_order(&submit, strategy_id);

        // Place the order via HTTP
        let venue_order_id = self.http.place_order(&submit).await?;

        Ok((venue_order_id, position_id))
    }

    /// Cancel an order via Binance HTTP API.
    /// Publishes `OrderCancelled` to MsgBus on success.
    pub async fn cancel_order(
        &mut self,
        cancel: CancelOrder,
        _msgbus: &crate::actor::MessageBus,
    ) -> Result<bool, BinanceApiError> {
        let result = self.http.cancel_order(&cancel).await?;
        Ok(result)
    }

    /// Handle an incoming WebSocket message (fills, balance updates).
    /// Updates Cache and publishes events to MsgBus.
    pub fn handle_ws_message(&mut self, msg: WsMessage, msgbus: &crate::actor::MessageBus) {
        match msg {
            WsMessage::ExecutionReport(report) => {
                self.handle_execution_report(report, msgbus);
            }
            WsMessage::BalanceUpdate(_) => {
                // Balance updates handled separately
            }
            WsMessage::AccountUpdate(_) => {
                // Account position updates handled separately
            }
            WsMessage::ListStatus(_) => {
                // OCO/list orders handled separately
            }
            WsMessage::ListenKeyExpired => {
                // TODO: reconnect with new listen key
            }
            WsMessage::Unknown(_) => {
                // Ignore unknown event types
            }
        }
    }

    fn handle_execution_report(
        &mut self,
        report: crate::live::ws_adapter::ExecutionReport,
        _msgbus: &crate::actor::MessageBus,
    ) {
        let client_order_id = ClientOrderId::new(&report.client_order_id);
        let venue_order_id = VenueOrderId::new(&report.exchange_order_id.to_string());

        // Dispatch to OMS based on order status
        match report.order_status.as_str() {
            "NEW" | "REPLACED" => {
                // Order accepted by exchange — update OMS with venue_order_id
                self.oms.apply_accepted(&client_order_id, venue_order_id);
            }
            "PARTIALLY_FILLED" | "FILLED" => {
                // Fill event — build OrderFilled and apply to OMS
                let fill = self.build_order_filled(&report);
                self.oms.apply_fill(&client_order_id, &fill);
            }
            "CANCELED" | "EXPIRED" => {
                // Order cancelled — notify OMS
                self.oms.cancel(&client_order_id);
            }
            "REJECTED" => {
                // Order rejected — notify OMS
                let reason = report.reject_reason.clone();
                self.oms.apply_rejection(&client_order_id, &reason);
            }
            _ => {}
        }
    }

    fn build_order_filled(
        &self,
        report: &crate::live::ws_adapter::ExecutionReport,
    ) -> OrderFilled {
        let fill_price = report
            .last_fill_price
            .parse::<f64>()
            .unwrap_or(0.0);
        let commission = report
            .commission
            .as_ref()
            .and_then(|c| c.parse::<f64>().ok())
            .unwrap_or(0.0);
        let filled_qty = report
            .last_fill_qty
            .parse::<f64>()
            .unwrap_or(0.0);

        OrderFilled {
            trader_id: TraderId::new("LIVE-TRADER"),
            strategy_id: StrategyId::new("LIVE-STRATEGY"),
            client_order_id: ClientOrderId::new(&report.client_order_id),
            venue_order_id: VenueOrderId::new(&report.exchange_order_id.to_string()),
            position_id: crate::messages::PositionId::new("TBD"), // PositionId set by OMS
            trade_id: crate::messages::TradeId::new(&report.trade_id.to_string()),
            instrument_id: format!("{}.BINANCE", report.symbol),
            order_side: report.side,
            filled_qty,
            fill_price,
            commission,
            slippage_bps: 0.0, // Calculated by risk engine
            is_maker: report.is_maker,
            ts_event: report.trade_time * 1_000_000, // millis to nanos
            ts_init: report.event_time * 1_000_000,
        }
    }
}

// =============================================================================
// Actor Trait Implementation
// =============================================================================

impl Actor for ExecutionClient {
    fn component(&self) -> &crate::actor::Component {
        // Placeholder — a real Component should be created during
        // ExecutionClient initialization with proper Clock + MsgBus wiring
        panic!("ExecutionClient::component called — must be initialized with real Component")
    }

    fn trader_id(&self) -> &str {
        "LIVE-TRADER"
    }

    fn trader_id_obj(&self) -> &TraderId {
        // Placeholder — in real use, TraderId is set during initialization
        panic!("ExecutionClient::trader_id_obj called — must be initialized with real TraderId")
    }

    fn on_start(&mut self) {
        // Spawn the async receive loop
        // In real implementation: tokio::spawn(async move { ... })
    }

    fn on_order_submitted(&mut self, _submit: &OrderSubmitted) {
        // Handler for order submission events
    }

    fn on_order_filled(&mut self, _fill: &OrderFilled) {
        // Handler for fill events
    }

    fn on_order_cancelled(&mut self, _event: &OrderCancelled) {
        // Handler for cancellation events
    }
}
