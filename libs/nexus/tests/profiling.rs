//! Micro-benchmark — isolate where time goes in the sweep hot path.
//!
//! Run with: cargo test -p nexus --test profiling --release -- --nocapture

use nexus::buffer::RingBufferSet;
use nexus::engine::CommissionConfig;
use nexus::instrument::InstrumentId;
use nexus::portfolio::{Portfolio, PortfolioConfig, PortfolioStrategy};
use nexus_types::Signal as NexusSignal;
use std::collections::HashMap;
use std::time::Instant;

// ── Strategy ─────────────────────────────────────────────────────────────────

struct MomentumStrategy {
    threshold: f64,
    last_price: f64,
    position_open: bool,
}

impl MomentumStrategy {
    fn new(threshold: f64) -> Self {
        Self { threshold, last_price: 0.0, position_open: false }
    }
}

impl Clone for MomentumStrategy {
    fn clone(&self) -> Self { Self::new(self.threshold) }
}

impl PortfolioStrategy for MomentumStrategy {
    fn on_trade(
        &mut self,
        _: InstrumentId,
        _: u64,
        price: f64,
        _: f64,
        _: &mut Portfolio,
    ) -> NexusSignal {
        if self.last_price == 0.0 {
            self.last_price = price;
            return NexusSignal::Close;
        }
        let delta = (price - self.last_price) / self.last_price * 100.0;
        self.last_price = price;
        if delta > self.threshold && !self.position_open {
            self.position_open = true;
            NexusSignal::Buy
        } else if delta < -self.threshold && self.position_open {
            self.position_open = false;
            NexusSignal::Sell
        } else {
            NexusSignal::Close
        }
    }
}

// ── Data generation ──────────────────────────────────────────────────────────

fn generate_synthetic_ticks(n_ticks: usize) -> (std::path::PathBuf, InstrumentId) {
    use tvc::TvcWriter;
    use tvc::TradeTick;

    let path = std::path::PathBuf::from(format!("/tmp/prof_ticks_{}.tvc", n_ticks));
    let instrument_id = InstrumentId::new("BTCUSDT", "BINANCE");

    let mut writer = TvcWriter::new(&path, 1u32, 10, 9).unwrap();
    let base_price = 50_000i64 * 1_000_000_000;
    let start_ts = 1_700_000_000_000_000_000u64;

    let mut price = base_price;
    for i in 0..n_ticks {
        let noise = ((i as i64 % 100) - 50) * 100_000_000;
        price += noise;
        let tick = TradeTick::new(
            start_ts + (i as u64) * 1_000_000_000,
            price,
            1_000_000_000,
            (i % 2) as u8,
            1,
            i as u32,
        );
        writer.write_tick(&tick).unwrap();
    }
    writer.finalize().unwrap();
    (path, instrument_id)
}

// ── Phases ────────────────────────────────────────────────────────────────────

/// Phase 1: RingBufferSet creation + mmap load.
fn phase1_load(n_ticks: usize) -> (RingBufferSet, std::time::Duration) {
    let (path, instrument_id) = generate_synthetic_ticks(n_ticks);
    let start = Instant::now();
    let buffer_set = RingBufferSet::single(&path, instrument_id.clone()).unwrap();
    let elapsed = start.elapsed();
    let _ = std::fs::remove_file(&path);
    (buffer_set, elapsed)
}

/// Phase 2: iter_state_from_global_tick only (seek + index read, no decode).
fn phase2_iter_only(buffer_set: &RingBufferSet, total_ticks: u64) -> std::time::Duration {
    let start = Instant::now();
    let mut count = 0u64;
    for global_tick in 0..total_ticks {
        if buffer_set.iter_state_from_global_tick(global_tick).is_some() {
            count += 1;
        }
    }
    let elapsed = start.elapsed();
    println!(
        "  [phase2] iter_state (seek only): {} ticks in {:.3}ms  ({:.0}/sec)",
        count,
        elapsed.as_secs_f64() * 1000.0,
        count as f64 / elapsed.as_secs_f64()
    );
    elapsed
}

