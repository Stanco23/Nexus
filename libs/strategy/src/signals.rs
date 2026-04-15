//! Signal subsystem — decoupled pub/sub for named signal events.
//!
//! Unlike `Signal` (Buy/Sell/Close trading action), `NamedSignal` is a data event
//! e.g., "RSI_OVERSOLD", "BB_LOW". Indicators publish to `SignalBus`; strategies
//! subscribe to specific signal names and receive callbacks when they fire.
//!
//! # Nautilus Source
//! `common/signal.py` (dynamic SignalData generation), `common/actor.pyx` (publish_signal / subscribe_signal)

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Callback type for signal notifications: (signal_name, value, timestamp_ns).
pub type SignalCallback = Box<dyn Fn(&str, f64, u64) + Send + Sync>;

/// A named signal event (e.g., "RSI_OVERSOLD", "BB_LOW").
#[derive(Debug, Clone)]
pub struct SignalEvent {
    pub name: String,
    pub value: f64,
    pub timestamp_ns: u64,
}

/// Condition for triggering a signal.
#[derive(Debug, Clone)]
pub enum SignalCondition {
    /// Fire when indicator crosses above threshold.
    CrossAbove { threshold: f64 },
    /// Fire when indicator crosses below threshold.
    CrossBelow { threshold: f64 },
    /// Fire when indicator value is above threshold.
    Above { threshold: f64 },
    /// Fire when indicator value is below threshold.
    Below { threshold: f64 },
}

/// Signal bus — synchronous pub/sub for named signal events.
///
/// Subscribers register callbacks for specific signal names (or "*" for all).
/// Publishing immediately invokes all matching callbacks in the calling thread.
#[derive(Default)]
pub struct SignalBus {
    subscribers: RwLock<HashMap<String, Vec<SignalCallback>>>,
}

impl SignalBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe a callback to a named signal.
    ///
    /// Use `"*"` as `name` to receive all signals.
    pub fn subscribe(&self, name: &str, callback: SignalCallback) {
        let mut subs = self.subscribers.write().unwrap();
        subs.entry(name.to_string()).or_default().push(callback);
    }

    /// Publish a signal — immediately invokes all matching callbacks.
    pub fn publish(&self, name: &str, value: f64, timestamp_ns: u64) {
        let subs = self.subscribers.read().unwrap();
        // Exact match
        if let Some(callbacks) = subs.get(name) {
            for cb in callbacks {
                cb(name, value, timestamp_ns);
            }
        }
        // Wildcard subscribers
        if let Some(callbacks) = subs.get("*") {
            for cb in callbacks {
                cb(name, value, timestamp_ns);
            }
        }
    }

    /// Unsubscribe all callbacks for a signal name.
    pub fn unsubscribe(&self, name: &str) {
        let mut subs = self.subscribers.write().unwrap();
        subs.remove(name);
    }

    /// Clear all subscribers.
    pub fn clear(&self) {
        let mut subs = self.subscribers.write().unwrap();
        subs.clear();
    }
}

impl Clone for SignalBus {
    fn clone(&self) -> Self {
        // RwLock is not Clone, but we share via Arc in practice
        Self::default()
    }
}

/// Thread-safe handle to a `SignalBus` for sharing across workers.
pub type SharedSignalBus = Arc<SignalBus>;

/// Indicator wrapper that publishes signal events to a `SignalBus` when conditions fire.
pub struct SignalIndicator {
    indicator: Box<dyn crate::indicators::Indicator>,
    signal_name: String,
    condition: SignalCondition,
    last_value: Option<f64>,
    bus: SharedSignalBus,
}

impl SignalIndicator {
    pub fn new(
        indicator: Box<dyn crate::indicators::Indicator>,
        signal_name: &str,
        condition: SignalCondition,
        bus: SharedSignalBus,
    ) -> Self {
        Self {
            indicator,
            signal_name: signal_name.to_string(),
            condition,
            last_value: None,
            bus,
        }
    }

    /// Update the indicator and fire signals if conditions are met.
    /// Returns the indicator's value (same as calling the indicator directly).
    pub fn update(&mut self, value: f64, timestamp_ns: u64) -> Option<f64> {
        let result = self.indicator.update(value);
        let current = result.unwrap_or(value);

        let should_fire = match (&self.condition, self.last_value) {
            (SignalCondition::CrossAbove { threshold }, Some(prev)) => {
                prev <= *threshold && current > *threshold
            }
            (SignalCondition::CrossBelow { threshold }, Some(prev)) => {
                prev >= *threshold && current < *threshold
            }
            (SignalCondition::Above { threshold }, _) => current > *threshold,
            (SignalCondition::Below { threshold }, _) => current < *threshold,
            (_, None) => false,
        };

        if should_fire {
            self.bus
                .publish(&self.signal_name, current, timestamp_ns);
        }

        self.last_value = Some(current);
        result
    }

