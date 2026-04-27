//! Paper broker — simulated order execution using OrderEmulator.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use parking_lot::Mutex;
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
use crate::paper::PaperExecution;

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
    emulator: Mutex<OrderEmulator>,
    order_book: Mutex<OrderBook>,
    #[allow(dead_code)]
    cache: Arc<Mutex<Cache>>,
    account: Mutex<Account>,
    oms: Oms,
    paper_trades: Mutex<Vec<PaperTrade>>,
    taker_fee: f64,
    maker_fee: f64,
    /// Maps OrderEmulator order_id → pending order metadata for limit fills.
    /// This allows on_trade (which receives fills by emulator order_id) to look up
    /// the real client_order_id, position_id, and instrument_id.
    pending_limit_orders: Mutex<std::collections::HashMap<u64, PendingPaperOrder>>,
    /// Simulated execution latency in nanoseconds.
    latency_ns: u64,
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
        let oms_cache = Arc::new(StdMutex::new(Cache::new(1000, 1000)));
        let oms = Oms::new(oms_cache, msgbus.clone(), oms_type, None);
        Self {
            emulator: Mutex::new(OrderEmulator::new_with_config(slippage_config)),
            order_book: Mutex::new(OrderBook::default()),
            cache,
            account: Mutex::new(account),
            oms,
            paper_trades: Mutex::new(Vec::new()),
            taker_fee,
            maker_fee,
            pending_limit_orders: Mutex::new(std::collections::HashMap::new()),
            latency_ns: 0,
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
        // Apply simulated latency delay if configured
        if self.latency_ns > 0 {
            std::thread::sleep(Duration::from_nanos(self.latency_ns));
        }

        let ts = submit.ts_init;
        let client_order_id = submit.client_order_id.clone();
        let instrument_id_str = &submit.instrument_id;
        let instrument_id = InstrumentId::parse(instrument_id_str)
            .unwrap_or_else(|_| InstrumentId::new("UNKNOWN", "PAPER"));

        let venue_str = instrument_id_str
            .split('.')
            .nth(1)
            .unwrap_or("PAPER");
        let _venue = Venue::new(venue_str);
        let strategy_id = submit.strategy_id.clone();

        let position_id = self.oms.submit_order(&submit, strategy_id.clone());

        match submit.order_type {
            crate::messages::OrderType::Market => {
                let book_side = match Self::to_book_side(submit.order_side) {
                    Some(s) => s,
                    None => return vec![],
                };
                // Lock order_book first, then emulator (consistent lock order)
                let book = self.order_book.lock();
                let fills = self.emulator.lock().process_market_order(
                    submit.quantity,
                    book_side,
                    &book,
                    ts,
                    self.taker_fee,
                );
                drop(book);
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
                let order_id = self.emulator.lock().submit_limit(
                    submit.price.unwrap_or(0.0),
                    submit.quantity,
                    book_side,
                    ts,
                );
                self.pending_limit_orders.lock().insert(order_id, PendingPaperOrder {
                    client_order_id: client_order_id.clone(),
                    position_id: position_id.clone(),
                    instrument_id,
                    order_side: submit.order_side,
                    strategy_id: strategy_id.clone(),
                });

                // Check immediate fill
                let book = self.order_book.lock();
                let vpin = book.vpin;
                let last_price = book.last_price;
                let fills = self.emulator.lock().process_fills(
                    last_price,
                    vpin,
                    ts,
                    self.maker_fee,
                    &book,
                );
                drop(book);
                let mut results = Vec::new();
                for fill in fills {
                    let pending = {
                        let mut pending_map = self.pending_limit_orders.lock();
                        pending_map.remove(&fill.order_id)
                    };
                    if let Some(pending) = pending {
                        let filled = self.record_fill(
                            fill,
                            pending.client_order_id.clone(),
                            pending.position_id,
                            pending.instrument_id,
                            pending.order_side,
                            true,
                            venue_str.to_string(),
                        );
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
    pub fn cancel_order(&mut self, cancel: CancelOrder) -> bool {
        let order_id_to_remove = {
            let pending = self.pending_limit_orders.lock();
            pending.iter()
                .find(|(_, p)| p.client_order_id == cancel.client_order_id)
                .map(|(id, _)| *id)
        };
        if let Some(order_id) = order_id_to_remove {
            self.pending_limit_orders.lock().remove(&order_id);
        }
        self.oms.cancel(&cancel.client_order_id)
    }

    /// Called by DataEngine when a new trade tick arrives.
    pub fn on_trade(&mut self, tick: &TradeFlowStats) {
        // Update order book
        self.order_book.lock().update_from_trade(tick);

        // Check pending limit fills
        let book = self.order_book.lock();
        let vpin = book.vpin;
        let last_price = book.last_price;
        let fills = self.emulator.lock().process_fills(
            last_price,
            vpin,
            tick.timestamp_ns,
            self.maker_fee,
            &book,
        );
        drop(book);

        for fill in fills {
            let pending = {
                let mut pending_map = self.pending_limit_orders.lock();
                pending_map.remove(&fill.order_id)
            };
            if let Some(pending) = pending {
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
        self.paper_trades.lock().push(PaperTrade {
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

        // Update account
        let currency = Currency::new("USDT");
        self.account.lock().apply_commission(&currency, fill.fee);

        let pnl = if fill.side == BookSide::Sell {
            fill.fill_price * fill.fill_size - fill.fee
        } else {
            -(fill.fill_price * fill.fill_size + fill.fee)
        };

        if let Some(balance) = self.account.lock().balances.get_mut(&currency) {
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
        self.account.lock().equity()
    }

    /// Get paper trade history.
    pub fn trades(&self) -> Vec<PaperTrade> {
        self.paper_trades.lock().clone()
    }
}

// =============================================================================
// PaperExecution implementation
// =============================================================================

impl PaperExecution for PaperBroker {
    fn submit_order(&mut self, submit: &SubmitOrder) -> Vec<OrderFilled> {
        self.process_order(submit.clone())
    }

    fn cancel_order(&mut self, cancel: &CancelOrder) -> bool {
        let order_id_to_remove = {
            let pending = self.pending_limit_orders.lock();
            pending.iter()
                .find(|(_, p)| p.client_order_id == cancel.client_order_id)
                .map(|(id, _)| *id)
        };
        if let Some(order_id) = order_id_to_remove {
            self.pending_limit_orders.lock().remove(&order_id);
        }
        self.oms.cancel(&cancel.client_order_id)
    }

    fn apply_trade(&mut self, trade: &TradeFlowStats) {
        self.on_trade(trade);
    }

    fn seed_order_book(&mut self, bids: &[(f64, f64)], asks: &[(f64, f64)]) {
        self.order_book.lock().seed_from_l2(bids, asks);
    }

    fn latency_ns(&self) -> u64 {
        self.latency_ns
    }

    fn set_latency_ns(&mut self, latency_ns: u64) {
        self.latency_ns = latency_ns;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_broker_internal(cache: Arc<Mutex<Cache>>) -> PaperBroker {
        let account = Account::new(
            crate::engine::account::AccountId::new("PAPER-ACCT-001"),
            Some(crate::engine::account::Currency::new("USDT")),
            std::collections::HashMap::new(),
            crate::engine::account::OmsType::Hedge,
            10.0,
        );
        let msgbus = Arc::new(MessageBus::new());
        PaperBroker::new(
            cache,
            account,
            crate::slippage::SlippageConfig::default(),
            0.0005,
            0.0002,
            msgbus,
            crate::engine::account::OmsType::Hedge,
        )
    }

    fn make_test_broker() -> PaperBroker {
        let cache = Arc::new(Mutex::new(Cache::new(1000, 1000)));
        make_test_broker_internal(cache)
    }

    #[test]
    fn test_paper_broker_latency_injection() {
        let mut broker = make_test_broker();
        broker.set_latency_ns(100_000_000);

        let submit = SubmitOrder::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            "BTCUSDT.BINANCE".to_string(),
            ClientOrderId::new("LATENCY-TEST-001"),
            OrderSide::Buy,
            crate::messages::OrderType::Market,
            0.001,
            1_000_000_000,
        );

        let start = std::time::Instant::now();
        let fills = broker.process_order(submit);
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_millis() >= 100,
            "Expected >= 100ms latency, got {}ms",
            elapsed.as_millis()
        );
        let _ = fills;
    }

    #[test]
    fn test_paper_broker_seed_order_book() {
        let mut broker = make_test_broker();

        let bids = &[(100.0, 1.0), (99.0, 2.0)];
        let asks = &[(101.0, 1.5), (102.0, 0.5)];

        broker.seed_order_book(bids, asks);

        let book = broker.order_book.lock();
        assert_eq!(book.best_bid(), Some(100.0));
        assert_eq!(book.best_ask(), Some(101.0));
    }

    #[test]
    fn test_paper_execution_trait_impl() {
        let mut broker = make_test_broker();
        broker.set_latency_ns(0);

        let submit = SubmitOrder::new(
            TraderId::new("TRADER-001"),
            StrategyId::new("TEST-STRAT"),
            "ETHUSDT.BINANCE".to_string(),
            ClientOrderId::new("TRAIT-TEST-001"),
            OrderSide::Buy,
            crate::messages::OrderType::Market,
            0.01,
            1_000_000_000,
        );

        let fills = PaperExecution::submit_order(&mut broker, &submit);
        let _ = fills;

        assert_eq!(PaperExecution::latency_ns(&broker), 0);
        PaperExecution::set_latency_ns(&mut broker, 50_000_000);
        assert_eq!(PaperExecution::latency_ns(&broker), 50_000_000);
    }
}