/// Phase 3: iter_state + decode_anchor_at.
fn phase3_iter_and_decode(buffer_set: &RingBufferSet, total_ticks: u64) -> std::time::Duration {
    let start = Instant::now();
    let mut count = 0u64;
    for global_tick in 0..total_ticks {
        if let Some((buf, offset, _, _)) = buffer_set.iter_state_from_global_tick(global_tick) {
            if buf.decode_anchor_at(offset).is_ok() {
                count += 1;
            }
        }
    }
    let elapsed = start.elapsed();
    println!(
        "  [phase3] iter_state + decode_anchor: {} ticks in {:.3}ms  ({:.0}/sec)",
        count,
        elapsed.as_secs_f64() * 1000.0,
        count as f64 / elapsed.as_secs_f64()
    );
    elapsed
}

/// Phase 4: Full sweep tick loop (mirrors sweep/mod.rs:run_sweep_tick_loop).
fn phase4_full_loop(buffer_set: &RingBufferSet, n_ticks: usize) -> std::time::Duration {
    use nexus::engine::Signal as EngineSignal;
    use nexus::StrategyCtx;
    use nexus::engine::core::EngineContext;

    let config = PortfolioConfig::new(10_000.0, CommissionConfig::new(0.001));
    let mut portfolio = Portfolio::new(config.initial_equity_per_instrument);
    let instrument_ids = buffer_set.instrument_ids();
    let instrument_id = instrument_ids.first().cloned().unwrap();
    portfolio.register_instrument(instrument_id.clone());

    let total_ticks = buffer_set.total_ticks() as u64;
    let mut last_prices: HashMap<u32, f64> = HashMap::new();
    let mut strategy = MomentumStrategy::new(0.005);

    let start = Instant::now();
    let mut global_tick: u64 = 0;
    while global_tick < total_ticks {
        let Some((buffer, offset, _tick_idx, _anchor_slot)) =
            buffer_set.iter_state_from_global_tick(global_tick)
        else {
            global_tick += 1;
            continue;
        };
        let Ok(tick) = buffer.decode_anchor_at(offset) else {
            global_tick += 1;
            continue;
        };

        let ts = tick.timestamp_ns;
        let price = tick.price_int as f64 / 1e9;
        let size = tick.size_int as f64 / 1e9;

        let anchor = &buffer_set.merged_anchors()[global_tick as usize];
        let inst_id = instrument_ids
            .get(anchor.buffer_idx)
            .cloned()
            .unwrap_or_else(|| instrument_ids.first().cloned().unwrap());

        last_prices.insert(inst_id.id, price);
        if let Some(state) = portfolio.state_mut(&inst_id) {
            state.update_unrealized_pnl(price);
        }
        portfolio.record_equity();

        let signal_bus =
            std::sync::Arc::new(std::sync::Mutex::new(nexus::signals::SignalBus::new()));
        let mut ctx = EngineContext::new(
            config.initial_equity_per_instrument,
            signal_bus,
            std::ptr::null_mut(),
        );
        ctx.subscribe_instruments(vec![inst_id.clone()]);

        let ntick = nexus_types::Tick {
            timestamp_ns: ts,
            price,
            size,
            vpin: 0.0,
        };
        let _ = ntick;

        // Strategy call
        let sig = strategy.on_trade(inst_id.clone(), ts, price, size, &mut portfolio);
        let es = match sig {
            NexusSignal::Buy => EngineSignal::Buy,
            NexusSignal::Sell => EngineSignal::Sell,
            NexusSignal::Close => EngineSignal::Close,
        };

        let position = portfolio.state(&inst_id).map(|s| s.position).unwrap_or(0.0);
        let has_pos = position != 0.0;
        let is_long = position > 0.0;

        match (es, has_pos, is_long) {
            (EngineSignal::Buy, false, _) | (EngineSignal::Buy, true, false) => {
                if let Some(state) = portfolio.state_mut(&inst_id) {
                    let sz = if has_pos { position.abs() + 1.0 } else { 1.0 };
                    let comm = config.commission.compute(price, sz.abs());
                    if state.position == 0.0 {
                        state.position = sz;
                        state.entry_price = price;
                    } else {
                        let pnl = if state.position > 0.0 {
                            (price - state.entry_price) * state.position.abs()
                        } else {
                            (state.entry_price - price) * state.position.abs()
                        };
                        state.realized_pnl += pnl;
                        state.position = sz;
                        state.entry_price = price;
                    }
                    state.equity -= comm;
                    state.commissions += comm;
                    state.num_trades += 1;
                }
            }
            (EngineSignal::Sell, false, _) | (EngineSignal::Sell, true, true) => {
                if let Some(state) = portfolio.state_mut(&inst_id) {
                    let sz = if has_pos { position.abs() + 1.0 } else { 1.0 };
                    let comm = config.commission.compute(price, sz.abs());
                    if state.position == 0.0 {
                        state.position = -sz;
                        state.entry_price = price;
                    } else {
                        let pnl = if state.position > 0.0 {
                            (price - state.entry_price) * state.position.abs()
                        } else {
                            (state.entry_price - price) * state.position.abs()
                        };
                        state.realized_pnl += pnl;
                        state.position = -sz;
                        state.entry_price = price;
                    }
                    state.equity -= comm;
                    state.commissions += comm;
                    state.num_trades += 1;
                }
            }
            (EngineSignal::Close, true, _) => {
                if let Some(state) = portfolio.state_mut(&inst_id) {
                    let pnl = if state.position > 0.0 {
                        (price - state.entry_price) * state.position.abs()
                    } else {
                        (state.entry_price - price) * state.position.abs()
                    };
                    let comm = config.commission.compute(price, state.position.abs());
                    state.realized_pnl += pnl;
                    state.equity += pnl - comm;
                    state.commissions += comm;
                    state.position = 0.0;
                    state.entry_price = 0.0;
                    state.num_trades += 1;
                }
            }
            _ => {}
        }

        for (_id, p) in last_prices.iter() {
            if let Some(state) = portfolio.state_mut(&inst_id) {
                state.update_peak(*p);
            }
        }

        global_tick += 1;
    }

    let elapsed = start.elapsed();
    println!(
        "  [phase4] full sweep loop: {} ticks in {:.3}ms  ({:.0}/sec)",
        n_ticks,
        elapsed.as_secs_f64() * 1000.0,
        n_ticks as f64 / elapsed.as_secs_f64()
    );
    elapsed
}

