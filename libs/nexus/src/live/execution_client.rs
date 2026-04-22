//! ExecutionClient — wires BinanceHttpAdapter + BinanceWsAdapter + Cache + MsgBus.
//!
//! Implements the Actor trait. Publishes order events to MsgBus and updates Cache.
//! Wires RiskEngine and Account for live trading safety and balance tracking.
//!
//! Nautilus source: `execution/engine.pyx`

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::actor::MessageBus;
use crate::cache::Cache;
use crate::data::DataEngine;
use crate::engine::account::{Account, AccountId};
use crate::engine::oms::{Oms, OmsOrder};
use crate::instrument::{Venue, fnv1a_hash};
use crate::live::exchange::{Exchange, ExchangeError, ExchangeWs, MarketDataAdapter, WsMessage as ExchangeWsMessage};
use crate::actor::Actor;
use crate::messages::{
    CancelOrder, ClientOrderId, ModifyOrder, OrderCancelled, OrderFilled,
    OrderSide, OrderSubmitted, OrderType, PositionId, StrategyId,
    SubmitOrder, TraderId, VenueOrderId,
};
use crate::engine::oms::OrderState;

// =============================================================================
// Execution Client
// =============================================================================

/// Live execution client for Binance.
/// Wires HTTP and WebSocket adapters to the Cache and MsgBus.
pub struct ExecutionClient {
    /// Component for Actor trait.
    component: crate::actor::Component,
    /// Trader ID for this client.
    trader_id: TraderId,
    /// HTTP adapter for order submission and cancellation.
    http: Box<dyn Exchange>,
    /// WebSocket adapter for receiving fills and balance updates.
    ws: Arc<Mutex<Box<dyn ExchangeWs>>>,
    /// Authoritative state store.
    #[allow(dead_code)]
    cache: Arc<Mutex<Cache>>,
    /// Order Management System.
    oms: Oms,
    /// Account ID for this client.
    #[allow(dead_code)]
    account_id: AccountId,
    /// Account for balance and equity tracking (wired in Phase 5.6).
    account: Arc<Mutex<Account>>,
    /// Data engine for routing quote and orderbook data (wired in Phase 5.5).
    #[allow(dead_code)]
    data_engine: Arc<Mutex<DataEngine>>,
    /// Risk engine for pre-trade checks (wired in Phase 5.6).
    risk_engine: Arc<Mutex<crate::engine::risk::RiskEngine>>,
    /// Message bus for publishing WS events and registering endpoints.
    msgbus: Arc<MessageBus>,
    /// Pending order modifications awaiting exchange confirmation.
    /// Maps client_order_id -> ModifyOrder. Only applied to OMS after exchange
    /// sends ExecutionReport with REPLACED status.
    pending_modifications: HashMap<ClientOrderId, ModifyOrder>,
    /// Pending order submissions — maps client_order_id to the uuid of the submit request.
    /// Used for audit trail / correlation when execution reports arrive.
    pending_submissions: HashMap<ClientOrderId, uuid::Uuid>,
    /// Channel for routing WS messages back to the main task context for processing.
    /// This allows handle_ws_message to be called (which dispatches to on_* handlers)
    /// from the correct async context rather than the spawned WS loop.
    ws_tx: Arc<Mutex<Option<tokio::sync::mpsc::Sender<ExchangeWsMessage>>>>,
    /// Optional market data adapter for routing trade/quote/orderbook data to DataEngine.
    /// Set via `set_market_data_adapter()` before calling `connect()`.
    market_data_adapter: Option<Arc<Mutex<Box<dyn MarketDataAdapter>>>>
}

