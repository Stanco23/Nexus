//! Paper broker — simulated order execution using OrderEmulator.

use std::sync::{Arc, Mutex};

use crate::actor::MessageBus;
use crate::book::{FillEvent, OrderBook, OrderEmulator, Side as BookSide};
use crate::buffer::tick_buffer::TradeFlowStats;
use crate::cache::Cache;
use crate::engine::account::{Account, Currency, OmsType};
use crate::engine::oms::Oms;
use crate::instrument::{InstrumentId, Venue};
use crate::messages::{
    CancelOrder, ClientOrderId, OrderFilled, OrderSide,
    PositionId, StrategyId, SubmitOrder, TraderId, VenueOrderId, TradeId,
};

/// Paper trade record for the trade log.
#[derive(Debug, Clone)]
pub struct PaperTrade {
    pub timestamp_ns: u64,
    pub client_order_id: ClientOrderId,
    pub instrument_id: InstrumentId,
    pub venue: String,
    pub side: OrderSide,
    pub fill_price: f64,
    pub size: f64,
    pub commission: f64,
    pub slippage_bps: f64,
}

/// Paper broker — simulated order execution using OrderEmulator.
pub struct PaperBroker {
    emulator: OrderEmulator,
    order_book: OrderBook,
    #[allow(dead_code)]
    cache: Arc<Mutex<Cache>>,
    account: Account,
    oms: Oms,
    paper_trades: Vec<PaperTrade>,
    taker_fee: f64,
    maker_fee: f64,
    /// Maps OrderEmulator order_id → pending order metadata for limit fills.
    /// This allows on_trade (which receives fills by emulator order_id) to look up
    /// the real client_order_id, position_id, and instrument_id.
    pending_limit_orders: std::collections::HashMap<u64, PendingPaperOrder>,
}

/// Minimal order metadata needed to resolve fills back to their source order.
#[derive(Clone)]
struct PendingPaperOrder {
    client_order_id: ClientOrderId,
    position_id: PositionId,
    instrument_id: InstrumentId,
    order_side: OrderSide,
    #[allow(dead_code)]
    strategy_id: StrategyId,
}

impl PaperBroker {
    /// Create a new PaperBroker.
    pub fn new(
        cache: Arc<Mutex<Cache>>,
        account: Account,
        slippage_config: crate::slippage::SlippageConfig,
        taker_fee: f64,
        maker_fee: f64,
        msgbus: Arc<MessageBus>,
        oms_type: OmsType,
    ) -> Self {
        let oms = Oms::new(cache.clone(), msgbus, oms_type, None);
        Self {
            emulator: OrderEmulator::new_with_config(slippage_config),
            order_book: OrderBook::default(),
            cache,
            account,
            oms,
            paper_trades: Vec::new(),
            taker_fee,
            maker_fee,
            pending_limit_orders: std::collections::HashMap::new(),
        }
    }

    fn to_book_side(side: OrderSide) -> Option<BookSide> {
        match side {
            OrderSide::Buy => Some(BookSide::Buy),
            OrderSide::Sell => Some(BookSide::Sell),
        }
    }

    fn to_messages_side(side: BookSide) -> OrderSide {
        match side {
            BookSide::Buy => OrderSide::Buy,
            BookSide::Sell => OrderSide::Sell,
        }
    }

    fn make_trade_id(order_id: u64, timestamp_ns: u64) -> TradeId {
        TradeId::new(&format!("PAPER-{}-{}", order_id, timestamp_ns))
    }

    /// Process a submitted order (market or limit).
    /// Returns OrderFilled events for each fill.
    pub fn process_order(&mut self, submit: SubmitOrder) -> Vec<OrderFilled> {
        let ts = submit.ts_init;
        let client_order_id = submit.client_order_id.clone();
        // SubmitOrder.instrument_id is a String — we need InstrumentId
        // Parse from string or create a placeholder (0 id, which is "UNKNOWN")
        let instrument_id_str = &submit.instrument_id;
        let instrument_id = InstrumentId::parse(instrument_id_str)
            .unwrap_or_else(|_| InstrumentId::new("UNKNOWN", "PAPER"));

        // Extract venue from instrument_id string (format "SYMBOL.VENUE")
        let venue_str = instrument_id_str
            .split('.')
            .nth(1)
            .unwrap_or("PAPER");
        let _venue = Venue::new(venue_str);
        let strategy_id = submit.strategy_id.clone();

        // Submit via OMS — generates position_id and publishes OrderSubmitted
        let position_id = self.oms.submit_order(&submit, strategy_id.clone());

        match submit.order_type {
            crate::messages::OrderType::Market => {
                let book_side = match Self::to_book_side(submit.order_side) {
                    Some(s) => s,
                    None => return vec![],
                };
                let fills = self.emulator.process_market_order(
                    submit.quantity,
                    book_side,
                    &self.order_book,
                    ts,
                    self.taker_fee,
                );
                let instrument_id_cloned = instrument_id.clone();
                    fills
                    .into_iter()
                    .map(|e| {
                        let filled = self.record_fill(
                            e,
                            client_order_id.clone(),
                            position_id.clone(),
                            instrument_id_cloned.clone(),
                            submit.order_side,
                            false,
                            venue_str.to_string(),
                        );
                        // Update OMS state without publishing (PaperBroker already published)
                        self.oms.apply_fill_no_publish(&client_order_id, &filled);
                        filled
                    })
                    .collect()
            }
            crate::messages::OrderType::Limit => {
                let book_side = match Self::to_book_side(submit.order_side) {
                    Some(s) => s,
                    None => return vec![],
                };
                // MISS-3 FIX: Store order_id → metadata mapping so on_trade can resolve fills
                let order_id = self.emulator.submit_limit(
                    submit.price.unwrap_or(0.0),
                    submit.quantity,
                    book_side,
                    ts,
                );
                self.pending_limit_orders.insert(order_id, PendingPaperOrder {
                    client_order_id: client_order_id.clone(),
                    position_id: position_id.clone(),
                    instrument_id,
                    order_side: submit.order_side,
                    strategy_id: strategy_id.clone(),
                });

                // Check immediate fill (limit price may already be crossed)
                let fills = self.emulator.process_fills(
                    self.order_book.last_price,
                    self.order_book.vpin,
                    ts,
                    self.maker_fee,
                );
                let mut results = Vec::new();
                for fill in fills {
                    // MISS-3 FIX: Look up real IDs from pending_limit_orders
                    if let Some(pending) = self.pending_limit_orders.remove(&fill.order_id) {
                        let filled = self.record_fill(
                            fill,
                            pending.client_order_id.clone(),
                            pending.position_id,
                            pending.instrument_id,
                            pending.order_side,
                            true,
                            venue_str.to_string(),
                        );
                        // Update OMS state without publishing
                        self.oms.apply_fill_no_publish(&pending.client_order_id, &filled);
                        results.push(filled);
                    }
                }
                results
            }
            _ => vec![],
        }
    }

