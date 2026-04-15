use nexus::engine::{CommissionConfig, EngineContext, Signal, Strategy};

struct SimpleBuyThenCloseStrategy {
    buy_at_tick: usize,
    close_at_tick: usize,
    counter: usize,
}

impl SimpleBuyThenCloseStrategy {
    fn new(buy_at_tick: usize, close_at_tick: usize) -> Self {
        Self {
            buy_at_tick,
            close_at_tick,
            counter: 0,
        }
    }
}

impl Strategy for SimpleBuyThenCloseStrategy {
    fn on_tick(
        &mut self,
        _timestamp_ns: u64,
        _price: f64,
        _size: f64,
        _ctx: &mut EngineContext,
    ) -> Signal {
        self.counter += 1;
        if self.counter == self.buy_at_tick {
            Signal::Buy
        } else if self.counter == self.close_at_tick {
            Signal::Close
        } else {
            Signal::Close
        }
    }
}

#[test]
fn test_commission_long_entry_exit() {
    let comm = CommissionConfig::new(0.001);
    let entry = 100.0;
    let size = 1.0;
    let exit = 110.0;

    let entry_comm = comm.compute(entry, size);
    let exit_comm = comm.compute(exit, size);
    let gross_pnl = (exit - entry) * size;
    let net_pnl = gross_pnl - entry_comm - exit_comm;

    assert!((net_pnl - 9.998).abs() < 0.001, "net_pnl={}", net_pnl);
}

#[test]
fn test_commission_short_entry_exit() {
    let comm = CommissionConfig::new(0.001);
    let entry = 100.0;
    let size = 1.0;
    let exit = 90.0;

    let entry_comm = comm.compute(entry, size);
    let exit_comm = comm.compute(exit, size);
    let gross_pnl = (entry - exit) * size;
    let net_pnl = gross_pnl - entry_comm - exit_comm;

    assert!((net_pnl - 9.998).abs() < 0.001, "net_pnl={}", net_pnl);
}

#[test]
fn test_engine_context_position_lifecycle() {
    let mut ctx = EngineContext::new(10000.0);
    let comm = CommissionConfig::new(0.001);

    assert_eq!(ctx.position, 0.0);
    assert_eq!(ctx.num_trades, 0);

    let entry_price = 100.0;
    let size = 1.0;
    let entry_comm = comm.compute(entry_price, size);

    ctx.equity -= entry_comm;
    ctx.position = size;
    ctx.entry_price = entry_price;
    ctx.num_trades += 1;

    assert_eq!(ctx.position, 1.0);
    assert_eq!(ctx.entry_price, 100.0);

    let exit_price = 110.0;
    let exit_comm = comm.compute(exit_price, size);
    ctx.equity -= exit_comm;

    let pnl = (exit_price - ctx.entry_price) * ctx.position.abs();
    ctx.equity += pnl;
    ctx.position = 0.0;
    ctx.entry_price = 0.0;

    let expected_equity = 10000.0 - entry_comm + pnl - exit_comm;
    assert!((ctx.equity - expected_equity).abs() < 0.001);
    assert_eq!(ctx.position, 0.0);
}

#[test]
fn test_engine_context_partial_close() {
    let mut ctx = EngineContext::new(10000.0);
    let comm = CommissionConfig::new(0.001);

    ctx.position = 1.0;
    ctx.entry_price = 100.0;
    ctx.equity = 10000.0;

    let partial_size = 0.5;
    let exit_price = 110.0;
    let exit_comm = comm.compute(exit_price, partial_size);

    ctx.equity -= exit_comm;
    let pnl = (exit_price - ctx.entry_price) * partial_size;
    ctx.equity += pnl;
    ctx.position -= partial_size;

    assert!((ctx.position - 0.5).abs() < 0.001);
    assert!((ctx.entry_price - 100.0).abs() < 0.001);

    let final_exit = 120.0;
    let final_comm = comm.compute(final_exit, ctx.position.abs());
    ctx.equity -= final_comm;
    let final_pnl = (final_exit - ctx.entry_price) * ctx.position.abs();
    ctx.equity += final_pnl;
    ctx.position = 0.0;
    ctx.entry_price = 0.0;

    let total_comm = exit_comm + final_comm;
    let total_pnl = pnl + final_pnl;
    let expected_change = total_pnl - total_comm;
    assert!((ctx.equity - (10000.0 + expected_change)).abs() < 0.001);
}
