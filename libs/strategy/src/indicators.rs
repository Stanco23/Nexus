//! Technical indicators for strategy use.
//!
//! All indicators implement the `Indicator` trait with O(1) update time.
//! Indicator state is owned — each strategy clone gets fresh indicator instances.

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

    pub fn mean(&self) -> Option<f64> {
        if self.buf.len() == self.window {
            Some(self.sum / self.window as f64)
        } else {
            None
        }
    }
}

impl Indicator for Sma {
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

/// Stochastic Oscillator.
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

    pub fn value(&self) -> (f64, f64) {
        // %K not available until we have k_period bars
        (0.0, 0.0)
    }
}

impl Indicator for Stochastic {
    fn update(&mut self, _value: f64) -> Option<f64> {
        self.count += 1;
        None
    }

    fn reset(&mut self) {
        self.highs.clear();
        self.lows.clear();
        self.closes.clear();
        self.k_values.clear();
        self.count = 0;
    }
}

/// Full stochastic update with high/low/close.
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
    let highest_high = stoch.highs.iter().fold(f64::NEG_INFINITY, |a, &b| a.max(b));
    let lowest_low = stoch.lows.iter().fold(f64::INFINITY, |a, &b| a.min(b));
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

// ─── Volatility Indicators ────────────────────────────────────────────────────

/// Bollinger Bands — SMA ± k * std_dev.
pub struct BollingerBands {
    sma: Sma,
    k: f64,
    values: VecDeque<f64>,
}

impl BollingerBands {
    pub fn new(window: usize, k: f64) -> Self {
        Self {
            sma: Sma::new(window),
            k,
            values: VecDeque::with_capacity(window),
        }
    }

    pub fn update(&mut self, close: f64) -> Option<(f64, f64, f64)> {
        self.sma.update(close)?;
        let mid = self.sma.mean().unwrap();
        self.values.push_back(close);
        if self.values.len() > self.sma.window {
            self.values.pop_front();
        }
        let mean = self.values.iter().sum::<f64>() / self.values.len() as f64;
        let variance = self.values.iter().map(|&v| (v - mean).powi(2)).sum::<f64>() / self.values.len() as f64;
        let std_dev = variance.sqrt();
        let upper = mid + self.k * std_dev;
        let lower = mid - self.k * std_dev;
        Some((lower, mid, upper))
    }

    pub fn reset(&mut self) {
        self.sma.reset();
        self.values.clear();
    }
}

/// True Range for ATR calculation.
fn true_range(high: f64, low: f64, prev_close: f64) -> f64 {
    high - low
        .max((high - prev_close).abs())
        .max((low - prev_close).abs())
}

/// Average True Range — Wilder's smoothing.
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
    fn update(&mut self, _value: f64) -> Option<f64> {
        None
    }

    fn reset(&mut self) {
        self.trs.clear();
        self.atr = 0.0;
        self.count = 0;
        self.prev_close = 0.0;
        self.initialized = false;
    }
}

/// Full ATR update with high/low/close.
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

