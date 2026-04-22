//! Technical indicators for strategy use.
//!
//! All indicators implement the `Indicator` trait with O(1) update time.
//! Indicator state is owned — each strategy clone gets fresh indicator instances.
//!
//! # Warmup behavior
//! All indicators return `None` during their warmup period (until enough data is
//! accumulated for a meaningful output). After warmup, they return `Some(value)`
//! on every subsequent update. This allows strategies to safely call `.unwrap()`
//! once warmup is confirmed.

use std::collections::VecDeque;

/// Indicator trait — all technical indicators implement this.
pub trait Indicator: Send + Sync {
    /// Update with a new value. Returns indicator output once initialized.
    fn update(&mut self, value: f64) -> Option<f64>;

    /// Reset indicator state to initial.
    fn reset(&mut self);
}

// ─── Trend Indicators ────────────────────────────────────────────────────────

/// Simple Moving Average — rolling mean over a fixed window.
pub struct Sma {
    window: usize,
    buf: VecDeque<f64>,
    sum: f64,
}

impl Sma {
    pub fn new(window: usize) -> Self {
        Self {
            window,
            buf: VecDeque::with_capacity(window),
            sum: 0.0,
        }
    }

    /// Returns the current SMA value (available once window is full).
    pub fn mean(&self) -> Option<f64> {
        if self.buf.len() == self.window {
            Some(self.sum / self.window as f64)
        } else {
            None
        }
    }
}

impl Indicator for Sma {
    /// Returns `None` until the window is full. Returns `Some(mean)` after.
    fn update(&mut self, value: f64) -> Option<f64> {
        if self.buf.len() == self.window {
            let oldest = self.buf.pop_front().unwrap();
            self.sum -= oldest;
        }
        self.buf.push_back(value);
        self.sum += value;
        self.mean()
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.sum = 0.0;
    }
}

/// Exponential Moving Average — alpha = 2/(period+1).
pub struct Ema {
    alpha: f64,
    value: f64,
    initialized: bool,
}

impl Ema {
    pub fn new(period: usize) -> Self {
        let alpha = 2.0 / (period as f64 + 1.0);
        Self {
            alpha,
            value: 0.0,
            initialized: false,
        }
    }
}

impl Indicator for Ema {
    /// Immediately returns `Some(ema)` on first update (initialization value).
    fn update(&mut self, value: f64) -> Option<f64> {
        if !self.initialized {
            self.value = value;
            self.initialized = true;
        } else {
            self.value = value * self.alpha + self.value * (1.0 - self.alpha);
        }
        Some(self.value)
    }

    fn reset(&mut self) {
        self.value = 0.0;
        self.initialized = false;
    }
}

// ─── Momentum Indicators ─────────────────────────────────────────────────────

/// Relative Strength Index.
#[derive(Clone)]
pub struct Rsi {
    period: usize,
    gains: VecDeque<f64>,
    losses: VecDeque<f64>,
    last_price: f64,
    avg_gain: f64,
    avg_loss: f64,
    count: usize,
}

impl Rsi {
    pub fn new(period: usize) -> Self {
        Self {
            period,
            gains: VecDeque::with_capacity(period),
            losses: VecDeque::with_capacity(period),
            last_price: 0.0,
            avg_gain: 0.0,
            avg_loss: 0.0,
            count: 0,
        }
    }

    /// Returns RSI value (0-100).
    pub fn value(&self) -> f64 {
        if self.avg_loss == 0.0 {
            if self.avg_gain == 0.0 {
                return 50.0;
            }
            return 100.0;
        }
        let rs = self.avg_gain / self.avg_loss;
        100.0 - (100.0 / (1.0 + rs))
    }
}

impl Indicator for Rsi {
    /// Returns `None` until the first period of gains/losses is accumulated.
    /// Then returns `Some(rsi)` on every subsequent update.
    fn update(&mut self, price: f64) -> Option<f64> {
        if self.last_price == 0.0 {
            self.last_price = price;
            return None;
        }
        let change = price - self.last_price;
        let gain = if change > 0.0 { change } else { 0.0 };
        let loss = if change < 0.0 { -change } else { 0.0 };
        self.last_price = price;

        if self.count < self.period {
            // First period: accumulate and compute simple average at end
            self.gains.push_back(gain);
            self.losses.push_back(loss);
            self.count += 1;
            if self.count == self.period {
                let sum_gain: f64 = self.gains.iter().sum();
                let sum_loss: f64 = self.losses.iter().sum();
                self.avg_gain = sum_gain / self.period as f64;
                self.avg_loss = sum_loss / self.period as f64;
                return Some(self.value());
            }
            return None;
        }

        // True Wilder smoothing — O(1) recursive average
        self.avg_gain = (self.avg_gain * (self.period as f64 - 1.0) + gain) / self.period as f64;
        self.avg_loss = (self.avg_loss * (self.period as f64 - 1.0) + loss) / self.period as f64;

        Some(self.value())
    }

