//! Signal subsystem for nexus.
//!
//! Provides in-process pub/sub for named signal events. Unlike `Signal` (Buy/Sell/Close
//! trading action), `NamedSignal` carries arbitrary data e.g., "RSI_OVERSOLD", "BB_LOW".

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
        if let Some(callbacks) = subs.get(name) {
            for cb in callbacks {
                cb(name, value, timestamp_ns);
            }
        }
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

impl std::fmt::Debug for SignalBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalBus").finish()
    }
}

impl Clone for SignalBus {
    fn clone(&self) -> Self {
        Self::default()
    }
}