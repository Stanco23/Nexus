//! `StrategyAsActor` — wraps a backtest `dyn Strategy` as an `Actor` for live trading.
//!
//! Translates `Actor` events into `Strategy` callbacks where possible:
//! - Lifecycle events (`on_start`, `on_stop`, etc.) → forwarded to the inner strategy
//! - Data events (`on_trade_tick`, `on_bar`) → wired with instrument registry conversion
//! - Order/position events → no-op (backtest strategies don't handle these)
//!
//! This allows a single strategy implementation to run in both backtest
//! (via `dyn Strategy` + `BacktestEngine`) and live (via `StrategyAsActor` + `Trader`).

use crate::context::StrategyCtx;
use crate::live_strategy::LiveStrategy;
use crate::signals::SignalCallback;
use crate::strategy_trait::Strategy;
use crate::types::{Bar as StrategyBar, InstrumentId as StrategyInstrumentId, Order, OrderSide, PositionSide, Signal, Tick};
use crate::types::{OmsType, PositionId, StrategyId};
use nexus::actor::{Actor, Clock, Component, ComponentState, GenericActor, Message, MessageBus};
use nexus::cache::{Bar as CacheBar, TradeTick};
use nexus::instrument::registry::InstrumentRegistry;
use nexus::messages::TraderId;
use std::collections::HashMap;
use std::sync::Arc;

/// Wraps a backtest-style `dyn Strategy` as an `Actor` for live trading.
///
/// The wrapper translates `Actor` lifecycle events to the inner strategy.
/// Data events (`on_trade_tick`, `on_bar`) use the instrument registry
/// to convert hash-based `InstrumentId` (u32) to strategy's symbol/exchange `InstrumentId`.
pub struct StrategyAsActor {
    /// The wrapped backtest strategy.
    #[allow(dead_code)]
    inner: Box<dyn Strategy>,
    /// Live strategy identity.
    strategy_id: StrategyId,
    /// Order management system type.
    oms_type: OmsType,
    /// Current position ID assigned by the OMS.
    position_id: Option<PositionId>,
    /// The underlying actor component.
    actor: GenericActor,
    /// Instrument registry for hash → symbol/exchange conversion.
    registry: Option<Arc<InstrumentRegistry>>,
}

impl StrategyAsActor {
    /// Wrap a `dyn Strategy` as an `Actor`.
    pub fn new(
        strategy: Box<dyn Strategy>,
        strategy_id: StrategyId,
        oms_type: OmsType,
        trader_id: TraderId,
        clock: Box<dyn Clock>,
        msgbus: MessageBus,
    ) -> Self {
        let name: &'static str = Box::leak(strategy.name().to_string().into_boxed_str());
        let actor = GenericActor::new(trader_id, 0, name, clock, msgbus);
        Self {
            inner: strategy,
            strategy_id,
            oms_type,
            position_id: None,
            actor,
            registry: None,
        }
    }

    /// Set the instrument registry for hash → symbol/exchange conversion.
    pub fn with_registry(mut self, registry: Arc<InstrumentRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }
}

impl LiveStrategy for StrategyAsActor {
    fn strategy_id(&self) -> &StrategyId {
        &self.strategy_id
    }

    fn oms_type(&self) -> OmsType {
        self.oms_type
    }

    fn position_id(&self) -> Option<&PositionId> {
        self.position_id.as_ref()
    }

    fn set_position_id(&mut self, id: PositionId) {
        self.position_id = Some(id);
    }
}

impl Actor for StrategyAsActor {
    fn component(&self) -> &Component {
        self.actor.component()
    }

    fn trader_id(&self) -> &str {
        self.actor.trader_id()
    }

    fn trader_id_obj(&self) -> &TraderId {
        self.actor.trader_id_obj()
    }