    /// Reset the internal indicator state.
    pub fn reset(&mut self) {
        self.indicator.reset();
        self.last_value = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signal_bus_subscribe_and_publish() {
        let bus = SignalBus::new();
        let received = Arc::new(RwLock::new(Vec::new()));
        let received_clone = received.clone();

        bus.subscribe("RSI_OVERSOLD", Box::new(move |name, value, _ts| {
            received_clone.write().unwrap().push((name.to_string(), value));
        }));

        bus.publish("RSI_OVERSOLD", 25.0, 1000);
        bus.publish("RSI_OVERSOLD", 28.0, 2000);

        let r = received.read().unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "RSI_OVERSOLD");
        assert_eq!(r[0].1, 25.0);
    }

    #[test]
    fn test_signal_bus_wildcard() {
        let bus = SignalBus::new();
        let received = Arc::new(RwLock::new(Vec::new()));
        let received_clone = received.clone();

        bus.subscribe("*", Box::new(move |name, value, _ts| {
            received_clone.write().unwrap().push((name.to_string(), value));
        }));

        bus.publish("RSI_OVERSOLD", 25.0, 1000);
        bus.publish("BB_LOW", 99.5, 2000);

        let r = received.read().unwrap();
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn test_signal_bus_unsubscribe() {
        let bus = SignalBus::new();
        let counter = Arc::new(RwLock::new(0));
        let counter_clone = counter.clone();

        bus.subscribe("TEST", Box::new(move |_, _, _| {
            *counter_clone.write().unwrap() += 1;
        }));

        bus.publish("TEST", 1.0, 1000);
        assert_eq!(*counter.read().unwrap(), 1);

        bus.unsubscribe("TEST");
        bus.publish("TEST", 2.0, 2000);
        assert_eq!(*counter.read().unwrap(), 1);
    }

    #[test]
    fn test_signal_indicator_cross_below() {
        let bus: SharedSignalBus = Arc::new(SignalBus::new());
        let received = Arc::new(RwLock::new(Vec::new()));
        let received_clone = received.clone();

        bus.subscribe("SMA_CROSS_DOWN", Box::new(move |name, value, _ts| {
            received_clone.write().unwrap().push((name.to_string(), value));
        }));

        // Use SMA(3) and signal on cross below threshold 115
        // Values: 110, 120, 115 → SMA(3) = 115.0 (exactly at threshold, no cross yet)
        // Then 105 → SMA(3) = (120+115+105)/3 = 113.3 (still above 115)
        // Then 90 → SMA(3) = (115+105+90)/3 = 103.3 < 115 → cross below!
        let mut indicator = SignalIndicator::new(
            Box::new(crate::indicators::Sma::new(3)),
            "SMA_CROSS_DOWN",
            SignalCondition::CrossBelow { threshold: 115.0 },
            bus,
        );

        indicator.update(110.0, 1); // buf=[110], no SMA output yet
        indicator.update(120.0, 2); // buf=[110,120], no SMA output yet
        indicator.update(115.0, 3); // buf=[110,120,115], SMA=115.0, prev=None (no cross possible)
        // SMA = 115.0, last_value = 115.0

        indicator.update(105.0, 4); // buf=[120,115,105], SMA=113.3, prev=115.0
        // 115.0 <= 115.0 && 113.3 < 115.0 → cross below! FIRE!
        assert_eq!(received.read().unwrap().len(), 1, "should fire on cross below 115");

        indicator.update(90.0, 5); // buf=[115,105,90], SMA=103.3, prev=113.3
        // 113.3 <= 115.0 && 103.3 < 115.0 → still below, no new cross (already fired)

        let r = received.read().unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "SMA_CROSS_DOWN");
    }

    #[test]
    fn test_signal_indicator_below_condition() {
        let bus: SharedSignalBus = Arc::new(SignalBus::new());
        let received = Arc::new(RwLock::new(Vec::new()));
        let received_clone = received.clone();

        bus.subscribe("PRICE_LOW", Box::new(move |name, value, _ts| {
            received_clone.write().unwrap().push((name.to_string(), value));
        }));

        let mut indicator = SignalIndicator::new(
            Box::new(crate::indicators::Sma::new(3)),
            "PRICE_LOW",
            SignalCondition::Below { threshold: 105.0 },
            bus,
        );

        // Feed values: 110 (no fire, above 105), 100 (fire, below 105), 112 (no fire)
        indicator.update(110.0, 1); // no fire
        indicator.update(100.0, 2); // fire
        indicator.update(112.0, 3); // no fire

        let r = received.read().unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "PRICE_LOW");
        assert!((r[0].1 - 100.0).abs() < 0.001);
    }

    #[test]
    fn test_signal_event_struct() {
        let event = SignalEvent {
            name: "TEST_SIGNAL".into(),
            value: 42.5,
            timestamp_ns: 1_000_000_000,
        };
        assert_eq!(event.name, "TEST_SIGNAL");
        assert!((event.value - 42.5).abs() < 0.001);
        assert_eq!(event.timestamp_ns, 1_000_000_000);
    }

    #[test]
    fn test_signal_condition_debug() {
        let c1 = SignalCondition::CrossAbove { threshold: 70.0 };
        let c2 = SignalCondition::Below { threshold: 30.0 };
        assert!(format!("{:?}", c1).contains("CrossAbove"));
        assert!(format!("{:?}", c2).contains("Below"));
    }
}