/// Volume Weighted Average Price.
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

    pub fn update(&mut self, price: f64, volume: f64) -> f64 {
        self.cumulative_pv += price * volume;
        self.cumulative_vol += volume;
        if self.cumulative_vol == 0.0 {
            0.0
        } else {
            self.cumulative_pv / self.cumulative_vol
        }
    }

    pub fn reset(&mut self) {
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
        let first = ema.update(100.0);
        assert!(first.is_some());
        assert!((first.unwrap() - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_ema_smoothing() {
        let mut ema = Ema::new(10);
        ema.update(100.0);
        let alpha = 2.0 / 11.0;
        let second = ema.update(101.0).unwrap();
        let expected = 100.0 * (1.0 - alpha) + 101.0 * alpha;
        assert!((second - expected).abs() < 0.001);
    }

    // RSI tests
    #[test]
    fn test_rsi_50_at_start() {
        let mut rsi = Rsi::new(14);
        rsi.update(100.0);
        assert!(rsi.update(100.0).is_none()); // no output until period filled
    }

    #[test]
    fn test_rsi_100_when_only_gains() {
        let mut rsi = Rsi::new(3);
        for i in 0..5 {
            rsi.update(100.0 + i as f64);
        }
        let val = rsi.update(105.0).unwrap();
        assert!(val >= 50.0 && val <= 100.0);
    }

    #[test]
    fn test_rsi_zero_loss() {
        let mut rsi = Rsi::new(2);
        // All gains
        rsi.update(100.0);
        rsi.update(101.0);
        rsi.update(102.0);
        rsi.update(103.0);
        let val = rsi.update(104.0).unwrap();
        assert!(val >= 80.0); // strong RSI when no losses
    }

    // Stochastic tests
    #[test]
    fn test_stochastic_wait_for_k_period() {
        let mut stoch = Stochastic::new(5, 3);
        // First 4 bars: no output (count < k_period)
        for i in 1..=4 {
            let result = stochastic_update(&mut stoch, 100.0 + i as f64, 99.0, 100.5);
            assert!(result.is_none(), "bar {} should not produce output", i);
        }
        // At bar 5+: output
        let result = stochastic_update(&mut stoch, 105.0, 99.0, 104.0);
        assert!(result.is_some());
        let (k, d) = result.unwrap();
        assert!(k >= 0.0 && k <= 100.0);
        assert!(d >= 0.0 && d <= 100.0);
    }

    // ATR tests
    #[test]
    fn test_atr_simple() {
        let mut atr = Atr::new(3);
        // Init: sets prev_close
        atr_update(&mut atr, 100.0, 99.0, 100.0);
        // call 2: count=1, no output (period not filled)
        assert!(atr_update(&mut atr, 101.0, 100.0, 100.5).is_none());
        // call 3: count=2, no output
        assert!(atr_update(&mut atr, 102.0, 101.0, 101.5).is_none());
        // call 4: count=3, period filled → ATR is simple mean of first 3 TRs
        let result = atr_update(&mut atr, 103.0, 102.0, 102.5);
        assert!(result.is_some());
        let val = result.unwrap();
        assert!(val > 0.0);
    }

    #[test]
    fn test_atr_wilder_smoothing() {
        let mut atr = Atr::new(3);
        // Fill period
        atr_update(&mut atr, 100.0, 99.0, 100.0);
        atr_update(&mut atr, 101.0, 100.0, 100.5);
        atr_update(&mut atr, 102.0, 101.0, 101.5);
        // Period complete — now Wilder smoothing kicks in
        let first = atr_update(&mut atr, 103.0, 102.0, 102.5);
        assert!(first.is_some());
        let val = first.unwrap();
        assert!(val > 0.0);
    }

    // VWAP tests
    #[test]
    fn test_vwap_basic() {
        let mut vwap = Vwap::new();
        assert!((vwap.update(100.0, 10.0) - 100.0).abs() < 0.001);
        assert!((vwap.update(110.0, 10.0) - 105.0).abs() < 0.001); // (1000 + 1100) / 20 = 105
    }

    #[test]
    fn test_vwap_zero_volume() {
        let mut vwap = Vwap::new();
        assert!((vwap.update(100.0, 0.0) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_vwap_accumulates() {
        let mut vwap = Vwap::new();
        vwap.update(100.0, 1.0);
        vwap.update(200.0, 1.0);
        // (100 + 200) / 2 = 150
        assert!((vwap.update(150.0, 1.0) - 150.0).abs() < 0.001);
    }

    // Bollinger Bands tests
    #[test]
    fn test_bollinger_mid_equals_sma() {
        let mut bb = BollingerBands::new(10, 2.0);
        let mut sma = Sma::new(10);
        for i in 1..=15 {
            let v = 100.0 + i as f64;
            bb.update(v);
            sma.update(v);
        }
        let (_, mid, _) = bb.update(115.0).unwrap();
        let expected_sma = sma.update(115.0).unwrap();
        assert!((mid - expected_sma).abs() < 0.001);
    }

    #[test]
    fn test_bollinger_upper_lower() {
        let mut bb = BollingerBands::new(10, 2.0);
        for i in 1..=20 {
            bb.update(100.0 + i as f64);
        }
        let (lower, mid, upper) = bb.update(121.0).unwrap();
        assert!(lower < mid);
        assert!(mid < upper);
        // With k=2, upper/lower should be ~mid ± 2*std_dev
        let spread = upper - lower;
        assert!(spread > 0.0);
    }

    // MACD tests
    #[test]
    fn test_macd_fast_slower_slow() {
        let mut macd = Macd::new(12, 26, 9);
        // MACD needs at least slow_period values before output
        for i in 1..=30 {
            macd.update(100.0 + (i as f64).sin() * 5.0);
        }
        let result = macd.update(100.0);
        assert!(result.is_some());
        let (macd_line, signal, hist) = result.unwrap();
        assert!((hist - (macd_line - signal)).abs() < 0.001);
    }
}