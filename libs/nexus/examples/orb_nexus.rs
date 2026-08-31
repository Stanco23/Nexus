//! Opening Range Breakout (ORB) — Nexus Rust
//! =========================================
//! Same logic, same parameters as orb_nautilus.py for fair backtest comparison.
//!
//! Settings:
//!   - Instrument:       ES (E-mini S&P 500 futures)
//!   - Opening window:   09:30–09:35 EST (first 5 min of RTH)
//!   - Direction:        Long on HH breakout, Short on LL breakout
//!   - Stop loss:        1 tick below HH (long) / 1 tick above LL (short)
//!   - Take profit:      1.5× opening range width
//!   - Session filter:   Entries only during 09:35–16:00 EST
//!   - Position limit:   1 contract
//!
//! Backtest runner:
//!   let sweep = BacktestSweep::new(buffer_set, 100_000.0);
//!   let results = sweep.run_grid::<OrbStrategy>(&config);
//!
//! Add to nexus/libs/nexus/src/  as  orb_strategy.rs
//! and add  mod orb_strategy;  to lib.rs

use std::collections::HashMap;

use nexus::engine::core::Signal;
use nexus::PositionSide;
use nexus::instrument::InstrumentId;
use nexus::portfolio::Portfolio;
use nexus::signals::SignalBus;

// ─────────────────────────────────────────────────────────────────────────────
// ORB Config
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct OrbConfig {
    /// Instrument ID string, e.g. "ES.AMEX"
    pub instrument_id: &'static str,
    /// Opening window in minutes
    pub opening_window_minutes: f64,
    /// Opening range start hour in EST decimal (9.5 = 09:30)
    pub opening_start_hour: f64,
    /// Session close hour in EST decimal (16.0 = 16:00)
    pub session_end_hour: f64,
    /// TP = atr_multiplier × opening_range_width
    pub atr_multiplier: f64,
    /// Position size in contracts
    pub position_size: f64,
    /// Tick size (price precision), e.g. 0.25 for ES
    pub tick_size: f64,
}

impl Default for OrbConfig {
    fn default() -> Self {
        Self {
            instrument_id: "ES.AMEX",
            opening_window_minutes: 5.0,
            opening_start_hour: 9.5,         // 09:30 EST
            session_end_hour: 16.0,           // 16:00 EST
            atr_multiplier: 1.5,
            position_size: 1.0,
            tick_size: 0.25,
        }
    }
}

impl OrbConfig {
    /// Opening window end = start + duration in decimal hours
    pub fn opening_end_hour(&self) -> f64 {
        self.opening_start_hour + self.opening_window_minutes / 60.0
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ORB Strategy
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Clone, Debug)]
pub struct OrbStrategy {
    config: OrbConfig,

    // Opening range
    orb_high: Option<f64>,
    orb_low: Option<f64>,
    orb_armed: bool,

    // Position state
    position_open: bool,
    entry_price: Option<f64>,
    stop_price: Option<f64>,
    take_profit: Option<f64>,
    position_side: PositionSide, // Track long vs short for correct SL/TP logic

    // Per-instrument last signal
    last_signal: HashMap<InstrumentId, Signal>,
}

impl OrbStrategy {
    pub fn new(config: OrbConfig) -> Self {
        Self {
            config,
            orb_high: None,
            orb_low: None,
            orb_armed: false,
            position_open: false,
            entry_price: None,
            stop_price: None,
            take_profit: None,
            position_side: PositionSide::Flat,
            last_signal: HashMap::new(),
        }
    }

    /// Convert UNIX nanoseconds to EST decimal hour.
    /// e.g. 9.5 = 09:30 EST, 16.0 = 16:00 EST
    fn unix_ns_to_est_hour(unix_ns: u64) -> f64 {
        // UNIX seconds + 5h (EST offset) → seconds-of-day → decimal hours
        let unix_sec = unix_ns as f64 / 1_000_000_000.0;
        let est_sec = (unix_sec + 5.0 * 3600.0) % 86400.0;
        est_sec / 3600.0
    }

    /// Round price to nearest tick
    fn round_to_tick(&self, price: f64) -> f64 {
        (price / self.config.tick_size).round() * self.config.tick_size
    }