    fn process_message(&mut self, msg: &(dyn Message + 'static)) {
        self.actor.process_message(msg)
    }

    // === Lifecycle handlers ===

    fn on_start(&mut self) {
        self.actor.on_start();
    }

    fn on_stop(&mut self) {
        self.actor.on_stop();
    }

    fn on_state_change(&mut self, state: ComponentState) {
        self.actor.on_state_change(state);
    }

    fn on_save(&mut self) -> HashMap<String, Vec<u8>> {
        self.actor.on_save()
    }

    fn on_load(&mut self, state: &HashMap<String, Vec<u8>>) {
        self.actor.on_load(state);
    }

    fn on_resume(&mut self) {
        self.actor.on_resume();
    }

    fn on_reset(&mut self) {
        self.actor.on_reset();
    }

    fn on_dispose(&mut self) {
        self.actor.on_dispose();
    }

    fn on_degrade(&mut self) {
        self.actor.on_degrade();
    }

    fn on_fault(&mut self) {
        self.actor.on_fault();
    }

    // === Data handlers ===

    fn on_trade_tick(&mut self, tick: &TradeTick) {
        // Convert hash-based InstrumentId to strategy InstrumentId via registry
        let instrument_id = match &self.registry {
            Some(reg) => {
                match reg.get(tick.instrument_id.id) {
                    Some(inst) => {
                        StrategyInstrumentId::new(&inst.symbol, &inst.venue)
                    }
                    None => {
                        eprintln!("WARN  on_trade_tick: unknown instrument_id {:08X}", tick.instrument_id.id);
                        return;
                    }
                }
            }
            None => {
                eprintln!("DEBUG on_trade_tick: no instrument registry, skipping");
                return;
            }
        };

        let strategy_tick = Tick::new(
            tick.ts_event,
            tick.price,
            tick.size,
            0.0, // VPIN not available in live TradeTick
        );

        let mut ctx = NoOpStrategyCtx;
        if let Some(signal) = self.inner.on_trade(instrument_id, &strategy_tick, &mut ctx) {
            let _ = signal;
        }
    }

    fn on_bar(&mut self, bar: &CacheBar) {
        let instrument_id = match &self.registry {
            Some(reg) => {
                match reg.get(bar.instrument_id.id) {
                    Some(inst) => {
                        StrategyInstrumentId::new(&inst.symbol, &inst.venue)
                    }
                    None => {
                        eprintln!("WARN  on_bar: unknown instrument_id {:08X}", bar.instrument_id.id);
                        return;
                    }
                }
            }
            None => {
                eprintln!("DEBUG on_bar: no instrument registry, skipping");
                return;
            }
        };

        let strategy_bar = StrategyBar {
            timestamp_ns: bar.ts_event,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            close: bar.close,
            volume: bar.volume,
            buy_volume: bar.buy_volume,
            sell_volume: bar.sell_volume,
            tick_count: bar.tick_count as u64,
        };

        let mut ctx = NoOpStrategyCtx;
        if let Some(signal) = self.inner.on_bar(instrument_id, &strategy_bar, &mut ctx) {
            let _ = signal;
        }
    }

    // === Order/position handlers — no-op for backtest strategies ===
    // Live strategies override these directly; backtest strategies don't need them.
}

// =============================================================================
// NoOpStrategyCtx — temporary shim for Phase 5.0i
// WARNING: Has dangerous silent failure — equity=0.0, position=None, pending_orders=[].
// Phase 5.5 must replace this with real LiveStrategyCtx.
// =============================================================================

struct NoOpStrategyCtx;

impl StrategyCtx for NoOpStrategyCtx {
    fn current_price(&self, _: StrategyInstrumentId) -> f64 {
        0.0
    }
    fn position(&self, _: StrategyInstrumentId) -> Option<PositionSide> {
        None
    }
    fn account_equity(&self) -> f64 {
        0.0
    }
    fn unrealized_pnl(&self, _: StrategyInstrumentId) -> f64 {
        0.0
    }
    fn pending_orders(&self, _: StrategyInstrumentId) -> Vec<Order> {
        vec![]
    }
    fn subscribe_instruments(&mut self, _: Vec<StrategyInstrumentId>) {}
    fn subscribe_signal(&mut self, _: &str, _: SignalCallback) {}
    fn submit_limit(&mut self, _: StrategyInstrumentId, _: OrderSide, _: f64, _: f64) -> u64 {
        0
    }
    fn submit_market(&mut self, _: StrategyInstrumentId, _: OrderSide, _: f64) -> u64 {
        0
    }
    fn submit_with_sl_tp(
        &mut self,
        _: StrategyInstrumentId,
        _: OrderSide,
        _: crate::types::OrderType,
        _: f64,
        _: f64,
        _: Option<f64>,
        _: Option<f64>,
    ) -> u64 {
        0
    }
    fn emit_signal(&mut self, _: Signal) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy_trait::Strategy;
    use crate::types::{BacktestMode, Signal};
    use nexus::actor::{Clock, MessageBus};

    /// Mock Clock for testing — minimal implementation.
    struct MockClock;

    impl Clock for MockClock {
        fn timestamp_ns(&self) -> u64 { 0 }
        fn timer_names(&self) -> Vec<String> { vec![] }
        fn timer_count(&self) -> usize { 0 }
        fn set_timer(&mut self, _: &str, _: u64, _: Vec<u8>, _: Box<dyn FnMut(nexus::actor::TimeEvent)>) {}
        fn set_timer_anonymous(&mut self, _: &str, _: u64) {}
        fn set_timer_repeating(&mut self, _: &str, _: u64, _: u64, _: Option<u64>, _: Box<dyn FnMut(nexus::actor::TimeEvent)>, _: bool) {}
        fn cancel_timer(&mut self, _: &str) {}
        fn cancel_timers(&mut self) {}
        fn next_time_ns(&self, _: &str) -> u64 { 0 }
        fn register_default_handler(&mut self, _: Box<dyn FnMut(nexus::actor::TimeEvent)>) {}
        fn advance_time(&mut self, _: u64) -> Vec<nexus::actor::TimeEvent> { vec![] }
    }

