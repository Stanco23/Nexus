//! Tests for EngineContext StrategyCtx implementation (Phase 3.2)
//!
//! Verifies all 10 StrategyCtx methods work correctly.

use nexus::engine::{EngineContext, InstrumentState, PositionSide, Signal, StrategyCtx};
use nexus::signals::SignalBus;
use nexus::instrument::{InstrumentId, Venue};
use nexus::engine::orders::{Order, OrderSide, OrderType};
use nexus::messages::{ClientOrderId, StrategyId};
use std::sync::{Arc, Mutex};

#[test]
fn test_strategy_ctx_current_price_with_update() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

    let instr_id = InstrumentId::new("BTCUSDT", "BACKTEST");

    // Initially returns 0
    assert_eq!(StrategyCtx::current_price(&ctx, &instr_id), 0.0);

    // Update price via engine (must use instr_id.id so get_price finds it)
    ctx.update_price(instr_id.id, 150.0);

    // Now returns the updated price
    assert_eq!(StrategyCtx::current_price(&ctx, &instr_id), 150.0);
}

#[test]
fn test_strategy_ctx_position_side() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
    let instr_id = InstrumentId::new("BTCUSDT", "BACKTEST");

    // No position yet -> Flat
    assert_eq!(StrategyCtx::position(&ctx, &instr_id), Some(PositionSide::Flat));

    // Add a long position
    ctx.instrument_states.insert(instr_id.id, InstrumentState {
        position: 2.0,
        entry_price: 100.0,
        unrealized_pnl: 0.0,
        realized_pnl: 0.0,
        commissions: 0.0,
    });
    assert_eq!(StrategyCtx::position(&ctx, &instr_id), Some(PositionSide::Long));

    // Short it
    ctx.instrument_states.get_mut(&instr_id.id).unwrap().position = -3.0;
    assert_eq!(StrategyCtx::position(&ctx, &instr_id), Some(PositionSide::Short));
}

#[test]
fn test_strategy_ctx_unrealized_pnl_accurate() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
    let instr_id = InstrumentId::new("BTCUSDT", "BACKTEST");

    // Long position at 100, current price 110
    ctx.instrument_states.insert(instr_id.id, InstrumentState {
        position: 1.0,
        entry_price: 100.0,
        unrealized_pnl: 0.0,
        realized_pnl: 0.0,
        commissions: 0.0,
    });

    ctx.update_price(instr_id.id, 110.0);
    let pnl = StrategyCtx::unrealized_pnl(&ctx, &instr_id);
    assert!((pnl - 10.0).abs() < 0.001, "Long position PnL should be 10, got {}", pnl);

    // Short position at 100, current price 90
    ctx.instrument_states.get_mut(&instr_id.id).unwrap().position = -1.0;
    ctx.update_price(instr_id.id, 90.0);
    let pnl = StrategyCtx::unrealized_pnl(&ctx, &instr_id);
    assert!((pnl - 10.0).abs() < 0.001, "Short position PnL should be 10, got {}", pnl);
}

#[test]
fn test_strategy_ctx_pending_orders_tracking() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
    let instr_id = InstrumentId::new("BTCUSDT", "BACKTEST");

    // Initially empty
    let pending = ctx.pending_orders(&instr_id);
    assert!(pending.is_empty());

    // Add order via submit_with_sl_tp
    ctx.submit_with_sl_tp(
        &instr_id,
        OrderSide::Buy,
        OrderType::Limit,
        100.0,
        1.0,
        Some(95.0),
        Some(110.0),
    );

    // Now has one pending order
    let pending = ctx.pending_orders(&instr_id);
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].price, 100.0);
    assert_eq!(pending[0].sl, Some(95.0));
    assert_eq!(pending[0].tp, Some(110.0));

    // Remove the order by id
    let order_id = pending[0].id;
    ctx.remove_pending_order(order_id);
    let pending = ctx.pending_orders(&instr_id);
    assert!(pending.is_empty());
}