impl ExecutionClient {
    /// Create a new ExecutionClient.
    ///
    /// Accepts boxed exchange adapters for runtime polymorphism. Callers should
    /// construct and box the concrete adapters:
    ///
    /// ```ignore
    /// let http = Box::new(BinanceHttpAdapter::new(api_key, secret_key, base_url));
    /// let ws = Box::new(BinanceWsAdapter::new(listen_key, ws_url));
    /// let client = ExecutionClient::new(http, ws, trader_id, account_id, cache, oms, data_engine, clock, msgbus);
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        http: Box<dyn Exchange>,
        ws: Arc<Mutex<Box<dyn ExchangeWs>>>,
        trader_id: TraderId,
        account_id: AccountId,
        account: Arc<Mutex<Account>>,
        cache: Arc<Mutex<Cache>>,
        oms: Oms,
        data_engine: Arc<Mutex<DataEngine>>,
        risk_engine: Arc<Mutex<crate::engine::risk::RiskEngine>>,
        clock: Box<dyn crate::actor::Clock>,
        msgbus: Arc<MessageBus>,
    ) -> Self {
        let msgbus_for_component: crate::actor::MessageBus = (*msgbus).clone();
        let logger = crate::actor::Logger::new("ExecutionClient");
        let component = crate::actor::Component::new(
            0, // id assigned by actor system
            "ExecutionClient",
            trader_id.clone(),
            clock,
            msgbus_for_component,
            logger,
        );

        Self {
            component,
            trader_id,
            http,
            ws,
            cache,
            oms,
            account_id,
            account,
            data_engine,
            risk_engine,
            msgbus: Arc::clone(&msgbus),
            pending_modifications: HashMap::new(),
            pending_submissions: HashMap::new(),
            ws_tx: Arc::new(Mutex::new(None)),
            market_data_adapter: None,
        }
    }

    /// Set the market data adapter for routing trade/quote/orderbook data to DataEngine.
    /// Must be called before `connect()`.
    pub fn set_market_data_adapter(
        &mut self,
        adapter: Arc<Mutex<Box<dyn MarketDataAdapter>>>,
    ) {
        self.market_data_adapter = Some(adapter);
    }

    /// Connect to the WebSocket and start the receive loop.
    /// Must be called before placing orders.
    pub async fn connect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.ws.lock().unwrap().connect().await?;

        // Create a channel so the spawned WS recv loop can feed messages to
        // handle_ws_message running in the main task context (where &mut self is available).
        let (tx, mut rx) = tokio::sync::mpsc::channel::<ExchangeWsMessage>(200);
        // Store a clone in ws_tx so disconnect() can drop it to signal shutdown
        *self.ws_tx.lock().unwrap() = Some(tx.clone());

        // Spawn WS receive loop — this task ONLY receives from the websocket and
        // feeds messages into the channel. It does NOT process them.
        let ws = Arc::clone(&self.ws);
        let name = self.component.name.to_string();
        tokio::task::spawn_local(async move {
            let log_prefix = format!("[{}] WS Recv", name);
            // tx was moved here as `mut tx` in the outer scope
            loop {
                let msg_result = {
                    let mut ws_guard = ws.lock().unwrap();
                    ws_guard.recv().await
                };

                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("{} error: {}, reconnecting...", log_prefix, e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        break;
                    }
                };

                // MISS-5 FIX: send to channel instead of msgbus.send directly.
                // This allows handle_ws_message to be called in the main task context
                // where we have &mut self access.
                if tx.send(msg).await.is_err() {
                    // Receiver (main task) dropped — client disconnected
                    break;
                }
            }
            eprintln!("{} exited", log_prefix);
        });

        // MISS-5 FIX: Process loop running in main task context.
        // This calls handle_ws_message (which invokes on_* handlers and updates
        // Cache/Account/RiskEngine) for every WS message received.
        let msgbus = Arc::clone(&self.msgbus);
        while let Some(msg) = rx.recv().await {
            self.handle_ws_message(msg, &msgbus);
        }

        Ok(())
    }

    /// Disconnect and stop the WebSocket receive loop.
    pub async fn disconnect(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Drop the tx to signal the recv loop to exit
        *self.ws_tx.lock().unwrap() = None;
        self.ws.lock().unwrap().close().await?;
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
    ) -> Result<(VenueOrderId, PositionId), ExchangeError> {
        // Parse instrument_id to InstrumentId for Cache lookup
        let instrument_id = crate::instrument::InstrumentId::parse(&submit.instrument_id)
            .unwrap_or_else(|_| crate::instrument::InstrumentId::new(&submit.instrument_id, "BINANCE"));

        // Get current position for this instrument
        let position_qty = {
            let cache = self.cache.lock().unwrap();
            cache.get_position_for_instrument(&instrument_id)
                .map(|p| p.quantity)
                .unwrap_or(0.0)
        };

        // Get equity for drawdown calculation
        let equity = {
            let cache = self.cache.lock().unwrap();
            cache.get_equity_for_venue(&Venue::new("BINANCE"))
        };

        // Compute current drawdown percentage
        let peak_equity = self.risk_engine.lock().unwrap().peak_equity();
        let drawdown_pct = if peak_equity > 0.0 {
            ((peak_equity - equity) / peak_equity) * 100.0
        } else {
            0.0
        };

        // Get order price (limit price or 0.0 for market orders)
        let order_price = submit.price.unwrap_or(0.0);

        // Run pre-trade risk check
        if let Some(reason) = self.risk_engine.lock().unwrap().check_order(
            submit.quantity,
            order_price,
            position_qty,
            equity,
            drawdown_pct,
        ) {
            return Err(ExchangeError::RiskRejected(reason));
        }

        // Get strategy_id from submit or use default
        let strategy_id = submit.strategy_id.clone();

        // Submit to OMS (generates PositionId, persists to cache, publishes OrderSubmitted)
        let position_id = self.oms.submit_order(&submit, strategy_id);

        // Store uuid for correlation when execution report arrives
        let uuid = submit.uuid;
        self.pending_submissions.insert(submit.client_order_id.clone(), uuid);

        // Place the order via HTTP
        let venue_order_id = self.http.place_order(&submit).await?;

        Ok((venue_order_id, position_id))
    }

    /// Cancel an order via HTTP API.
    /// Publishes `OrderCancelled` to MsgBus on success.
    pub async fn cancel_order(
        &mut self,
        cancel: CancelOrder,
        _msgbus: &crate::actor::MessageBus,
    ) -> Result<bool, ExchangeError> {
        let result = self.http.cancel_order(&cancel).await?;
        Ok(result)
    }

    /// Handle an incoming WebSocket message (fills, balance updates).
    /// Updates Cache and publishes events to MsgBus.
    pub fn handle_ws_message(&mut self, msg: ExchangeWsMessage, _msgbus: &crate::actor::MessageBus) {
        self.maybe_route_to_data_engine(&msg);
        match msg {
            ExchangeWsMessage::BinanceExec(report) => {
                self.handle_execution_report_from_binance(report);
            }
            ExchangeWsMessage::BybitExec(report) => {
                self.handle_execution_report_from_bybit(report);
            }
            ExchangeWsMessage::OkxExec(report) => {
                self.handle_execution_report_from_okx(report);
            }
            ExchangeWsMessage::BinanceBalance(bal) => {
                // MISS-2 FIX: Apply exchange-reported balance update to Account.
                // This keeps Account balance in sync with exchange-reported balances.
                use crate::engine::account::Currency;
                if let Ok(total) = bal.balance.parse::<f64>() {
                    self.account.lock().unwrap().apply_balance_update(
                        Currency::new(&bal.asset),
                        total,
                        0.0, // BinanceBalanceUpdate doesn't report locked separately
                    );
                }
            }
            ExchangeWsMessage::BybitBalance(bal) => {
                // BybitBalanceUpdate has only `balance` field (no free/locked split)
                use crate::engine::account::Currency;
                if let Ok(total) = bal.balance.parse::<f64>() {
                    self.account.lock().unwrap().apply_balance_update(
                        Currency::new(&bal.asset),
                        total,
                        0.0,
                    );
                }
            }
            ExchangeWsMessage::OkxBalance(bal) => {
                // OkxBalanceUpdate has only `balance` field (no free/locked split)
                use crate::engine::account::Currency;
                if let Ok(total) = bal.balance.parse::<f64>() {
                    self.account.lock().unwrap().apply_balance_update(
                        Currency::new(&bal.asset),
                        total,
                        0.0,
                    );
                }
            }
            ExchangeWsMessage::BinanceAccount(acc) => {
                // MISS-2 FIX: Apply Binance account update (per-currency free/locked balances).
                use crate::engine::account::Currency;
                for bal in &acc.balances {
                    if let (Ok(free), Ok(locked)) = (
                        bal.free.parse::<f64>(),
                        bal.locked.parse::<f64>(),
                    ) {
                        self.account.lock().unwrap().apply_balance_update(
                            Currency::new(&bal.asset),
                            free + locked,
                            locked,
                        );
                    }
                }
            }
            // BybitAccount and OkxAccount: available when their adapters are fully wired
            ExchangeWsMessage::BybitAccount(_) | ExchangeWsMessage::OkxAccount(_) => {
                // Account updates handled separately — TODO: wire when Bybit/OKX adapters complete
            }
            ExchangeWsMessage::BinanceListStatus(status) => {
                self.handle_list_status_from_binance(status);
            }
            ExchangeWsMessage::OkxListStatus(status) => {
                self.handle_list_status_from_okx(status);
            }
            ExchangeWsMessage::ListenKeyExpired => {
                drop(self.reconnect_and_reconcile());
            }
            ExchangeWsMessage::Unknown(_) => {}
        }
    }

    fn handle_execution_report(&mut self, report: crate::live::ws_adapter::ExecutionReport) {
        let client_order_id = ClientOrderId::new(&report.client_order_id);
        let venue_order_id = VenueOrderId::new(&report.exchange_order_id.to_string());

        // Dispatch to OMS based on order status
        match report.order_status.as_str() {
            "NEW" => {
                // Order accepted by exchange — update OMS with venue_order_id
                self.oms.apply_accepted(&client_order_id, venue_order_id);
            }
            "REPLACED" => {
                // Check if this is a modification confirmation
                if let Some(modify) = self.pending_modifications.remove(&client_order_id) {
                    // Apply modification to OMS with the stored parameters
                    self.oms.apply_modify(
                        &client_order_id,
                        modify.new_price,
                        modify.new_quantity,
                    );
                } else {
                    // Not a pending modification — treat as normal acceptance
                    self.oms.apply_accepted(&client_order_id, venue_order_id);
                }
            }
            "PARTIALLY_FILLED" | "FILLED" => {
                // Fill event — build OrderFilled and apply to OMS
                let fill = self.build_order_filled(&report);
                self.oms.apply_fill(&client_order_id, &fill);

                // D.13 FIX: Remove from pending submissions on fill (no correlation needed after fill)
                self.pending_submissions.remove(&client_order_id);

                // MISS-1 FIX: Update RiskEngine with new equity after fill.
                // This drives the TradingState state machine (Active → ReduceOnly → Halted).
                // Without this call, drawdown circuit breaker and daily loss limit are non-functional.
                let equity = {
                    let cache = self.cache.lock().unwrap();
                    cache.get_equity_for_venue(&Venue::new("BINANCE"))
                };
                let instrument_id_str = fill.instrument_id.to_string();
                let instrument_id = fnv1a_hash(instrument_id_str.as_bytes());
                self.risk_engine.lock().unwrap().on_trade(instrument_id, equity);

                // MISS-2 FIX: Update Account balance after fill.
                // Account tracks balance, margin, commissions, and realized PnL.
                // Without this call, Account is inert and balance never reflects fills.
                self.account.lock().unwrap().update_with_order(
                    fill.client_order_id.to_string(),
                    fill.instrument_id.clone(),
                    fill.order_side,
                    fill.filled_qty,
                    fill.fill_price,
                    fill.commission,
                    fill.ts_event,
                );
            }
            "CANCELED" | "EXPIRED" => {
                // Order cancelled — notify OMS
                self.oms.cancel(&client_order_id);
                // D.13 FIX: Remove from pending submissions on cancel/expire
                self.pending_submissions.remove(&client_order_id);
            }
            "REJECTED" => {
                // Remove from pending modifications if present (no OMS update needed)
                // Otherwise apply normal rejection handling
                if self.pending_modifications.remove(&client_order_id).is_none() {
                    let reason = report.reject_reason.clone();
                    self.oms.apply_rejection(&client_order_id, &reason);
                }
                // D.13 FIX: Remove from pending submissions on rejection
                self.pending_submissions.remove(&client_order_id);
            }
            _ => {}
        }
    }

    fn build_order_filled(
        &self,
        report: &crate::live::ws_adapter::ExecutionReport,
    ) -> OrderFilled {
        let client_order_id = ClientOrderId::new(&report.client_order_id);

        // Look up position_id, strategy_id, instrument_id from OMS/Cache
        let (position_id, strategy_id, instrument_id_str, order_side, filled_qty, commission,
             ts_event, ts_init) = {
            let cache = self.cache.lock().unwrap();
            match cache.get_oms_order(&client_order_id) {
                Some(o) => (
                    o.position_id.clone(),
                    o.strategy_id.clone(),
                    o.instrument_id.clone(),
                    o.order_side,
                    report.last_fill_qty.parse::<f64>().unwrap_or(0.0),
                    report.commission.as_ref()
                        .and_then(|c| c.parse::<f64>().ok()).unwrap_or(0.0),
                    report.trade_time * 1_000_000,
                    report.event_time * 1_000_000,
                ),
                None => (
                    crate::messages::PositionId::new("TBD"),
                    StrategyId::new("UNKNOWN"),
                    format!("{}.BINANCE", report.symbol),
                    report.side,
                    report.last_fill_qty.parse::<f64>().unwrap_or(0.0),
                    report.commission.as_ref()
                        .and_then(|c| c.parse::<f64>().ok()).unwrap_or(0.0),
                    report.trade_time * 1_000_000,
                    report.event_time * 1_000_000,
                )
            }
        };

        OrderFilled {
            trader_id: TraderId::new("LIVE-TRADER"),
            strategy_id,
            client_order_id,
            venue_order_id: VenueOrderId::new(&report.exchange_order_id.to_string()),
            position_id,
            trade_id: crate::messages::TradeId::new(&report.trade_id.to_string()),
            instrument_id: instrument_id_str,
            order_side,
            filled_qty,
            fill_price: report.last_fill_price.parse::<f64>().unwrap_or(0.0),
            commission,
            slippage_bps: 0.0,
            is_maker: report.is_maker,
            ts_event,
            ts_init,
        }
    }

    fn handle_list_status_from_binance(&mut self, status: crate::live::exchange::BinanceListStatus) {
        // list_order_status is GROUP-LEVEL — same for all orders in OCO/OTO group
        let group_status = status.list_order_status.as_str();

        for list_order in &status.orders {
            let coid = ClientOrderId::new(&list_order.client_order_id);

            match group_status {
                "CANCELLED" => {
                    self.oms.cancel(&coid);
                    self.pending_submissions.remove(&coid);
                }
                "EXECUTED" => {
                    // Individual ExecutionReport messages already handled fills
                }
                _ => {}
            }
        }
    }

    fn handle_list_status_from_okx(&mut self, status: crate::live::exchange::OkxListStatus) {
        // OKX list status handler
        let group_status = status.list_order_status.as_str();

        for list_order in &status.orders {
            let coid = ClientOrderId::new(&list_order.client_order_id);

            match group_status {
                "CANCELLED" => {
                    self.oms.cancel(&coid);
                    self.pending_submissions.remove(&coid);
                }
                "EXECUTED" => {
                    // Individual ExecutionReport messages already handled fills
                }
                _ => {}
            }
        }
    }

    /// Route market data messages to DataEngine (wired in Phase 5.5).
    ///
    /// Market data (quotes, orderbook) arrives via a **separate**
    /// `BinanceMarketDataAdapter` WebSocket connection (e.g., `!miniTicker` /
    /// `<symbol>@depth` streams), NOT the user-data-stream WS that
    /// `ExecutionClient` connects to for fills and balances.
    ///
    /// When `BinanceMarketDataAdapter` is implemented, it will hold an
    /// `Arc<Mutex<DataEngine>>` and call `data_engine.process_quote()` /
    /// `data_engine.process_orderbook()` directly — NOT through this hook.
    ///
    /// This stub exists because `ExecutionClient` is the natural place to
    /// document the data flow; the actual wiring happens in the market-data
    /// adapter, not here.
    fn maybe_route_to_data_engine(&mut self, _msg: &ExchangeWsMessage) {
        // No-op: quote/OB data arrives via BinanceMarketDataAdapter (Phase 5.5),
        // not via the user-data-stream WS that ExecutionClient manages.
    }

    fn handle_execution_report_from_binance(
        &mut self,
        report: crate::live::exchange::BinanceExecutionReport,
    ) {
        // Build ExecutionReport from BinanceExecutionReport and dispatch to OMS
        let exec_report = crate::live::ws_adapter::ExecutionReport {
            event_type: "executionReport".to_string(),
            event_time: report.event_time,
            symbol: report.symbol.clone(),
            client_order_id: report.client_order_id.clone(),
            exchange_order_id: report.exchange_order_id,
            side: report.side,
            order_type: report.order_type,
            time_in_force: report.time_in_force,
            quantity: report.quantity,
            price: report.price,
            stop_price: report.stop_price,
            iceberg_qty: "0".to_string(),
            last_fill_qty: report.last_fill_qty,
            accumulated_qty: report.accumulated_qty,
            last_fill_price: report.last_fill_price,
            commission: report.commission,
            commission_asset: report.commission_asset,
            trade_time: report.trade_time,
            trade_id: report.trade_id,
            is_on_book: report.is_on_book,
            is_maker: report.is_maker,
            order_status: report.order_status,
            reject_reason: report.reject_reason,
            venue_order_id: report.exchange_order_id,
        };
        self.handle_execution_report(exec_report);
    }

    fn handle_execution_report_from_bybit(
        &mut self,
        report: crate::live::exchange::BybitExecutionReport,
    ) {
        // Build ExecutionReport from BybitExecutionReport
        // Bybit doesn't have time_in_force, stop_price, accumulated_qty, iceberg_qty in its report
        let exec_report = crate::live::ws_adapter::ExecutionReport {
            event_type: "orderReport".to_string(),
            event_time: report.event_time,
            symbol: report.symbol.clone(),
            client_order_id: report.client_order_id.clone(),
            exchange_order_id: report.exchange_order_id.parse().unwrap_or(0),
            side: report.side,
            order_type: report.order_type,
            time_in_force: String::new(),
            quantity: report.quantity.clone(),
            price: report.price.clone(),
            stop_price: String::new(),
            iceberg_qty: "0".to_string(),
            last_fill_qty: report.last_fill_qty.clone(),
            accumulated_qty: report.last_fill_qty, // Bybit doesn't have accumulated separate
            last_fill_price: report.last_fill_price,
            commission: report.commission,
            commission_asset: report.commission_asset,
            trade_time: report.trade_time,
            trade_id: report.trade_id.parse().unwrap_or(0),
            is_on_book: false,
            is_maker: report.is_maker,
            order_status: report.order_status,
            reject_reason: report.reject_reason,
            venue_order_id: report.exchange_order_id.parse().unwrap_or(0),
        };
        self.handle_execution_report(exec_report);
    }

    fn handle_execution_report_from_okx(
        &mut self,
        report: crate::live::exchange::OkxExecutionReport,
    ) {
        // Build ExecutionReport from OkxExecutionReport
        // OKX doesn't have time_in_force, stop_price, accumulated_qty, iceberg_qty in its report
        let exec_report = crate::live::ws_adapter::ExecutionReport {
            event_type: "orders".to_string(),
            event_time: report.event_time,
            symbol: report.symbol.clone(),
            client_order_id: report.client_order_id.clone(),
            exchange_order_id: report.exchange_order_id.parse().unwrap_or(0),
            side: report.side,
            order_type: report.order_type,
            time_in_force: String::new(),
            quantity: report.quantity.clone(),
            price: report.price.clone(),
            stop_price: String::new(),
            iceberg_qty: "0".to_string(),
            last_fill_qty: report.last_fill_qty.clone(),
            accumulated_qty: report.last_fill_qty,
            last_fill_price: report.last_fill_price,
            commission: report.commission,
            commission_asset: report.commission_asset,
            trade_time: report.trade_time,
            trade_id: report.trade_id.parse().unwrap_or(0),
            is_on_book: false,
            is_maker: report.is_maker,
            order_status: report.order_status,
            reject_reason: report.reject_reason,
            venue_order_id: report.exchange_order_id.parse().unwrap_or(0),
        };
        self.handle_execution_report(exec_report);
    }

    async fn reconnect_and_reconcile(&mut self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        // Close old WS connection
        self.ws.lock().unwrap().close().await?;

        // Get fresh listen-key from the HTTP adapter
        let listen_key = self.http.fetch_listen_key().await?;

        // Reconnect the WS adapter with the new listen key
        self.ws.lock().unwrap().reconnect(&listen_key).await?;

        // Fetch open orders from exchange
        let exchange_open = self.http.get_open_orders().await?;

        // Build list of (ClientOrderId, VenueOrderId) from exchange
        let exchange_orders: Vec<(ClientOrderId, VenueOrderId)> = exchange_open
            .iter()
            .map(|o| (
                ClientOrderId::new(&o.client_order_id),
                VenueOrderId::new(&o.order_id.to_string()),
            ))
            .collect();

        // Reconcile
        let (_oms_missing, exchange_only) = self.oms.reconcile(&exchange_orders);

        // G7 Fix: For ICEBERG/TWAP/VWAP parent orders, fetch full group state
        // to recover child orders that may not appear in get_open_orders.
        for order_info in &exchange_open {
            if order_info.order_type == "ICEBERG"
                || order_info.order_type == "TWAP"
                || order_info.order_type == "VWAP"
            {
                let coid = ClientOrderId::new(&order_info.client_order_id);
                if let Ok(status) = self.http.get_order_status(&coid, &order_info.symbol).await {
                    let strategy_id = {
                        let cache = self.cache.lock().unwrap();
                        cache.get_oms_order(&coid)
                            .map(|o| o.strategy_id.clone())
                            .unwrap_or_else(|| StrategyId::new("RECONCILED"))
                    };
                    let recovered =
                        self.build_oms_order_from_status_response(&coid, &status, &strategy_id);
                    self.oms.apply_recovered_order(recovered);
                }
            }
        }

        // Recover exchange-only orders
        for (coid, _vid) in exchange_only {
            let symbol = exchange_open.iter()
                .find(|o| o.client_order_id == coid.0)
                .map(|o| o.symbol.clone())
                .unwrap_or_default();

            if let Ok(status) = self.http.get_order_status(&coid, &symbol).await {
                let strategy_id = {
                    let cache = self.cache.lock().unwrap();
                    cache.get_oms_order(&coid)
                        .map(|o| o.strategy_id.clone())
                        .unwrap_or_else(|| StrategyId::new("RECONCILED"))
                };
                let recovered = self.build_oms_order_from_status_response(&coid, &status, &strategy_id);
                self.oms.apply_recovered_order(recovered);
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    fn build_oms_order_from_status_response(
        &self,
        client_order_id: &ClientOrderId,
        status: &crate::live::http_adapter::OrderStatusResponse,
        strategy_id: &StrategyId,
    ) -> OmsOrder {
        let instrument_id = format!("{}.BINANCE", status.symbol);
        let quantity = status.orig_qty.parse::<f64>().unwrap_or(0.0);
        let filled_qty = status.executed_qty.parse::<f64>().unwrap_or(0.0);
        let price = status.price.parse::<f64>().ok();

        let order_side = if status.side == "BUY" { OrderSide::Buy } else { OrderSide::Sell };

        let order_type = match status.order_type.as_str() {
            "MARKET" => OrderType::Market,
            "LIMIT" => OrderType::Limit,
            "STOP_LOSS" => OrderType::Stop,
            "STOP_LOSS_LIMIT" => OrderType::StopLimit,
            "ICEBERG" => OrderType::Iceberg,
            _ => OrderType::Limit,
        };

        OmsOrder {
            client_order_id: client_order_id.clone(),
            venue_order_id: Some(VenueOrderId::new(&status.order_id.to_string())),
            position_id: PositionId::new("RECONCILED"),
            state: match status.status.as_str() {
                "NEW" | "PARTIALLY_FILLED" => OrderState::Accepted,
                "FILLED" => OrderState::Filled,
                "CANCELED" | "EXPIRED" => OrderState::Cancelled,
                "REJECTED" => OrderState::Rejected,
                _ => OrderState::Pending,
            },
            filled_qty,
            last_fill_price: 0.0,
            avg_fill_price: 0.0,
            num_fills: 0,
            last_trade_ns: 0,
            submitted_at_ns: 0,
            strategy_id: strategy_id.clone(),
            instrument_id,
            order_side,
            order_type,
            quantity,
            price,
            time_in_force: None,
            expire_time_ns: None,
            last_venue_order_id: None,
        }
    }

    pub async fn modify_order(
        &mut self,
        modify: ModifyOrder,
    ) -> Result<VenueOrderId, ExchangeError> {
        // Look up the order's side and instrument from OMS
        let (order_side, instrument_id_str) = {
            let cache = self.cache.lock().unwrap();
            cache.get_oms_order(&modify.client_order_id)
                .map(|o| (o.order_side, o.instrument_id.clone()))
                .unwrap_or((OrderSide::Buy, "UNKNOWN.BINANCE".to_string()))
        };

        let symbol = instrument_id_str
            .split('.')
            .next()
            .unwrap_or(&instrument_id_str);

        // Send modification to exchange via HTTP
        let venue_order_id = self.http
            .modify_order(
                &modify.client_order_id,
                modify.venue_order_id.as_ref(),
                order_side,
                modify.new_price,
                modify.new_quantity,
                symbol,
            )
            .await?;

        // Store in pending_modifications — only apply to OMS after exchange
        // sends ExecutionReport with REPLACED status via WebSocket.
        self.pending_modifications.insert(modify.client_order_id.clone(), modify);

        Ok(venue_order_id)
    }

    #[allow(dead_code)]
    fn build_oms_order_from_status(
        &self,
        client_order_id: &ClientOrderId,
        status: &crate::live::http_adapter::OrderInfoResponse,
        strategy_id: &StrategyId,
    ) -> OmsOrder {
        let instrument_id = format!("{}.BINANCE", status.symbol);
        let quantity = status.orig_qty.parse::<f64>().unwrap_or(0.0);
        let filled_qty = status.executed_qty.parse::<f64>().unwrap_or(0.0);
        let price = status.price.parse::<f64>().ok();

        let order_side = if status.side == "BUY" { OrderSide::Buy } else { OrderSide::Sell };

        let order_type = match status.order_type.as_str() {
            "MARKET" => OrderType::Market,
            "LIMIT" => OrderType::Limit,
            "STOP_LOSS" => OrderType::Stop,
            "STOP_LOSS_LIMIT" => OrderType::StopLimit,
            "ICEBERG" => OrderType::Iceberg,
            _ => OrderType::Limit,
        };

        OmsOrder {
            client_order_id: client_order_id.clone(),
            venue_order_id: Some(VenueOrderId::new(&status.order_id.to_string())),
            position_id: PositionId::new("RECONCILED"),
            state: match status.status.as_str() {
                "NEW" | "PARTIALLY_FILLED" => OrderState::Accepted,
                "FILLED" => OrderState::Filled,
                "CANCELED" | "EXPIRED" => OrderState::Cancelled,
                "REJECTED" => OrderState::Rejected,
                _ => OrderState::Pending,
            },
            filled_qty,
            last_fill_price: 0.0,
            avg_fill_price: 0.0,
            num_fills: 0,
            last_trade_ns: status.update_time.unwrap_or(0) * 1_000_000,
            submitted_at_ns: status.time.unwrap_or(0) * 1_000_000,
            strategy_id: strategy_id.clone(),
            instrument_id,
            order_side,
            order_type,
            quantity,
            price,
            time_in_force: None,
            expire_time_ns: None,
            last_venue_order_id: None,
        }
    }
}

// =============================================================================
// Actor Trait Implementation
// =============================================================================

impl Actor for ExecutionClient {
    fn component(&self) -> &crate::actor::Component {
        &self.component
    }

    fn trader_id(&self) -> &str {
        &self.trader_id.0
    }

    fn trader_id_obj(&self) -> &TraderId {
        &self.trader_id
    }

    fn on_start(&mut self) {
        let ws = Arc::clone(&self.ws);
        let msgbus = Arc::clone(&self.msgbus);
        let name = self.component.name.to_string();

        // Spawn WS receive loop using spawn_local since MessageBus is !Send
        // The loop is self-contained and doesn't need to be Send across threads
        tokio::task::spawn_local(async move {
            let log_prefix = format!("[{}] WS Loop", name);
            loop {
                // Acquire lock and await recv while holding it
                let msg_result = {
                    let mut ws_guard = ws.lock().unwrap();
                    ws_guard.recv().await
                };

                let msg = match msg_result {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("{} error: {}, reconnecting...", log_prefix, e);
                        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
                        break;
                    }
                };

                let topic = match &msg {
                    ExchangeWsMessage::BinanceExec(_) => "execution.execution_report",
                    ExchangeWsMessage::BybitExec(_) => "execution.execution_report",
                    ExchangeWsMessage::OkxExec(_) => "execution.execution_report",
                    ExchangeWsMessage::BinanceBalance(_) => "execution.balance_update",
                    ExchangeWsMessage::BybitBalance(_) => "execution.balance_update",
                    ExchangeWsMessage::OkxBalance(_) => "execution.balance_update",
                    ExchangeWsMessage::BinanceAccount(_) => "execution.account_update",
                    ExchangeWsMessage::BybitAccount(_) => "execution.account_update",
                    ExchangeWsMessage::OkxAccount(_) => "execution.account_update",
                    ExchangeWsMessage::BinanceListStatus(_) => "execution.list_status",
                    ExchangeWsMessage::OkxListStatus(_) => "execution.list_status",
                    ExchangeWsMessage::ListenKeyExpired => "execution.listen_key_expired",
                    ExchangeWsMessage::Unknown(_) => "execution.unknown",
                };

                msgbus.send(topic, &msg);
            }
            eprintln!("{} exited", log_prefix);
        });
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