    fn reset(&mut self) {
        self.gains.clear();
        self.losses.clear();
        self.last_price = 0.0;
        self.avg_gain = 0.0;
        self.avg_loss = 0.0;
        self.count = 0;
    }
}

/// Stochastic Oscillator (%K/%D).
///
/// Stores high/low/close windows. The `update(close)` variant uses a
/// high/low approximation from the close-only context. For accurate
/// stochastic, use `stochastic_update(stoch, high, low, close)` which
/// maintains separate high/low buffers.
pub struct Stochastic {
    k_period: usize,
    d_period: usize,
    highs: VecDeque<f64>,
    lows: VecDeque<f64>,
    closes: VecDeque<f64>,
    k_values: VecDeque<f64>,
    count: usize,
}

impl Stochastic {
    pub fn new(k_period: usize, d_period: usize) -> Self {
        Self {
            k_period,
            d_period,
            highs: VecDeque::with_capacity(k_period),
            lows: VecDeque::with_capacity(k_period),
            closes: VecDeque::with_capacity(k_period),
            k_values: VecDeque::with_capacity(d_period),
            count: 0,
        }
    }

    /// Returns `(k, d)` values.
    pub fn value(&self) -> (f64, f64) {
        match (self.k_values.back(), self.k_values.len()) {
            (Some(&k), len) if len >= self.d_period => {
                let d: f64 = self.k_values.iter().sum::<f64>() / self.d_period as f64;
                (k, d)
            }
            _ => (0.0, 0.0),
        }
    }
}

impl Indicator for Stochastic {
    /// Returns `None` until `k_period` closes are accumulated.
    /// Then returns `Some(d)` on every subsequent update.
    fn update(&mut self, close: f64) -> Option<f64> {
        self.closes.push_back(close);
        if self.closes.len() > self.k_period {
            self.closes.pop_front();
        }
        self.count += 1;

        if self.count < self.k_period {
            return None;
        }

        // Compute %K from high/low/close windows for accurate stochastic.
        // Uses separate buffers maintained by `stochastic_update` when available.
        let highest_high = self.highs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let lowest_low = self.lows.iter().copied().fold(f64::INFINITY, f64::min);

        // Fallback approximation: if highs/lows buffers are empty (single-input mode),
        // approximate from close window.
        let (high, low) = if self.highs.is_empty() {
            let high_est = *self.closes.iter().fold(&f64::NEG_INFINITY, |a, &b| if *a > b { a } else { &b });
            let low_est = *self.closes.iter().fold(&f64::INFINITY, |a, &b| if *a < b { a } else { &b });
            (high_est, low_est)
        } else {
            (highest_high, lowest_low)
        };

        let latest_close = *self.closes.back().unwrap();
        let range = high - low;
        let k = if range == 0.0 { 50.0 } else { (latest_close - low) / range * 100.0 };
        self.k_values.push_back(k);
        if self.k_values.len() > self.d_period {
            self.k_values.pop_front();
        }
        let d: f64 = self.k_values.iter().sum::<f64>() / self.d_period as f64;
        Some(d)
    }

    fn reset(&mut self) {
        self.highs.clear();
        self.lows.clear();
        self.closes.clear();
        self.k_values.clear();
        self.count = 0;
    }
}

/// Full stochastic update with high/low/close — maintains separate HLC buffers.
///
/// For use when OHLC data is available (bar-mode strategies).
/// Returns `None` until `k_period` bars are accumulated.
pub fn stochastic_update(stoch: &mut Stochastic, high: f64, low: f64, close: f64) -> Option<(f64, f64)> {
    stoch.highs.push_back(high);
    stoch.lows.push_back(low);
    stoch.closes.push_back(close);
    if stoch.highs.len() > stoch.k_period {
        stoch.highs.pop_front();
        stoch.lows.pop_front();
        stoch.closes.pop_front();
    }
    stoch.count += 1;

    if stoch.count < stoch.k_period {
        return None;
    }

    let highest_high = stoch.highs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let lowest_low = stoch.lows.iter().copied().fold(f64::INFINITY, f64::min);
    let latest_close = *stoch.closes.back().unwrap();
    let range = highest_high - lowest_low;
    let k = if range == 0.0 { 50.0 } else { (latest_close - lowest_low) / range * 100.0 };
    stoch.k_values.push_back(k);
    if stoch.k_values.len() > stoch.d_period {
        stoch.k_values.pop_front();
    }
    let d: f64 = stoch.k_values.iter().sum::<f64>() / stoch.d_period as f64;
    Some((k, d))
}