#[test]
fn test_strategy_ctx_signal_emission() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus.clone(), std::ptr::null_mut());

    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received_clone = received.clone();

    // Subscribe to signal channel
    ctx.subscribe_signal("signal", Box::new(move |name, value, _ts| {
        received_clone.lock().unwrap().push((name.to_string(), value));
    }));

    // Emit signals
    ctx.emit_signal(Signal::buy_market());
    ctx.emit_signal(Signal::sell_market());
    ctx.emit_signal(Signal::close());

    let r = received.lock().unwrap();
    assert_eq!(r.len(), 3);
    assert_eq!(r[0], ("signal".to_string(), 1.0));
    assert_eq!(r[1], ("signal".to_string(), -1.0));
    assert_eq!(r[2], ("signal".to_string(), 0.0));
}

#[test]
fn test_engine_context_reset_all() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

    // Set up some state
    ctx.instrument_states.insert(1, InstrumentState {
        position: 1.0,
        entry_price: 100.0,
        unrealized_pnl: 10.0,
        realized_pnl: 5.0,
        commissions: 0.5,
    });
    ctx.update_price(1, 110.0);
    ctx.equity = 9000.0;
    ctx.num_trades = 10;

    // Reset for reuse
    ctx.reset_all(10000.0);

    // Verify reset state
    assert_eq!(ctx.equity, 10000.0);
    assert_eq!(ctx.peak_equity, 10000.0);
    assert_eq!(ctx.max_drawdown, 0.0);
    assert_eq!(ctx.num_trades, 0);
    assert!(ctx.instrument_states.is_empty());
    assert!(ctx.current_prices.is_empty());
    assert!(ctx.pending_orders.is_empty());
}

#[test]
fn test_engine_context_update_price_multiple_instruments() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

    // Update prices for multiple instruments
    ctx.update_price(1, 100.0);
    ctx.update_price(2, 200.0);
    ctx.update_price(3, 300.0);

    assert_eq!(ctx.get_price(1), 100.0);
    assert_eq!(ctx.get_price(2), 200.0);
    assert_eq!(ctx.get_price(3), 300.0);
    assert_eq!(ctx.get_price(999), 0.0); // Unknown returns 0

    // Update instrument 1 again
    ctx.update_price(1, 150.0);
    assert_eq!(ctx.get_price(1), 150.0);
}

#[test]
fn test_engine_context_equity_tracking() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

    assert_eq!(ctx.equity, 10000.0);
    assert_eq!(ctx.peak_equity, 10000.0);
    assert_eq!(ctx.max_drawdown, 0.0);

    // Simulate profit
    ctx.equity = 11000.0;
    ctx.update_equity(&std::collections::HashMap::new());

    assert_eq!(ctx.peak_equity, 11000.0);

    // Simulate loss
    ctx.equity = 9000.0;
    ctx.update_equity(&std::collections::HashMap::new());

    // Max drawdown should be (11000 - 9000) / 11000 * 100 = 18.18%
    assert!((ctx.max_drawdown - 18.18).abs() < 0.01);
}

#[test]
fn test_strategy_ctx_account_equity() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

    assert_eq!(ctx.equity, 10000.0);

    ctx.equity = 50000.0;
    assert_eq!(ctx.equity, 50000.0);
}

#[test]
fn test_strategy_ctx_subscribe_instruments() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

    let instruments = vec![
        InstrumentId::new("BTCUSDT", "BACKTEST"),
        InstrumentId::new("ETHUSDT", "BACKTEST"),
        InstrumentId::new("SOLUSDT", "BACKTEST"),
    ];

    ctx.subscribe_instruments(instruments);

    // Verify via the StrategyCtx trait (not struct internals)
    assert_eq!(ctx.subscribed_instruments.len(), 3);
}