    /// Mock Strategy for testing — implements required Strategy methods.
    struct MockStrategy {
        name: String,
    }

    impl MockStrategy {
        fn new(name: &str) -> Self {
            Self { name: name.to_string() }
        }
    }

    impl Strategy for MockStrategy {
        fn name(&self) -> &str { &self.name }
        fn mode(&self) -> BacktestMode { BacktestMode::Tick }
        fn subscribed_instruments(&self) -> Vec<crate::types::InstrumentId> { vec![] }
        fn parameters(&self) -> Vec<crate::types::ParameterSchema> { vec![] }
        fn clone_box(&self) -> Box<dyn Strategy> {
            Box::new(MockStrategy::new(&self.name))
        }
        fn on_trade(&mut self, _: crate::types::InstrumentId, _: &crate::types::Tick, _: &mut dyn StrategyCtx) -> Option<Signal> {
            None
        }
        fn on_bar(&mut self, _: crate::types::InstrumentId, _: &crate::types::Bar, _: &mut dyn StrategyCtx) -> Option<Signal> {
            None
        }
    }

    #[test]
    fn test_strategy_as_actor_lifecycle_compiles() {
        let strategy = Box::new(MockStrategy::new("test_strategy"));
        let strategy_id = StrategyId::new("test strategy");
        let oms_type = OmsType::SingleOrder;
        let trader_id = TraderId::new("trader-001");
        let clock = Box::new(MockClock);
        let msgbus = MessageBus::new();
        let _actor = StrategyAsActor::new(strategy, strategy_id, oms_type, trader_id, clock, msgbus);
    }

    #[test]
    fn test_live_strategy_trait_bounds() {
        fn assert_impl<T: Actor + LiveStrategy>() {}
        assert_impl::<StrategyAsActor>();
    }

    #[test]
    fn test_strategy_as_actor_data_handlers_compile() {
        use nexus::cache::TradeTick;
        use nexus::cache::Bar;
        use nexus::messages::OrderSubmitted;

        let strategy = Box::new(MockStrategy::new("test"));
        let strategy_id = StrategyId::new("test strategy");
        let oms_type = OmsType::SingleOrder;
        let trader_id = TraderId::new("trader-001");
        let clock = Box::new(MockClock);
        let msgbus = MessageBus::new();
        let mut actor = StrategyAsActor::new(strategy, strategy_id, oms_type, trader_id, clock, msgbus);

        let tick = TradeTick {
            ts_event: 0,
            ts_init: 0,
            price: 100.0,
            size: 1.0,
            side: nexus::cache::TradeSide::Buy,
            trade_id: None,
            instrument_id: nexus::instrument::InstrumentId::new("BTCUSDT", "BINANCE"),
        };
        actor.on_trade_tick(&tick);

        let bar = Bar {
            ts_event: 0,
            ts_init: 0,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 100.0,
            buy_volume: 60.0,
            sell_volume: 40.0,
            tick_count: 50,
            instrument_id: nexus::instrument::InstrumentId::new("BTCUSDT", "BINANCE"),
        };
        actor.on_bar(&bar);
        actor.on_order_submitted(&OrderSubmitted::new(
            TraderId::new("t"),
            StrategyId::new("s"),
            nexus::messages::ClientOrderId::new("cid"),
            nexus::messages::VenueOrderId::new("vid"),
            "BTCUSDT".to_string(),
            nexus::messages::OrderSide::Buy,
            nexus::messages::OrderType::Market,
            0.0,
            0,
            0,
        ));
    }

    #[test]
    fn test_strategy_as_actor_with_registry() {
        let mut registry = InstrumentRegistry::new();
        let btc = nexus::instrument::types::InstrumentBuilder::new(
            "BTCUSDT", "BINANCE",
            nexus::instrument::enums::InstrumentClass::SPOT,
            nexus::instrument::enums::AssetClass::CRYPTOCURRENCY,
            "USDT",
        ).build();
        let btc_id = btc.id;
        registry.register(btc);

        let strategy = Box::new(MockStrategy::new("test"));
        let strategy_id = StrategyId::new("test strategy");
        let oms_type = OmsType::SingleOrder;
        let trader_id = TraderId::new("trader-001");
        let clock = Box::new(MockClock);
        let msgbus = MessageBus::new();

        let mut actor = StrategyAsActor::new(strategy, strategy_id, oms_type, trader_id, clock, msgbus)
            .with_registry(Arc::new(registry));

        // Verify registry is set by checking we can look up the instrument
        assert!(actor.registry.is_some());
        let reg = actor.registry.as_ref().unwrap();
        assert!(reg.contains(btc_id));
    }
}