// ─── Volatility Indicators ───────────────────────────────────────────────────

/// True Range — internal helper for ATR.
fn true_range(high: f64, low: f64, prev_close: f64) -> f64 {
    high - low
        .max((high - prev_close).abs())
        .max((low - prev_close).abs())
}

/// Average True Range — Wilder's smoothing.
///
/// The `update(close)` variant uses a simplified TR approximation (|close - prev_close|).
/// For full True Range (high/low/prev_close), use `atr_update(atr, high, low, close)`.
pub struct Atr {
    period: usize,
    trs: VecDeque<f64>,
    atr: f64,
    count: usize,
    prev_close: f64,
    initialized: bool,
}

impl Atr {
    pub fn new(period: usize) -> Self {
        Self {
            period,
            trs: VecDeque::with_capacity(period),
            atr: 0.0,
            count: 0,
            prev_close: 0.0,
            initialized: false,
        }
    }
}

impl Indicator for Atr {
    /// Returns `None` during warmup (until `period` TRs accumulated).
    /// Then returns `Some(atr)` on every subsequent update using Wilder smoothing.
    fn update(&mut self, close: f64) -> Option<f64> {
        if !self.initialized {
            self.prev_close = close;
            self.initialized = true;
            return None;
        }
        // Simplified True Range: absolute price change.
        // For full TR, use `atr_update()` which computes true_range(high, low, prev_close).
        let tr = (close - self.prev_close).abs();
        self.prev_close = close;
        self.trs.push_back(tr);
        if self.trs.len() > self.period {
            self.trs.pop_front();
        }
        self.count += 1;
        if self.count < self.period {
            return None;
        }
        if self.count == self.period {
            // First ATR: simple average of first `period` TRs
            self.atr = self.trs.iter().sum::<f64>() / self.period as f64;
        } else {
            // Wilder smoothing: recursive ATR
            self.atr = (self.atr * (self.period as f64 - 1.0) + tr) / self.period as f64;
        }
        Some(self.atr)
    }

    fn reset(&mut self) {
        self.trs.clear();
        self.atr = 0.0;
        self.count = 0;
        self.prev_close = 0.0;
        self.initialized = false;
    }
}

/// Full ATR update with high/low/close — computes proper True Range.
///
/// Returns `None` during warmup, then `Some(atr)` after `period` bars.
pub fn atr_update(atr: &mut Atr, high: f64, low: f64, close: f64) -> Option<f64> {
    if !atr.initialized {
        atr.prev_close = close;
        atr.initialized = true;
        return None;
    }
    let tr = true_range(high, low, atr.prev_close);
    atr.prev_close = close;
    atr.trs.push_back(tr);
    if atr.trs.len() > atr.period {
        atr.trs.pop_front();
    }
    atr.count += 1;
    if atr.count < atr.period {
        return None;
    }
    if atr.count == atr.period {
        atr.atr = atr.trs.iter().sum::<f64>() / atr.period as f64;
    } else {
        atr.atr = (atr.atr * (atr.period as f64 - 1.0) + tr) / atr.period as f64;
    }
    Some(atr.atr)
}

// ─── Volume Indicators ───────────────────────────────────────────────────────

/// Volume Weighted Average Price — cumulative price × volume divided by cumulative volume.
///
/// VWAP resets at the start of each session. For session-aware VWAP, track
/// session boundaries and call `reset()` at session start.
pub struct Vwap {
    cumulative_pv: f64,
    cumulative_vol: f64,
}

impl Vwap {
    pub fn new() -> Self {
        Self {
            cumulative_pv: 0.0,
            cumulative_vol: 0.0,
        }
    }

    /// Returns the current VWAP value. Returns 0.0 if no volume has been accumulated.
    pub fn value(&self) -> f64 {
        if self.cumulative_vol == 0.0 {
            0.0
        } else {
            self.cumulative_pv / self.cumulative_vol
        }
    }
}

impl Indicator for Vwap {
    /// Always returns `Some(vwap)` — VWAP is immediately available after first update.
    fn update(&mut self, price: f64, volume: f64) -> Option<f64> {
        self.cumulative_pv += price * volume;
        self.cumulative_vol += volume;
        Some(self.value())
    }

    fn reset(&mut self) {
        self.cumulative_pv = 0.0;
        self.cumulative_vol = 0.0;
    }
}

impl Default for Vwap {
    fn default() -> Self {
        Self::new()
    }
}

/// MACD — Moving Average Convergence Divergence.
pub struct Macd {
    fast_ema: Ema,
    slow_ema: Ema,
    signal_ema: Ema,
    macd_values: VecDeque<f64>,
    initialized: bool,
}