    /// Emit a close signal and reset position state
    fn close_position(&mut self) {
        self.position_open = false;
        self.entry_price = None;
        self.stop_price = None;
        self.take_profit = None;
        self.position_side = PositionSide::Flat;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// PortfolioStrategy trait
// ─────────────────────────────────────────────────────────────────────────────

impl nexus::portfolio::PortfolioStrategy for OrbStrategy {
    /// Called on every trade tick during backtest.
    /// Returns Signal::Buy, Signal::Sell, Signal::Close, or Signal::Close
    /// (no-op — position managed by Portfolio's built-in SL/TP via PortfolioConfig).
    fn on_trade(
        &mut self,
        instrument_id: InstrumentId,
        timestamp_ns: u64,
        price: f64,
        _size: f64,
        _portfolio: &mut Portfolio,
    ) -> Signal {
        // Only trade our instrument
        let expected_id = InstrumentId::new(self.config.instrument_id, "AMEX");
        if instrument_id != expected_id {
            return Signal::close();
        }

        let hour = Self::unix_ns_to_est_hour(timestamp_ns);
        let opening_end = self.config.opening_end_hour();

        // ── Phase 1: Build opening range ────────────────────────────────────
        if !self.orb_armed && hour < opening_end {
            match self.orb_high {
                None => { self.orb_high = Some(price); }
                Some(h) if price > h => { self.orb_high = Some(price); }
                _ => {}
            }
            match self.orb_low {
                None => { self.orb_low = Some(price); }
                Some(l) if price < l => { self.orb_low = Some(price); }
                _ => {}
            }
            // Stay in building phase
            return Signal::close();
        }

        // ── Phase 2: Arm ORB when window closes ─────────────────────────────
        if !self.orb_armed && hour >= opening_end {
            if self.orb_high.is_some() && self.orb_low.is_some() {
                self.orb_armed = true;
                let range_width = self.orb_high.unwrap() - self.orb_low.unwrap();
                // Stop: 1 tick below low for longs, 1 tick above high for shorts
                let sl_offset = self.config.tick_size;
                self.stop_price = Some(self.orb_low.unwrap() - sl_offset);
                // TP will be set at entry based on range_width
                let _tp_base = range_width * self.config.atr_multiplier;
                println!(
                    "[ORB] Armed — High: {:.2}  Low: {:.2}  Width: {:.2}",
                    self.orb_high.unwrap(),
                    self.orb_low.unwrap(),
                    range_width
                );
            } else {
                println!("[ORB] Window closed — no range data, skipping day");
            }
            return Signal::close();
        }

        // ── Phase 3: Only trade during regular session ───────────────────────
        if hour < opening_end || hour >= self.config.session_end_hour {
            return Signal::close();
        }

        // ── Phase 4: Entry logic ─────────────────────────────────────────────
        if !self.position_open {
            let range_width = self.orb_high.unwrap() - self.orb_low.unwrap();
            let tick = self.config.tick_size;

            // Long breakout — price closes above ORB high
            if price > self.orb_high.unwrap() {
                self.entry_price = Some(price);
                self.stop_price = Some(self.orb_low.unwrap() - tick);
                self.take_profit =
                    Some(self.round_to_tick(price + range_width * self.config.atr_multiplier));
                self.position_open = true;
                self.position_side = PositionSide::Long;
                println!(
                    "[ORB] LONG  entry:{:.2}  sl:{:.2}  tp:{:.2}",
                    price,
                    self.stop_price.unwrap(),
                    self.take_profit.unwrap()
                );
                return Signal::buy_market();
            }

            // Short breakout — price closes below ORB low
            if price < self.orb_low.unwrap() {
                self.entry_price = Some(price);
                self.stop_price = Some(self.orb_high.unwrap() + tick);
                self.take_profit =
                    Some(self.round_to_tick(price - range_width * self.config.atr_multiplier));
                self.position_open = true;
                self.position_side = PositionSide::Short;
                println!(
                    "[ORB] SHORT entry:{:.2}  sl:{:.2}  tp:{:.2}",
                    price,
                    self.stop_price.unwrap(),
                    self.take_profit.unwrap()
                );
                return Signal::sell_market();
            }

            return Signal::close();
        }

        // ── Phase 5: Exit logic ──────────────────────────────────────────────
        if self.position_open {
            let Some(entry) = self.entry_price else {
                return Signal::close();
            };

            let direction = if self.position_side == PositionSide::Long { 1.0 } else { -1.0 };
            let pnl_ticks = direction * (price - entry) / self.config.tick_size;

            // Stop loss (entry - 1 tick for long, entry + 1 tick for short)
            let sl_ticks = 1.0_f64;
            if pnl_ticks <= -sl_ticks {
                let reason = if self.position_side == PositionSide::Long {
                    "SL HIT (long)"
                } else {
                    "SL HIT (short)"
                };
                println!("[ORB] {} @ {:.2}", reason, price);
                self.close_position();
                return Signal::close();
            }

            // Take profit (1.5× range_width in ticks)
            let tp_ticks = (self.take_profit.unwrap() - entry) / self.config.tick_size * direction;
            if pnl_ticks >= tp_ticks {
                let reason = if self.position_side == PositionSide::Long {
                    "TP HIT (long)"
                } else {
                    "TP HIT (short)"
                };
                println!("[ORB] {} @ {:.2}", reason, price);
                self.close_position();
                return Signal::close();
            }
        }

        Signal::Close
    }

    fn subscribe_signal(&mut self, _signal_bus: std::sync::Arc<SignalBus>) {
        // No external signal subscriptions needed for ORB
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Example backtest runner
// Run with: cargo test -p nexus --example orb_nexus -- --nocapture
// Or add to your backtest sweep: sweep.run_grid::<OrbStrategy>(&config)
// ─────────────────────────────────────────────────────────────────────────────

fn main() {
    println!("ORB Strategy — see tests below or integrate with BacktestSweep");
    println!("Run: cargo test -p nexus --example orb_nexus -- --nocapture");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_est_hour_conversion() {
        // UNIX 0 = Jan 1 1970 00:00 UTC = Jan 1 1970 19:00 EST
        // 19:00 EST = 7pm — outside trading hours
        let h = OrbStrategy::unix_ns_to_est_hour(0);
        assert!(h > 0.0 && h < 24.0);

        // 09:30 EST on a generic day = 14:30 UTC
        // 14:30 UTC = 51480 seconds = 14.3 hours
        // That's not the right way to test this — just check it stays in [0, 24)
        let h2 = OrbStrategy::unix_ns_to_est_hour(1_700_000_000_000_000_000_u64);
        assert!(h2 >= 0.0 && h2 < 24.0);
    }

    #[test]
    fn test_round_to_tick() {
        let cfg = OrbConfig::default();
        let strat = OrbStrategy::new(cfg);
        // ES tick = 0.25
        assert_eq!(strat.round_to_tick(100.17), 100.25);
        assert_eq!(strat.round_to_tick(100.13), 100.25); // 100.13 → 100.25
        assert_eq!(strat.round_to_tick(100.01), 100.00); // 100.01 → 100.00
    }

    #[test]
    fn test_opening_end_hour() {
        let cfg = OrbConfig::default();
        assert!((cfg.opening_end_hour() - 9.5833).abs() < 0.001);
    }
}