// ── Test ──────────────────────────────────────────────────────────────────────

fn run_profile(n_ticks: usize) {
    println!("\n=== {} ticks ===", n_ticks);

    // Phase 1: Load
    let (buffer_set, load_dur) = phase1_load(n_ticks);
    println!(
        "  [phase1] load + mmap: {:.3}ms",
        load_dur.as_secs_f64() * 1000.0
    );

    let total_ticks = buffer_set.total_ticks() as u64;

    // Phase 2: Raw seek only
    let t2 = Instant::now();
    phase2_iter_only(&buffer_set, total_ticks);
    let t3 = Instant::now();

    // Phase 3: Add decode
    phase3_iter_and_decode(&buffer_set, total_ticks);
    let t4 = Instant::now();

    // Phase 4: Full loop with strategy + portfolio
    phase4_full_loop(&buffer_set, n_ticks);
    let t5 = Instant::now();

    println!(
        "  [delta] phase2 (seek only):    {:.3}ms",
        (t3 - t2).as_secs_f64() * 1000.0
    );
    println!(
        "  [delta] phase3 (iter+decode):  {:.3}ms",
        (t4 - t3).as_secs_f64() * 1000.0
    );
    println!(
        "  [delta] phase4 (full loop):    {:.3}ms",
        (t5 - t4).as_secs_f64() * 1000.0
    );
}

#[test]
fn test_profile_tick_throughput() {
    run_profile(1_000);
    run_profile(10_000);
    run_profile(100_000);
}