impl Macd {
    pub fn new(fast: usize, slow: usize, signal: usize) -> Self {
        Self {
            fast_ema: Ema::new(fast),
            slow_ema: Ema::new(slow),
            signal_ema: Ema::new(signal),
            macd_values: VecDeque::with_capacity(signal),
            initialized: false,
        }
    }

    pub fn update(&mut self, close: f64) -> Option<(f64, f64, f64)> {
        let fast_val = self.fast_ema.update(close)?;
        let slow_val = self.slow_ema.update(close)?;
        let macd_line = fast_val - slow_val;
        self.macd_values.push_back(macd_line);
        if self.macd_values.len() > self.signal_ema.alpha.recip() as usize + 1 {
            self.macd_values.pop_front();
        }
        let signal_line = self.signal_ema.update(macd_line).unwrap_or(macd_line);
        let histogram = macd_line - signal_line;
        Some((macd_line, signal_line, histogram))
    }

    pub fn reset(&mut self) {
        self.fast_ema.reset();
        self.slow_ema.reset();
        self.signal_ema.reset();
        self.macd_values.clear();
        self.initialized = false;
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // SMA tests
    #[test]
    fn test_sma_rolling() {
        let mut sma = Sma::new(3);
        assert_eq!(sma.update(1.0), None);
        assert_eq!(sma.update(2.0), None);
        assert!((sma.update(3.0).unwrap() - 2.0).abs() < 0.001);
        // Rolling: [2,3,4] → mean=3
        assert!((sma.update(4.0).unwrap() - 3.0).abs() < 0.001);
    }

    #[test]
    fn test_sma_20_reference() {
        let mut sma = Sma::new(20);
        for i in 1..=20 {
            sma.update(i as f64);
        }
        // After 20th update: buffer full with [1..20], mean=10.5
        // Next update (21): removes 1, adds 21 → window [2..21], mean=11.5
        let result = sma.update(21.0).unwrap();
        assert!((result - 11.5).abs() < 0.001);
        // Next: [3..22], mean=12.5
        let result2 = sma.update(22.0).unwrap();
        assert!((result2 - 12.5).abs() < 0.001);
    }

    // EMA tests
    #[test]
    fn test_ema_initializes() {
        let mut ema = Ema::new(10);
        // First update: initialization
        assert_eq!(ema.update(100.0), Some(100.0));
        // Subsequent: EMA smoothing
        assert_eq!(ema.update(110.0), Some(100.1817)); // alpha ≈ 0.1817
    }

    // RSI tests
    #[test]
    fn test_rsi_warmup() {
        let mut rsi = Rsi::new(3);
        // Updates 1-2: still warming up
        rsi.update(100.0); // last_price = 100
        assert_eq!(rsi.update(105.0), None); // count=1, < period
        assert_eq!(rsi.update(110.0), None); // count=2, < period
        // Update 3: period complete → first RSI
        let result = rsi.update(108.0);
        assert!(result.is_some());
    }

    // Stochastic tests
    #[test]
    fn test_stochastic_warmup() {
        let mut stoch = Stochastic::new(3, 2);
        assert_eq!(stoch.update(100.0), None);
        assert_eq!(stoch.update(105.0), None);
        // 3rd update: warmup complete
        let result = stoch.update(103.0);
        assert!(result.is_some());
    }

    // ATR tests
    #[test]
    fn test_atr_warmup() {
        let mut atr = Atr::new(3);
        assert_eq!(atr.update(100.0), None); // init
        assert_eq!(atr.update(102.0), None); // count=1
        assert_eq!(atr.update(104.0), None); // count=2
        // count=3 == period → first ATR
        let result = atr.update(106.0);
        assert!(result.is_some());
        // count=4 → Wilder smoothing
        let result2 = atr.update(108.0);
        assert!(result2.is_some());
    }

    // VWAP tests
    #[test]
    fn test_vwap_immediate() {
        let mut vwap = Vwap::new();
        // VWAP is immediately available
        assert_eq!(vwap.update(100.0, 10.0), Some(100.0));
        // (100*10 + 110*5) / 15 = 103.333
        assert!((vwap.update(110.0, 5.0).unwrap() - 103.333).abs() < 0.001);
    }

    #[test]
    fn test_vwap_reset() {
        let mut vwap = Vwap::new();
        vwap.update(100.0, 10.0);
        vwap.update(110.0, 5.0);
        vwap.reset();
        assert_eq!(vwap.value(), 0.0);
    }

    // MACD tests
    #[test]
    fn test_macd_returns_tuple() {
        let mut macd = Macd::new(12, 26, 9);
        let result = macd.update(100.0);
        assert!(result.is_some());
        let (macd_line, signal, histogram) = result.unwrap();
        assert!(histogram.is_finite());
    }
}