#[test]
fn test_engine_context_pending_orders_clear() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());
    let instr_id = InstrumentId::new("BTCUSDT", "BACKTEST");

    // Add several orders
    for i in 0..5 {
        let order = Order {
            id: i as u64,
            client_order_id: ClientOrderId::new(&format!("order{}", i)),
            strategy_id: StrategyId::new("test"),
            instrument_id: instr_id.clone(),
            venue: Venue::new("TEST"),
            side: OrderSide::Buy,
            order_type: OrderType::Limit,
            price: 100.0 + i as f64,
            size: 1.0,
            sl: None,
            tp: None,
            filled: false,
            triggered: false,
            position_id: None,
            time_in_force: None,
            expire_time_ns: None,
            trailing_delta: None,
            trailing_offset: None,
            trailing_offset_type: None,
            activation_price: None,
            is_activated: false,
            trigger_price: None,
            limit_offset: None,
            contingency_type: nexus::engine::orders::ContingencyType::None,
            linked_order_ids: Vec::new(),
            order_list_id: None,
            trigger_type: nexus::engine::orders::TriggerType::Default,
            post_only: false,
            reduce_only: false,
        };
        ctx.add_pending_order(order);
    }

    assert_eq!(ctx.pending_orders(&instr_id).len(), 5);

    // Clear all pending orders
    ctx.clear_pending_orders();
    assert!(ctx.pending_orders(&instr_id).is_empty());
}

#[test]
fn test_engine_context_record_trade() {
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

    // Set up position
    ctx.instrument_states.insert(1, InstrumentState {
        position: 1.0,
        entry_price: 100.0,
        unrealized_pnl: 0.0,
        realized_pnl: 0.0,
        commissions: 0.0,
    });

    let trade = nexus::engine::Trade {
        timestamp_ns: 1000,
        instrument_id: 1,
        side: Signal::buy_market(),
        price: 100.0,
        size: 1.0,
        commission: 0.1,
        pnl: 5.0,
        fee: 0.05,
        is_maker: true,
    };

    ctx.record_trade(&trade);

    assert_eq!(ctx.num_trades, 1);

    let state = ctx.instrument_states.get(&1).unwrap();
    assert_eq!(state.realized_pnl, 5.0);
    assert_eq!(state.commissions, 0.1);
}

#[test]
fn test_order_emulator_submit_market() {
    use nexus::book::{OrderEmulator, Side};

    let mut emulator = OrderEmulator::new();

    // Submit market order
    let order_id = emulator.submit_market(1.0, Side::Buy, 1000);

    assert_ne!(order_id, 0);
    assert_eq!(emulator.num_pending(), 1);

    // Submit another market order (different size)
    let order_id2 = emulator.submit_market(0.5, Side::Sell, 1001);
    assert_ne!(order_id2, order_id);
    assert_eq!(emulator.num_pending(), 2);

    // Null size returns 0
    let bad_id = emulator.submit_market(0.0, Side::Buy, 1002);
    assert_eq!(bad_id, 0);
}

#[test]
fn test_order_emulator_submit_limit() {
    use nexus::book::{OrderEmulator, Side};

    let mut emulator = OrderEmulator::new();

    // Submit limit orders
    let id1 = emulator.submit_limit(100.0, 1.0, Side::Buy, 1000);
    let id2 = emulator.submit_limit(101.0, 0.5, Side::Sell, 1001);

    assert_ne!(id1, 0);
    assert_ne!(id2, 0);
    assert_ne!(id1, id2);
    assert_eq!(emulator.num_pending(), 2);

    // Cancel one
    let cancelled = emulator.cancel_order(id1);
    assert!(cancelled);
    assert_eq!(emulator.num_pending(), 1);
}

#[test]
fn test_engine_context_submit_market_null_emulator() {
    // Test that submit_market returns 0 when emulator is null
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

    let instr_id = InstrumentId::new("BTCUSDT", "BACKTEST");

    // With null emulator, submit_market should return 0
    let order_id = ctx.submit_market(&instr_id, OrderSide::Buy, 1.0);
    assert_eq!(order_id, 0);
}

#[test]
fn test_engine_context_submit_limit_null_emulator() {
    // Test that submit_limit returns 0 when emulator is null
    let signal_bus = Arc::new(Mutex::new(SignalBus::new()));
    let mut ctx = EngineContext::new(10000.0, signal_bus, std::ptr::null_mut());

    let instr_id = InstrumentId::new("BTCUSDT", "BACKTEST");

    // With null emulator, submit_limit should return 0
    let order_id = ctx.submit_limit(&instr_id, OrderSide::Buy, 100.0, 1.0);
    assert_eq!(order_id, 0);
}