    /// Cancel a pending limit order.
    /// Returns true if the order was found and removed.
    pub fn cancel_order(&mut self, cancel: CancelOrder) -> bool {
        // Also remove from pending_limit_orders so cancelled orders don't still get filled
        // We need to find the order_id by client_order_id — iterate to find match
        let order_id_to_remove = self.pending_limit_orders.iter()
            .find(|(_, pending)| pending.client_order_id == cancel.client_order_id)
            .map(|(id, _)| *id);
        if let Some(order_id) = order_id_to_remove {
            self.pending_limit_orders.remove(&order_id);
        }
        self.oms.cancel(&cancel.client_order_id)
    }

    /// Called by DataEngine when a new trade tick arrives.
    pub fn on_trade(&mut self, tick: &TradeFlowStats) {
        // Update synthetic book from the tick
        self.order_book.update_from_trade(tick);

        // Check if any pending limit orders should fill at the new price
        let fills = self.emulator.process_fills(
            self.order_book.last_price,
            self.order_book.vpin,
            tick.timestamp_ns,
            self.maker_fee,
        );

        for fill in fills {
            // MISS-3 FIX: Look up real IDs from pending_limit_orders mapping
            if let Some(pending) = self.pending_limit_orders.remove(&fill.order_id) {
                let coid = pending.client_order_id.clone();
                let filled = self.record_fill(
                    fill,
                    coid.clone(),
                    pending.position_id,
                    pending.instrument_id,
                    pending.order_side,
                    true,
                    "PAPER".to_string(),
                );
                // Update OMS state
                self.oms.apply_fill_no_publish(&coid, &filled);
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn record_fill(
        &mut self,
        fill: FillEvent,
        client_order_id: ClientOrderId,
        position_id: PositionId,
        instrument_id: InstrumentId,
        order_side: OrderSide,
        is_maker: bool,
        venue: String,
    ) -> OrderFilled {
        // Record in paper trades log
        self.paper_trades.push(PaperTrade {
            timestamp_ns: fill.timestamp_ns,
            client_order_id: client_order_id.clone(),
            instrument_id: instrument_id.clone(),
            venue: venue.clone(),
            side: Self::to_messages_side(fill.side),
            fill_price: fill.fill_price,
            size: fill.fill_size,
            commission: fill.fee,
            slippage_bps: fill.slippage_bps,
        });

        // Update account: apply commission
        let currency = Currency::new("USDT");
        self.account.apply_commission(&currency, fill.fee);

        // Apply realized P&L based on side
        let pnl = if fill.side == BookSide::Sell {
            fill.fill_price * fill.fill_size - fill.fee
        } else {
            -(fill.fill_price * fill.fill_size + fill.fee)
        };

        // Update balance for realized P&L
        if let Some(balance) = self.account.balances.get_mut(&currency) {
            balance.total += pnl;
            balance.available += pnl;
        }

        OrderFilled {
            trader_id: TraderId::new("PAPER-TRADER"),
            strategy_id: StrategyId::new("PAPER-STRATEGY"),
            client_order_id,
            venue_order_id: VenueOrderId::new(&format!("PAPER-{}", fill.order_id)),
            position_id,
            trade_id: Self::make_trade_id(fill.order_id, fill.timestamp_ns),
            instrument_id: format!("{}.{}", instrument_id.id, venue),
            order_side,
            filled_qty: fill.fill_size,
            fill_price: fill.fill_price,
            commission: fill.fee,
            slippage_bps: fill.slippage_bps,
            is_maker,
            ts_event: fill.timestamp_ns,
            ts_init: fill.timestamp_ns,
        }
    }

    /// Get current paper equity.
    pub fn paper_equity(&self) -> f64 {
        self.account.equity()
    }

    /// Get paper trade history.
    pub fn paper_trades(&self) -> &[PaperTrade] {
        &self.paper_trades
    }
}
