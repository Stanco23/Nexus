//! Actor + MessageBus Architecture — Subsystem 5 (Live Trading) foundation.
//!
//! Provides the core messaging and lifecycle infrastructure for all live/paper
//! trading components. All operations are synchronous and single-threaded.
//!
//! # Architecture
//!
//! - [`Clock`] trait — time source abstraction (TestClock for backtest, SystemClock for live)
//! - [`ComponentState`] / [`ComponentTrigger`] — 16-state FSM matching Nautilus
//! - [`FiniteStateMachine`] — generic FSM with static transition table
//! - [`MessageBus`] — pub/sub with wildcard topics, endpoint registration, priority ordering
//! - [`Component`] — base struct with clock, msgbus, logger, FSM, lifecycle methods
//! - [`Actor`] — extends Component with trader_id and typed message processing
//!
//! # Message Types
//!
//! - [`ShutdownSystem`] — command to initiate system shutdown
//! - [`ComponentStateChanged`] — event published on every state transition
//! - [`TimeEvent`] — timer event from clock
//!
//! # Lifecycle
//!
//! ```text
//! PRE_INITIALIZED
//!     └─(Initialize)─→ INITIALIZED ─→ READY ─→ STARTING ─→ RUNNING
//!                          ↑           ↓         ↓           ↓
//!                     (Dispose)   (Reset)   (Stop)    (Degrade)
//!                          ↓           ↓         ↓           ↓
//!                      DISPOSED   READY   STOPPING → STOPPED   DEGRADED
//!                                                            ↓
//!                                                         RESUMING
//! ```
//!
//! Nautilus Source: `common/actor.pyx`, `common/component.pyx`, `common/messages.pyx`

use crate::cache::{Bar, OrderBook, QuoteTick, TradeTick};
use crate::instrument::Instrument;
use crate::messages::{
    FundingRateUpdate, IndexPriceUpdate, InstrumentClose, InstrumentStatus,
    MarkPriceUpdate, OrderAccepted, OrderCancelled, OrderFilled, OrderRejected,
    PositionChanged, PositionClosed, PositionOpened, SignalData, OrderSubmitted, TraderId,
};
use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
#[allow(unused_imports)]
use std::rc::Rc;
use std::sync::Arc;

// =============================================================================
// SECTION 1: Clock trait + implementations
// =============================================================================

/// Clock time source — object-safe trait for time abstraction.
/// Implementations: [`TestClock`] for backtesting, [`SystemClock`] for live trading.
pub trait Clock {
    /// Return the current UNIX timestamp in nanoseconds.
    fn timestamp_ns(&self) -> u64;

    /// Return the current UNIX timestamp in milliseconds.
    fn timestamp_ms(&self) -> u64 {
        self.timestamp_ns() / 1_000_000
    }

    /// Return the names of all active (pending) timers.
    fn timer_names(&self) -> Vec<String>;

    /// Return the count of active timers.
    fn timer_count(&self) -> usize;

    /// Set a one-shot timer to fire at the given deadline (UTC nanoseconds).
    ///
    /// When the deadline is reached, `handler` is called with the `TimeEvent`.
    /// If no handler is provided, the default handler (if registered) receives it.
    /// The name must be unique per clock.
    fn set_timer(&mut self, name: &str, deadline_ns: u64, handler: Box<dyn FnMut(TimeEvent)>);

    /// Set a one-shot timer with no specific handler (uses default handler).
    fn set_timer_anonymous(&mut self, name: &str, deadline_ns: u64);

    /// Set a repeating timer with an interval.
    ///
    /// Fires first at `start_ns + interval_ns`, then every `interval_ns` after.
    /// If `stop_ns` is provided, the timer stops after that deadline.
    /// If `fire_immediately` is true, also fires at `start_ns`.
    fn set_timer_repeating(
        &mut self,
        name: &str,
        interval_ns: u64,
        start_ns: u64,
        stop_ns: Option<u64>,
        handler: Box<dyn FnMut(TimeEvent)>,
        fire_immediately: bool,
    );

    /// Cancel the timer with the given name.
    fn cancel_timer(&mut self, name: &str);

    /// Cancel all pending timers.
    fn cancel_timers(&mut self);

    /// Find the next deadline for the given timer name.
    /// Returns 0 if the timer is not found.
    fn next_time_ns(&self, name: &str) -> u64;

    /// Register a default handler for timers that have no specific handler.
    fn register_default_handler(&mut self, handler: Box<dyn FnMut(TimeEvent)>);

    /// Advance the clock to `to_ns` and return all timer events that are due.
    /// Timers are consumed once returned.
    fn advance_time(&mut self, to_ns: u64) -> Vec<TimeEvent>;
}

/// SystemClock — wall-clock time for production use.
/// Timers are managed by an external event loop (tokio/etc); this clock
/// provides wall-clock timestamps and delegates timer handling to the runtime.
pub struct SystemClock {
    #[allow(dead_code)]
    start_ns: u64,
    /// Default handler for orphaned timer events (optional).
    default_handler: Option<Box<dyn FnMut(TimeEvent)>>,
}

impl SystemClock {
    pub fn new() -> Self {
        Self {
            start_ns: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64,
            default_handler: None,
        }
    }

    /// Set the default handler for timers without a specific callback.
    pub fn with_default_handler(mut self, handler: impl FnMut(TimeEvent) + 'static) -> Self {
        self.default_handler = Some(Box::new(handler));
        self
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn timestamp_ns(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos() as u64
    }

    fn timer_names(&self) -> Vec<String> {
        Vec::new() // SystemClock timers are managed externally
    }

    fn timer_count(&self) -> usize {
        0
    }

    fn set_timer(&mut self, _name: &str, _deadline_ns: u64, _handler: Box<dyn FnMut(TimeEvent)>) {
        // No-op: SystemClock timers are handled by the external event loop
    }

    fn set_timer_anonymous(&mut self, _name: &str, _deadline_ns: u64) {}

    fn set_timer_repeating(
        &mut self,
        _name: &str,
        _interval_ns: u64,
        _start_ns: u64,
        _stop_ns: Option<u64>,
        _handler: Box<dyn FnMut(TimeEvent)>,
        _fire_immediately: bool,
    ) {
        // No-op
    }

    fn cancel_timer(&mut self, _name: &str) {
        // No-op
    }

    fn cancel_timers(&mut self) {}

    fn next_time_ns(&self, _name: &str) -> u64 {
        0
    }

    fn register_default_handler(&mut self, handler: Box<dyn FnMut(TimeEvent)>) {
        self.default_handler = Some(handler);
    }

    fn advance_time(&mut self, _to_ns: u64) -> Vec<TimeEvent> {
        Vec::new() // SystemClock timers are handled externally by the event loop
    }
}

/// TestClock — deterministic clock for backtesting.
///
/// Timers are keyed by name for O(log n) lookup. Each timer carries an
/// optional callback. Repeating timers re-schedule themselves after firing.
///
/// Call `set_time` to set the current time, then `advance_time` to dispatch
/// due timers.
#[derive(Default)]
pub struct TestClock {
    current_ns: u64,
    /// Active timers: name → TimerData
    timers: BTreeMap<String, TimerData>,
    /// Default handler for timers without a specific callback.
    default_handler: Option<Box<dyn FnMut(TimeEvent)>>,
}

/// Internal timer state.
struct TimerData {
    /// Next deadline in nanoseconds.
    deadline_ns: u64,
    /// Interval for repeating timers (0 for one-shot).
    interval_ns: u64,
    /// Optional stop deadline (None = never stops).
    stop_ns: Option<u64>,
    /// Callback closure.
    callback: Option<Box<dyn FnMut(TimeEvent)>>,
    /// Whether the timer was already cancelled during this advance cycle.
    cancelled: bool,
}

impl TestClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the clock's current time (must be >= current time).
    pub fn set_time(&mut self, ns: u64) {
        assert!(
            ns >= self.current_ns,
            "TestClock cannot move backwards: {} -> {}",
            self.current_ns,
            ns
        );
        self.current_ns = ns;
    }

    /// Return the current time without advancing.
    pub fn now_ns(&self) -> u64 {
        self.current_ns
    }
}

impl Clock for TestClock {
    fn timestamp_ns(&self) -> u64 {
        self.current_ns
    }

    fn timer_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.timers.keys().cloned().collect();
        names.sort();
        names
    }

    fn timer_count(&self) -> usize {
        self.timers.len()
    }

    fn set_timer(&mut self, name: &str, deadline_ns: u64, handler: Box<dyn FnMut(TimeEvent)>) {
        self.timers.insert(
            name.to_string(),
            TimerData {
                deadline_ns,
                interval_ns: 0, // one-shot
                stop_ns: None,
                callback: Some(handler),
                cancelled: false,
            },
        );
    }

    fn set_timer_anonymous(&mut self, name: &str, deadline_ns: u64) {
        self.timers.insert(
            name.to_string(),
            TimerData {
                deadline_ns,
                interval_ns: 0,
                stop_ns: None,
                callback: None,
                cancelled: false,
            },
        );
    }

    fn set_timer_repeating(
        &mut self,
        name: &str,
        interval_ns: u64,
        start_ns: u64,
        stop_ns: Option<u64>,
        handler: Box<dyn FnMut(TimeEvent)>,
        fire_immediately: bool,
    ) {
        let deadline_ns = if fire_immediately {
            start_ns
        } else {
            start_ns.saturating_add(interval_ns)
        };

        self.timers.insert(
            name.to_string(),
            TimerData {
                deadline_ns,
                interval_ns,
                stop_ns,
                callback: Some(handler),
                cancelled: false,
            },
        );
    }

    fn cancel_timer(&mut self, name: &str) {
        self.timers.remove(name);
    }

    fn cancel_timers(&mut self) {
        self.timers.clear();
    }

    fn next_time_ns(&self, name: &str) -> u64 {
        self.timers.get(name).map(|t| t.deadline_ns).unwrap_or(0)
    }

    fn register_default_handler(&mut self, handler: Box<dyn FnMut(TimeEvent)>) {
        self.default_handler = Some(handler);
    }

    fn advance_time(&mut self, to_ns: u64) -> Vec<TimeEvent> {
        assert!(
            to_ns >= self.current_ns,
            "TestClock cannot advance backwards: {} -> {}",
            self.current_ns,
            to_ns
        );
        self.current_ns = to_ns;

        let mut fired: Vec<TimeEvent> = Vec::new();
        let mut to_remove: Vec<String> = Vec::new();
        let mut to_reschedule: Vec<(String, TimerData)> = Vec::new();

        for (name, timer) in self.timers.iter_mut() {
            if timer.deadline_ns <= to_ns && !timer.cancelled {
                let event = TimeEvent {
                    id: 0,
                    name: name.clone(),
                    payload: Vec::new(),
                    timestamp_ns: timer.deadline_ns,
                };

                // Invoke callback
                if let Some(ref mut cb) = timer.callback {
                    cb(event.clone());
                } else if let Some(ref mut dh) = self.default_handler {
                    dh(event.clone());
                }

                fired.push(event);

                if timer.interval_ns > 0 {
                    // Repeating: reschedule
                    let next_deadline = timer.deadline_ns.saturating_add(timer.interval_ns);
                    if let Some(stop) = timer.stop_ns {
                        if next_deadline > stop {
                            to_remove.push(name.clone());
                            continue;
                        }
                    }
                    timer.deadline_ns = next_deadline;
                    timer.cancelled = false;
                    to_reschedule.push((name.clone(), TimerData {
                        deadline_ns: timer.deadline_ns,
                        interval_ns: timer.interval_ns,
                        stop_ns: timer.stop_ns,
                        callback: timer.callback.take(), // move callback out
                        cancelled: false,
                    }));
                } else {
                    to_remove.push(name.clone());
                }
            }
        }

        // Remove fired one-shot timers
        for name in &to_remove {
            self.timers.remove(name);
        }
        // Re-insert repeating timers with moved callbacks
        for (name, data) in to_reschedule {
            self.timers.insert(name, data);
        }

        fired.sort_by_key(|e| e.timestamp_ns);
        fired
    }
}

// =============================================================================
// SECTION 2: TimeEvent
// =============================================================================

/// A timer event returned by `Clock::advance_time`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimeEvent {
    /// The timer identifier (0 for named timers).
    pub id: u64,
    /// The timer name (empty string for anonymous timers).
    pub name: String,
    /// The payload passed when the timer was set.
    pub payload: Vec<u8>,
    /// The timestamp (deadline) at which the timer fired.
    pub timestamp_ns: u64,
}

// =============================================================================
// SECTION 3: ComponentState + ComponentTrigger enums
// =============================================================================

/// The 16 states of a Nautilus Component (from `common/component.pyx`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum ComponentState {
    PreInitialized = 0,
    Initialized = 1,
    Ready = 2,
    Starting = 3,
    Running = 4,
    Stopping = 5,
    Stopped = 6,
    Resetting = 7,
    Disposing = 8,
    Disposed = 9,
    Faulting = 10,
    Faulted = 11,
    Degrading = 12,
    Degraded = 13,
    Resuming = 14,
}

impl std::fmt::Display for ComponentState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ComponentState::PreInitialized => "PRE_INITIALIZED",
            ComponentState::Initialized => "INITIALIZED",
            ComponentState::Ready => "READY",
            ComponentState::Starting => "STARTING",
            ComponentState::Running => "RUNNING",
            ComponentState::Stopping => "STOPPING",
            ComponentState::Stopped => "STOPPED",
            ComponentState::Resetting => "RESETTING",
            ComponentState::Disposing => "DISPOSING",
            ComponentState::Disposed => "DISPOSED",
            ComponentState::Faulting => "FAULTING",
            ComponentState::Faulted => "FAULTED",
            ComponentState::Degrading => "DEGRADING",
            ComponentState::Degraded => "DEGRADED",
            ComponentState::Resuming => "RESUMING",
        };
        write!(f, "{}", s)
    }
}

/// Triggers that drive Component state transitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentTrigger {
    Initialize,
    Start,
    Stop,
    Reset,
    Dispose,
    Fault,
    Degrade,
    Resume,
    StartCompleted,
    StopCompleted,
    ResetCompleted,
    DisposeCompleted,
    FaultCompleted,
    DegradeCompleted,
    ResumeCompleted,
}

/// The official 16-state transition table from Nautilus.
/// Format: (from_state, trigger, to_state)
/// Transitional states (Starting, Stopping, etc.) auto-advance via _completed triggers.
const COMPONENT_TRANSITIONS: &[(ComponentState, ComponentTrigger, ComponentState)] = &[
    // Initialization
    (
        ComponentState::PreInitialized,
        ComponentTrigger::Initialize,
        ComponentState::Initialized,
    ),
    // Start sequence
    (ComponentState::Initialized, ComponentTrigger::Start, ComponentState::Starting),
    (
        ComponentState::Starting,
        ComponentTrigger::StartCompleted,
        ComponentState::Running,
    ),
    // Stop sequence
    (ComponentState::Running, ComponentTrigger::Stop, ComponentState::Stopping),
    (
        ComponentState::Stopping,
        ComponentTrigger::StopCompleted,
        ComponentState::Stopped,
    ),
    // Reset sequence
    (ComponentState::Ready, ComponentTrigger::Reset, ComponentState::Resetting),
    (
        ComponentState::Resetting,
        ComponentTrigger::ResetCompleted,
        ComponentState::Ready,
    ),
    // Resume sequence
    (ComponentState::Stopped, ComponentTrigger::Resume, ComponentState::Resuming),
    (
        ComponentState::Resuming,
        ComponentTrigger::ResumeCompleted,
        ComponentState::Ready,
    ),
    // Dispose sequence
    (ComponentState::Ready, ComponentTrigger::Dispose, ComponentState::Disposing),
    (
        ComponentState::Disposing,
        ComponentTrigger::DisposeCompleted,
        ComponentState::Disposed,
    ),
    // Fault sequence
    (ComponentState::Running, ComponentTrigger::Fault, ComponentState::Faulting),
    (
        ComponentState::Faulting,
        ComponentTrigger::FaultCompleted,
        ComponentState::Faulted,
    ),
    // Degrade sequence
    (ComponentState::Running, ComponentTrigger::Degrade, ComponentState::Degrading),
    (
        ComponentState::Degrading,
        ComponentTrigger::DegradeCompleted,
        ComponentState::Degraded,
    ),
    // Resume from degraded
    (ComponentState::Degraded, ComponentTrigger::Resume, ComponentState::Resuming),
];

// =============================================================================
// SECTION 4: FiniteStateMachine
// =============================================================================

/// Generic FSM with a static transition table.
/// Transitions are driven by triggers; valid transitions are looked up in the table.
pub struct FiniteStateMachine<S, E>
where
    S: Eq + Copy + 'static,
    E: Eq + Copy + 'static,
{
    current: S,
    table: &'static [(S, E, S)],
}

impl<S, E> FiniteStateMachine<S, E>
where
    S: Eq + Copy,
    E: Eq + Copy,
{
    /// Create a new FSM starting at `initial` with the given transition `table`.
    pub fn new(initial: S, table: &'static [(S, E, S)]) -> Self {
        Self { current: initial, table }
    }

    /// Return the current state.
    pub fn current(&self) -> S {
        self.current
    }

    /// Trigger a transition. Returns `true` if the transition was valid and
    /// applied; `false` if no matching transition exists (state unchanged).
    pub fn trigger(&mut self, trigger: E) -> bool {
        for (from, trig, to) in self.table {
            if *from == self.current && *trig == trigger {
                self.current = *to;
                return true;
            }
        }
        false
    }
}

// =============================================================================
// SECTION 5: Message types
// =============================================================================

/// Base message trait — all messages implement this marker trait.
pub trait Message: Send + Sync + 'static + std::any::Any {}

/// Downcast a `&dyn Any` to a concrete type.
/// Uses the std::any::Any trait for safe type checking.
pub fn downcast<T: 'static>(msg: &dyn std::any::Any) -> Option<&T> {
    msg.downcast_ref::<T>()
}

/// Helper for message downcasting in tests and internal use.
pub trait AsAny: Message {
    fn as_any(&self) -> &dyn Any;
}

impl<T: Message + 'static> AsAny for T {
    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// A command to shut down the entire system.
#[derive(Debug)]
pub struct ShutdownSystem {
    pub reason: String,
}

impl Message for ShutdownSystem {}

/// Event published whenever a component's state changes.
#[derive(Debug, Clone)]
pub struct ComponentStateChanged {
    pub component_id: u64,
    pub component_name: String,
    pub state: ComponentState,
}

impl Message for ComponentStateChanged {}

/// Event published when trading state changes at the RiskEngine.
#[derive(Debug)]
pub struct TradingStateChanged {
    pub component_id: u64,
    pub state: TradingState,
}

impl Message for TradingStateChanged {}

/// Trading state for the risk engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TradingState {
    #[default]
    Active,
    Halted,
    ReduceOnly,
}

// =============================================================================
// SECTION 6: MessageBus
// =============================================================================

type Handler = Box<dyn Fn(&dyn std::any::Any)>;

/// A synchronous message bus with pub/sub, endpoint registration, and wildcard topics.
///
/// All operations are synchronous: `publish` delivers immediately to all handlers.
/// Uses `RefCell` for interior mutability (single-threaded, same event loop).
///
/// Topic patterns support glob wildcards:
/// - `*` matches one or more characters
/// - `?` matches a single character
pub struct MessageBus {
    #[allow(clippy::arc_with_non_send_sync)]
    inner: Arc<BusInner>,
}

#[allow(clippy::arc_with_non_send_sync)]
struct BusInner {
    #[allow(clippy::type_complexity)]
    subscriptions: RefCell<HashMap<String, Vec<(i32, u64, Arc<Handler>)>>>,
    endpoints: RefCell<HashMap<String, Handler>>,
}

impl Default for MessageBus {
    fn default() -> Self {
        Self {
            #[allow(clippy::arc_with_non_send_sync)]
            inner: Arc::new(BusInner {
                subscriptions: RefCell::new(HashMap::new()),
                endpoints: RefCell::new(HashMap::new()),
            }),
        }
    }
}

impl MessageBus {
    pub fn new() -> Self {
        Self::default()
    }

    /// Subscribe to a topic with a handler and priority.
    /// Handlers with higher priority receive messages first.
    pub fn subscribe(&self, topic: &str, component_id: u64, handler: Handler, priority: i32) {
        let mut subs = self.inner.subscriptions.borrow_mut();
        let entries = subs.entry(topic.to_string()).or_default();
        #[allow(clippy::arc_with_non_send_sync)]
        entries.push((priority, component_id, Arc::new(handler)));
        entries.sort_by(|a, b| b.0.cmp(&a.0));
    }

    /// Publish a message to all handlers subscribed to the topic (and matching
    /// wildcard patterns).
    pub fn publish(&self, topic: &str, msg: &dyn std::any::Any) {
        let handlers: Vec<Arc<Handler>> = {
            let subs = self.inner.subscriptions.borrow();
            let mut out = Vec::new();
            for (pattern, entries) in subs.iter() {
                if is_matching(pattern, topic) {
                    for (_, _, handler) in entries {
                        #[allow(clippy::arc_with_non_send_sync)]
                        out.push(Arc::clone(handler));
                    }
                }
            }
            out
        };
        for handler in &handlers {
            handler(msg);
        }
    }

    /// Register a direct endpoint (point-to-point messaging).
    pub fn register(&self, endpoint: &str, handler: Handler) {
        let mut eps = self.inner.endpoints.borrow_mut();
        eps.insert(endpoint.to_string(), handler);
    }

    /// Send a message directly to a registered endpoint.
    pub fn send(&self, endpoint: &str, msg: &dyn Message) {
        let eps = self.inner.endpoints.borrow();
        if let Some(handler) = eps.get(endpoint) {
            handler(msg);
        }
    }

    /// Unsubscribe a component from a topic.
    pub fn unsubscribe(&self, topic: &str, component_id: u64) {
        let mut subs = self.inner.subscriptions.borrow_mut();
        if let Some(handlers) = subs.get_mut(topic) {
            handlers.retain(|(_, id, _)| *id != component_id);
        }
    }

    /// Unsubscribe a component from all topics.
    pub fn unsubscribe_all(&self, component_id: u64) {
        let mut subs = self.inner.subscriptions.borrow_mut();
        for handlers in subs.values_mut() {
            handlers.retain(|(_, id, _)| *id != component_id);
        }
    }
}

impl Clone for MessageBus {
    fn clone(&self) -> Self {
        Self {
            #[allow(clippy::arc_with_non_send_sync)]
            inner: Arc::clone(&self.inner),
        }
    }
}

/// Check if a topic matches a glob pattern.
/// - `*` matches one or more characters
/// - `?` matches exactly one character
fn is_matching(pattern: &str, topic: &str) -> bool {
    is_matching_recursive(pattern.as_bytes(), topic.as_bytes())
}

fn is_matching_recursive(pat: &[u8], text: &[u8]) -> bool {
    match (pat.first(), text.first()) {
        // Both exhausted → match
        (None, None) => true,
        // Pattern exhausted, text remaining → no match
        (None, Some(_)) => false,
        // Text exhausted, pattern remaining → only trailing wildcards allowed
        (Some(&p), None) => p == b'*' && is_matching_recursive(&pat[1..], &[]),
        // Both have characters
        (Some(&p), Some(t)) => match p {
            b'*' => {
                // Try zero-char match (skip '*'), then try consuming one char at a time
                is_matching_recursive(&pat[1..], text)
                || is_matching_recursive(pat, &text[1..])
            }
            b'?' => is_matching_recursive(&pat[1..], &text[1..]),
            c => c == *t && is_matching_recursive(&pat[1..], &text[1..]),
        },
    }
}

// =============================================================================
// SECTION 7: Logger
// =============================================================================

/// Simple logger stub — prints to stdout with timestamp and level.
/// Replace with full implementation in Phase 5.1 (file logging, levels, etc.)
pub struct Logger {
    name: String,
}

impl Logger {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string() }
    }

    pub fn info(&self, msg: &str) {
        println!("[{}] INFO  {}: {}", timestamp_iso(), self.name, msg);
    }

    pub fn debug(&self, msg: &str) {
        println!("[{}] DEBUG {}: {}", timestamp_iso(), self.name, msg);
    }

    pub fn warn(&self, msg: &str) {
        println!("[{}] WARN  {}: {}", timestamp_iso(), self.name, msg);
    }

    pub fn error(&self, msg: &str) {
        println!("[{}] ERROR {}: {}", timestamp_iso(), self.name, msg);
    }
}

fn timestamp_iso() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| {
            let secs = d.as_secs();
            let nanos = d.subsec_nanos();
            format!("{}.{:09}", secs, nanos)
        })
        .unwrap_or_else(|_| "0.0".to_string())
}

// =============================================================================
// SECTION 8: Component
// =============================================================================

/// Base struct for all system components.
///
/// A Component has:
/// - A clock for timestamps
/// - A message bus for pub/sub
/// - A logger
/// - A finite state machine for lifecycle
/// - A name and ID
///
/// Components are organized in a hierarchy: Components can be actors, but all
/// actors are components. The Component provides the FSM lifecycle; subclasses
/// add message processing.
pub struct Component {
    pub id: u64,
    pub name: &'static str,
    clock: Box<dyn Clock>,
    msgbus: MessageBus,
    logger: Logger,
    fsm: FiniteStateMachine<ComponentState, ComponentTrigger>,
}

impl Component {
    /// Create a new Component in the PreInitialized state.
    pub fn new(
        id: u64,
        name: &'static str,
        clock: Box<dyn Clock>,
        msgbus: MessageBus,
        logger: Logger,
    ) -> Self {
        Self {
            id,
            name,
            clock,
            msgbus,
            logger,
            fsm: FiniteStateMachine::new(ComponentState::PreInitialized, COMPONENT_TRANSITIONS),
        }
    }

    /// Set the message bus (used when component is registered with a trader).
    pub fn set_msgbus(&mut self, msgbus: MessageBus) {
        self.msgbus = msgbus;
    }

    /// Return the current component state.
    pub fn state(&self) -> ComponentState {
        self.fsm.current()
    }

    /// Initialize the component (PreInitialized → Initialized).
    ///
    /// In Nautilus this happens automatically when a msgbus is set. We call it
    /// explicitly after construction so the lifecycle is clear.
    pub fn initialize(&mut self) {
        if self.fsm.trigger(ComponentTrigger::Initialize) {
            self.publish_state_changed();
        }
    }

    /// Start the component (Initialized → Ready → Starting → Running).
    pub fn start(&mut self) {
        // If already past Initialized, do nothing
        if self.fsm.current() != ComponentState::Initialized {
            return;
        }
        // Transition: Initialized → Starting → Running
        // (Start trigger: Initialized → Starting)
        self.fsm.trigger(ComponentTrigger::Start);
        self.publish_state_changed();
        // Auto-advance: Starting → Running
        self.fsm.trigger(ComponentTrigger::StartCompleted);
        self.publish_state_changed();
    }

    /// Stop the component (Running → Stopping → Stopped).
    pub fn stop(&mut self) {
        if self.fsm.current() != ComponentState::Running {
            return;
        }
        self.fsm.trigger(ComponentTrigger::Stop);
        self.publish_state_changed();
        self.fsm.trigger(ComponentTrigger::StopCompleted);
        self.publish_state_changed();
    }

    /// Reset the component (Ready → Resetting → Ready).
    pub fn reset(&mut self) {
        if self.fsm.current() != ComponentState::Ready {
            return;
        }
        self.fsm.trigger(ComponentTrigger::Reset);
        self.publish_state_changed();
        self.fsm.trigger(ComponentTrigger::ResetCompleted);
        self.publish_state_changed();
    }

    /// Dispose of the component (Ready → Disposing → Disposed).
    pub fn dispose(&mut self) {
        if self.fsm.current() != ComponentState::Ready {
            return;
        }
        self.fsm.trigger(ComponentTrigger::Dispose);
        self.publish_state_changed();
        self.fsm.trigger(ComponentTrigger::DisposeCompleted);
        self.publish_state_changed();
    }

    /// Trigger a fault (Running → Faulting → Faulted).
    pub fn fault(&mut self) {
        if self.fsm.current() != ComponentState::Running {
            return;
        }
        self.fsm.trigger(ComponentTrigger::Fault);
        self.publish_state_changed();
        self.fsm.trigger(ComponentTrigger::FaultCompleted);
        self.publish_state_changed();
    }

    /// Degrade the component (Running → Degrading → Degraded).
    pub fn degrade(&mut self) {
        if self.fsm.current() != ComponentState::Running {
            return;
        }
        self.fsm.trigger(ComponentTrigger::Degrade);
        self.publish_state_changed();
        self.fsm.trigger(ComponentTrigger::DegradeCompleted);
        self.publish_state_changed();
    }

    /// Resume from stopped or degraded state (→ Resuming → Ready).
    pub fn resume(&mut self) {
        let state = self.fsm.current();
        if state != ComponentState::Stopped && state != ComponentState::Degraded {
            return;
        }
        self.fsm.trigger(ComponentTrigger::Resume);
        self.publish_state_changed();
        self.fsm.trigger(ComponentTrigger::ResumeCompleted);
        self.publish_state_changed();
    }

    /// Return the clock's current timestamp.
    pub fn timestamp_ns(&self) -> u64 {
        self.clock.timestamp_ns()
    }

    /// Advance the clock and dispatch due timers.
    pub fn advance_clock(&mut self, to_ns: u64) -> Vec<TimeEvent> {
        self.clock.advance_time(to_ns)
    }

    /// Subscribe to a topic on the message bus.
    pub fn subscribe(
        &self,
        topic: &str,
        handler: Handler,
        priority: i32,
    ) {
        self.msgbus.subscribe(topic, self.id, handler, priority);
    }

    /// Publish to a topic on the message bus.
    pub fn publish(&self, topic: &str, msg: &dyn Message) {
        self.msgbus.publish(topic, msg);
    }

    /// Register an endpoint on the message bus.
    pub fn register_endpoint(&self, endpoint: &str, handler: Handler) {
        self.msgbus.register(endpoint, handler);
    }

    /// Send to a registered endpoint.
    pub fn send(&self, endpoint: &str, msg: &dyn Message) {
        self.msgbus.send(endpoint, msg);
    }

    /// Unsubscribe from a topic.
    pub fn unsubscribe(&self, topic: &str) {
        self.msgbus.unsubscribe(topic, self.id);
    }

    fn publish_state_changed(&self) {
        let event = ComponentStateChanged {
            component_id: self.id,
            component_name: self.name.to_string(),
            state: self.fsm.current(),
        };
        let topic = format!("events.system.{}.{}", self.name, self.id);
        self.msgbus.publish(&topic, &event);
    }
}

// =============================================================================
// SECTION 9: Actor
// =============================================================================

/// An Actor is a Component with a trader_id and typed message processing.
///
/// Actors are the primary building block for live/paper trading components:
/// strategies, data clients, execution clients, risk engines, etc.
///
/// The Actor trait allows subclasses to handle specific message types without
/// dynamic dispatch at the call site.
///
/// Note: `Actor` only requires `Send` (not `Sync`) because `MessageBus` uses
/// `RefCell` for interior mutability and is not thread-safe. The backtest engine
/// is single-threaded so this is acceptable. For multi-threaded use, replace
/// `RefCell` with `RwLock` in `MessageBus`.
pub trait Actor {
    /// Return the actor's component.
    fn component(&self) -> &Component;

    /// Return the actor's trader ID.
    fn trader_id(&self) -> &str;

    /// Return the actor's trader ID as an object.
    fn trader_id_obj(&self) -> &TraderId;

    /// Process an incoming message. Subclasses override `handle_message`.
    fn process_message(&mut self, _msg: &dyn Message) {
        // Default: log unknown messages
        let comp = self.component();
        comp.logger.debug("Actor received unknown message type");
    }

    /// Called when the actor is starting.
    fn on_start(&mut self) {}

    /// Called when the actor has stopped.
    fn on_stop(&mut self) {}

    /// Called when the actor's state changes.
    fn on_state_change(&mut self, _state: ComponentState) {}

    // === Lifecycle handlers ===

    /// Called when the actor state is saved (snapshot persistence).
    fn on_save(&mut self) -> std::collections::HashMap<String, Vec<u8>> {
        std::collections::HashMap::new()
    }

    /// Called when the actor state is loaded (snapshot restore).
    fn on_load(&mut self, _state: &std::collections::HashMap<String, Vec<u8>>) {}

    /// Called when the actor is resumed (after stop).
    fn on_resume(&mut self) {}

    /// Called when the actor is reset (clears indicators, state).
    fn on_reset(&mut self) {}

    /// Called when the actor is disposed (cleanup resources).
    fn on_dispose(&mut self) {}

    /// Called when the actor degrades (reduced functionality).
    fn on_degrade(&mut self) {}

    /// Called when the actor faults (emergency cleanup).
    fn on_fault(&mut self) {}

    // === Data handlers ===

    /// Called when a trade tick is received.
    fn on_trade_tick(&mut self, _tick: &TradeTick) {}

    /// Called when a quote tick is received.
    fn on_quote_tick(&mut self, _tick: &QuoteTick) {}

    /// Called when a bar is received.
    fn on_bar(&mut self, _bar: &Bar) {}

    /// Called when a full order book snapshot is received.
    fn on_order_book(&mut self, _book: &OrderBook) {}

    /// Called when an instrument definition is received.
    fn on_instrument(&mut self, _instrument: &Instrument) {}

    /// Called when an instrument status changes (trading halt, resume, etc.).
    fn on_instrument_status(&mut self, _status: &InstrumentStatus) {}

    /// Called when an instrument closes.
    fn on_instrument_close(&mut self, _close: &InstrumentClose) {}

    /// Called when a funding rate update is received.
    fn on_funding_rate(&mut self, _rate: &FundingRateUpdate) {}

    /// Called when a mark price update is received.
    fn on_mark_price(&mut self, _mark: &MarkPriceUpdate) {}

    /// Called when an index price update is received.
    fn on_index_price(&mut self, _index: &IndexPriceUpdate) {}

    /// Called when a generic data update is received.
    fn on_data(&mut self, _data: &dyn Any) {}

    // === Order/position event handlers ===

    /// Called when an order is submitted to the venue.
    fn on_order_submitted(&mut self, _event: &OrderSubmitted) {}

    /// Called when an order is accepted by the venue.
    fn on_order_accepted(&mut self, _event: &OrderAccepted) {}

    /// Called when an order is filled (fully or partially).
    fn on_order_filled(&mut self, _event: &OrderFilled) {}

    /// Called when an order is partially filled.
    fn on_order_partially_filled(&mut self, _event: &OrderFilled) {}

    /// Called when an order is cancelled.
    fn on_order_cancelled(&mut self, _event: &OrderCancelled) {}

    /// Called when an order is rejected by the venue.
    fn on_order_rejected(&mut self, _event: &OrderRejected) {}

    /// Called when a position is opened.
    fn on_position_opened(&mut self, _event: &PositionOpened) {}

    /// Called when a position is changed (size or entry price).
    fn on_position_changed(&mut self, _event: &PositionChanged) {}

    /// Called when a position is closed.
    fn on_position_closed(&mut self, _event: &PositionClosed) {}

    /// Called when any event is received (catch-all).
    fn on_event(&mut self, _event: &dyn Any) {}

    /// Called when a signal is received.
    fn on_signal(&mut self, _signal: &SignalData) {}
}

/// A concrete Actor implementation with a boxed inner type.
pub struct GenericActor {
    component: Component,
    trader_id: TraderId,
    trader_id_str: String,
}

impl GenericActor {
    pub fn new(
        trader_id: TraderId,
        id: u64,
        name: &'static str,
        clock: Box<dyn Clock>,
        msgbus: MessageBus,
    ) -> Self {
        let trader_id_str = trader_id.to_string();
        let logger = Logger::new(name);
        let component = Component::new(id, name, clock, msgbus, logger);
        Self {
            component,
            trader_id,
            trader_id_str,
        }
    }

    pub fn component(&self) -> &Component {
        &self.component
    }

    pub fn component_mut(&mut self) -> &mut Component {
        &mut self.component
    }

    pub fn trader_id(&self) -> &str {
        &self.trader_id_str
    }

    pub fn trader_id_obj(&self) -> &TraderId {
        &self.trader_id
    }
}

impl Actor for GenericActor {
    fn component(&self) -> &Component {
        &self.component
    }

    fn trader_id(&self) -> &str {
        &self.trader_id_str
    }

    fn trader_id_obj(&self) -> &TraderId {
        &self.trader_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instrument::InstrumentId;

    // --- Clock tests ---

    // --- Clock tests ---

    #[test]
    fn test_test_clock_set_time() {
        let mut clock = TestClock::new();
        assert_eq!(clock.timestamp_ns(), 0);
        clock.set_time(1_000_000_000);
        assert_eq!(clock.timestamp_ns(), 1_000_000_000);
    }

    #[test]
    fn test_test_clock_timestamp_ms() {
        let mut clock = TestClock::new();
        clock.set_time(1_500_000_000); // 1.5 seconds in ns
        assert_eq!(clock.timestamp_ms(), 1500); // 1_500_000_000 / 1_000_000
    }

    #[test]
    fn test_test_clock_timer_fires() {
        let mut clock = TestClock::new();
        clock.set_time(1000);

        clock.set_timer_anonymous("my_timer", 1500);

        // advance_time to 1499 — timer not yet due
        let events = clock.advance_time(1499);
        assert!(events.is_empty());

        // advance_time to 1500 — timer fires
        let events = clock.advance_time(1500);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "my_timer");
        assert_eq!(events[0].timestamp_ns, 1500);
    }

    #[test]
    fn test_test_clock_timer_with_callback() {
        let mut clock = TestClock::new();
        clock.set_time(1000);

        let fired = Rc::new(RefCell::new(Vec::new()));
        let fired_clone = Rc::clone(&fired);

        let mut handler = Box::new(move |e: TimeEvent| {
            fired_clone.borrow_mut().push(e.name.clone());
        });

        clock.set_timer("cb_timer", 2000, handler);

        let events = clock.advance_time(2000);
        assert_eq!(events.len(), 1);
        assert_eq!(*fired.borrow(), vec!["cb_timer"]);
    }

    #[test]
    fn test_test_clock_timer_cancel() {
        let mut clock = TestClock::new();
        clock.set_time(100);
        clock.set_timer_anonymous("timer_1", 500);
        clock.set_timer_anonymous("timer_2", 600);

        clock.cancel_timer("timer_1");

        assert_eq!(clock.timer_names(), vec!["timer_2"]);

        let events = clock.advance_time(1000);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].name, "timer_2");
    }

    #[test]
    fn test_test_clock_cancel_timers() {
        let mut clock = TestClock::new();
        clock.set_time(100);
        clock.set_timer_anonymous("a", 500);
        clock.set_timer_anonymous("b", 600);

        clock.cancel_timers();
        assert!(clock.timer_names().is_empty());
        assert!(clock.advance_time(1000).is_empty());
    }

    #[test]
    fn test_test_clock_next_time_ns() {
        let mut clock = TestClock::new();
        clock.set_time(100);
        clock.set_timer_anonymous("t1", 500);
        clock.set_timer_anonymous("t2", 300);

        assert_eq!(clock.next_time_ns("t1"), 500);
        assert_eq!(clock.next_time_ns("t2"), 300);
        assert_eq!(clock.next_time_ns("nonexistent"), 0);
    }

    #[test]
    fn test_test_clock_deterministic_ordering() {
        let mut clock = TestClock::new();
        clock.set_time(100);

        clock.set_timer_anonymous("z", 500);
        clock.set_timer_anonymous("a", 500);
        clock.set_timer_anonymous("m", 500);

        let events = clock.advance_time(500);
        assert_eq!(events.len(), 3);
        // Sorted by timestamp_ns then name: a, m, z
        assert_eq!(events[0].name, "a");
        assert_eq!(events[1].name, "m");
        assert_eq!(events[2].name, "z");
    }

    #[test]
    fn test_test_clock_repeating_timer() {
        let mut clock = TestClock::new();
        clock.set_time(1000);

        let count = Rc::new(RefCell::new(0u32));
        let count_clone = Rc::clone(&count);
        let mut handler = Box::new(move |_: TimeEvent| {
            *count_clone.borrow_mut() += 1;
        });

        // Repeating every 1000ns, starting at 1000
        clock.set_timer_repeating("repeater", 1000, 1000, None, handler, false);

        // First fire at 2000
        clock.advance_time(2000);
        assert_eq!(*count.borrow(), 1);

        // Second fire at 3000
        clock.advance_time(3000);
        assert_eq!(*count.borrow(), 2);

        // Third fire at 4000
        clock.advance_time(4000);
        assert_eq!(*count.borrow(), 3);

        // Cancel
        clock.cancel_timer("repeater");
        clock.advance_time(5000);
        assert_eq!(*count.borrow(), 3); // No more fires
    }

    #[test]
    fn test_test_clock_repeating_with_stop() {
        let mut clock = TestClock::new();
        clock.set_time(1000);

        let count = Rc::new(RefCell::new(0u32));
        let count_clone = Rc::clone(&count);
        let mut handler = Box::new(move |_: TimeEvent| {
            *count_clone.borrow_mut() += 1;
        });

        // Repeating every 1000ns, starting at 1000, stops at 3500
        clock.set_timer_repeating("bounded", 1000, 1000, Some(3500), handler, false);

        clock.advance_time(2000); // fires at 2000
        clock.advance_time(3000); // fires at 3000
        clock.advance_time(4000); // stop_ns exceeded, no more fires

        assert_eq!(*count.borrow(), 2);
        assert!(clock.timer_names().is_empty());
    }

    #[test]
    fn test_test_clock_fire_immediately() {
        let mut clock = TestClock::new();
        clock.set_time(1000);

        let count = Rc::new(RefCell::new(0u32));
        let count_clone = Rc::clone(&count);
        let mut handler = Box::new(move |_: TimeEvent| {
            *count_clone.borrow_mut() += 1;
        });

        // fire_immediately=true → fires at 1000 (start_ns), then 2000, 3000...
        clock.set_timer_repeating("immediate", 1000, 1000, None, handler, true);

        clock.advance_time(1000);
        assert_eq!(*count.borrow(), 1);

        clock.advance_time(2000);
        assert_eq!(*count.borrow(), 2);
    }

    #[test]
    fn test_test_clock_default_handler() {
        let mut clock = TestClock::new();
        clock.set_time(1000);

        let received = Rc::new(RefCell::new(Vec::new()));
        let received_clone = Rc::clone(&received);
        let mut default = Box::new(move |e: TimeEvent| {
            received_clone.borrow_mut().push(e.name.clone());
        });
        clock.register_default_handler(default);

        // Timer without specific handler → default handler fires
        clock.set_timer_anonymous("default_test", 2000);

        clock.advance_time(2000);
        assert_eq!(*received.borrow(), vec!["default_test"]);
    }

    #[test]
    fn test_test_clock_timer_names() {
        let mut clock = TestClock::new();
        clock.set_time(100);
        assert!(clock.timer_names().is_empty());

        clock.set_timer_anonymous("zebra", 500);
        clock.set_timer_anonymous("apple", 300);

        let names = clock.timer_names();
        assert_eq!(names, vec!["apple", "zebra"]);
    }

    #[test]
    fn test_test_clock_timer_count() {
        let mut clock = TestClock::new();
        clock.set_time(100);
        assert_eq!(clock.timer_count(), 0);

        clock.set_timer_anonymous("a", 500);
        clock.set_timer_anonymous("b", 600);
        assert_eq!(clock.timer_count(), 2);

        clock.cancel_timer("a");
        assert_eq!(clock.timer_count(), 1);
    }

    #[test]
    fn test_system_clock_timestamp() {
        let clock = SystemClock::new();
        let ts = clock.timestamp_ns();
        assert!(ts > 0);
        assert_eq!(ts / 1_000_000, clock.timestamp_ms());
    }

    #[test]
    fn test_system_clock_no_timers() {
        let mut clock = SystemClock::new();
        assert!(clock.timer_names().is_empty());
        assert_eq!(clock.timer_count(), 0);
        clock.set_timer_anonymous("any", 1_000_000);
        assert!(clock.timer_names().is_empty()); // No-op, no timers tracked
        assert_eq!(clock.advance_time(1_000_000).len(), 0);
    }

    #[test]
    fn test_time_event_with_name_and_payload() {
        let event = TimeEvent {
            id: 42,
            name: "my_timer".to_string(),
            payload: vec![1, 2, 3],
            timestamp_ns: 1_000_000,
        };
        assert_eq!(event.id, 42);
        assert_eq!(event.name, "my_timer");
        assert_eq!(event.payload, vec![1, 2, 3]);
        assert_eq!(event.timestamp_ns, 1_000_000);
    }

    // --- FSM tests ---

    #[test]
    fn test_fsm_initial_state() {
        let fsm = FiniteStateMachine::new(ComponentState::PreInitialized, COMPONENT_TRANSITIONS);
        assert_eq!(fsm.current(), ComponentState::PreInitialized);
    }

    #[test]
    fn test_fsm_initialize() {
        let mut fsm = FiniteStateMachine::new(ComponentState::PreInitialized, COMPONENT_TRANSITIONS);
        assert!(fsm.trigger(ComponentTrigger::Initialize));
        assert_eq!(fsm.current(), ComponentState::Initialized);
    }

    #[test]
    fn test_fsm_start_sequence() {
        let mut fsm = FiniteStateMachine::new(ComponentState::Initialized, COMPONENT_TRANSITIONS);
        assert!(fsm.trigger(ComponentTrigger::Start));
        assert_eq!(fsm.current(), ComponentState::Starting);
        assert!(fsm.trigger(ComponentTrigger::StartCompleted));
        assert_eq!(fsm.current(), ComponentState::Running);
    }

    #[test]
    fn test_fsm_stop_sequence() {
        let mut fsm = FiniteStateMachine::new(ComponentState::Running, COMPONENT_TRANSITIONS);
        assert!(fsm.trigger(ComponentTrigger::Stop));
        assert_eq!(fsm.current(), ComponentState::Stopping);
        assert!(fsm.trigger(ComponentTrigger::StopCompleted));
        assert_eq!(fsm.current(), ComponentState::Stopped);
    }

    #[test]
    fn test_fsm_invalid_trigger_no_change() {
        let mut fsm = FiniteStateMachine::new(ComponentState::PreInitialized, COMPONENT_TRANSITIONS);
        // Can't Stop from PreInitialized
        assert!(!fsm.trigger(ComponentTrigger::Stop));
        assert_eq!(fsm.current(), ComponentState::PreInitialized);
    }

    #[test]
    fn test_fsm_dispose_sequence() {
        let mut fsm = FiniteStateMachine::new(ComponentState::Ready, COMPONENT_TRANSITIONS);
        assert!(fsm.trigger(ComponentTrigger::Dispose));
        assert_eq!(fsm.current(), ComponentState::Disposing);
        assert!(fsm.trigger(ComponentTrigger::DisposeCompleted));
        assert_eq!(fsm.current(), ComponentState::Disposed);
    }

    #[test]
    fn test_fsm_fault_sequence() {
        let mut fsm = FiniteStateMachine::new(ComponentState::Running, COMPONENT_TRANSITIONS);
        assert!(fsm.trigger(ComponentTrigger::Fault));
        assert_eq!(fsm.current(), ComponentState::Faulting);
        assert!(fsm.trigger(ComponentTrigger::FaultCompleted));
        assert_eq!(fsm.current(), ComponentState::Faulted);
    }

    #[test]
    fn test_fsm_degrade_resume() {
        let mut fsm = FiniteStateMachine::new(ComponentState::Running, COMPONENT_TRANSITIONS);
        assert!(fsm.trigger(ComponentTrigger::Degrade));
        assert_eq!(fsm.current(), ComponentState::Degrading);
        assert!(fsm.trigger(ComponentTrigger::DegradeCompleted));
        assert_eq!(fsm.current(), ComponentState::Degraded);

        // Resume: Degraded → Resuming → Ready
        assert!(fsm.trigger(ComponentTrigger::Resume));
        assert_eq!(fsm.current(), ComponentState::Resuming);
        assert!(fsm.trigger(ComponentTrigger::ResumeCompleted));
        assert_eq!(fsm.current(), ComponentState::Ready);
    }

    // --- MessageBus tests ---

    #[test]
    fn test_msgbus_publish_subscribe() {
        let bus = MessageBus::new();
        let received = Rc::new(RefCell::new(Vec::new()));
        let received_clone = Rc::clone(&received);

        bus.subscribe(
            "test.topic",
            1,
            Box::new(move |msg| {
                if let Some(s) = downcast::<ShutdownSystem>(msg) {
                    received_clone.borrow_mut().push(s.reason.clone());
                }
            }),
            0,
        );

        bus.publish("test.topic", &ShutdownSystem { reason: "hello".to_string() });

        assert_eq!(received.borrow().as_slice(), &["hello".to_string()]);
    }

    #[test]
    fn test_msgbus_wildcard_star() {
        let bus = MessageBus::new();
        let received = Rc::new(RefCell::new(Vec::new()));
        let received_clone = Rc::clone(&received);

        bus.subscribe(
            "events.system.*",
            1,
            Box::new(move |msg| {
                if let Some(c) = downcast::<ComponentStateChanged>(msg) {
                    received_clone.borrow_mut().push(c.state);
                }
            }),
            0,
        );

        bus.publish(
            "events.system.mycomponent",
            &ComponentStateChanged {
                component_id: 0,
                component_name: "mycomponent".to_string(),
                state: ComponentState::Running,
            },
        );

        assert_eq!(received.borrow().len(), 1);
        assert_eq!(received.borrow()[0], ComponentState::Running);
    }

    #[test]
    fn test_msgbus_wildcard_question_mark() {
        let bus = MessageBus::new();
        let received = Rc::new(RefCell::new(0u32));
        let received_clone = Rc::clone(&received);

        bus.subscribe(
            "events.?.stopped",
            1,
            Box::new(move |_| {
                *received_clone.borrow_mut() += 1;
            }),
            0,
        );

        bus.publish("events.a.stopped", &ShutdownSystem { reason: "".to_string() });
        bus.publish("events.b.stopped", &ShutdownSystem { reason: "".to_string() });
        // Should NOT match
        bus.publish("events.ab.stopped", &ShutdownSystem { reason: "".to_string() });

        assert_eq!(*received.borrow(), 2);
    }

    #[test]
    fn test_msgbus_priority_ordering() {
        let bus = MessageBus::new();
        let order = Rc::new(RefCell::new(Vec::new()));
        let order_clone1 = Rc::clone(&order);
        let order_clone2 = Rc::clone(&order);

        // Higher priority (10) should receive first
        bus.subscribe(
            "prio",
            1,
            Box::new(move |_| {
                order_clone1.borrow_mut().push(1);
            }),
            10,
        );
        bus.subscribe(
            "prio",
            2,
            Box::new(move |_| {
                order_clone2.borrow_mut().push(2);
            }),
            0,
        );

        bus.publish("prio", &ShutdownSystem { reason: "".to_string() });

        // Order should be [1, 2] (priority 10 then priority 0)
        assert_eq!(*order.borrow(), vec![1, 2]);
    }

    #[test]
    fn test_msgbus_endpoint_send() {
        let bus = MessageBus::new();
        let received = Rc::new(RefCell::new(String::new()));
        let received_clone = Rc::clone(&received);

        bus.register(
            "actor.123",
            Box::new(move |msg| {
                if let Some(s) = downcast::<ShutdownSystem>(msg) {
                    *received_clone.borrow_mut() = s.reason.clone();
                }
            }),
        );

        bus.send("actor.123", &ShutdownSystem { reason: "direct".to_string() });

        assert_eq!(&*received.borrow(), "direct");
    }

    #[test]
    fn test_msgbus_unsubscribe() {
        let bus = MessageBus::new();
        let count = Rc::new(RefCell::new(0u32));
        let count_clone = Rc::clone(&count);

        bus.subscribe(
            "topic",
            5,
            Box::new(move |_| {
                *count_clone.borrow_mut() += 1;
            }),
            0,
        );

        bus.publish("topic", &ShutdownSystem { reason: "".to_string() });
        assert_eq!(*count.borrow(), 1);

        bus.unsubscribe("topic", 5);

        bus.publish("topic", &ShutdownSystem { reason: "".to_string() });
        assert_eq!(*count.borrow(), 1); // Still 1, not 2
    }

    #[test]
    fn test_is_matching_glob() {
        assert!(is_matching("events.*", "events.system"));
        assert!(is_matching("events.system.*", "events.system.component"));
        assert!(is_matching("events.?.*", "events.a.component"));
        assert!(!is_matching("events.?.*", "events.ab.component"));
        assert!(is_matching("*", "anything"));
        assert!(is_matching("?", "a"));
        assert!(!is_matching("?", "ab"));
        assert!(is_matching("a?b", "aab"));
        assert!(!is_matching("a?b", "ab"));
        assert!(!is_matching("a?b", "aabb"));
    }

    // --- Component tests ---

    #[test]
    fn test_component_lifecycle() {
        let bus = MessageBus::new();
        let clock = Box::new(TestClock::new());
        let logger = Logger::new("test_component");
        let mut comp = Component::new(1, "TestComponent", clock, bus.clone(), logger);

        assert_eq!(comp.state(), ComponentState::PreInitialized);

        comp.initialize();
        assert_eq!(comp.state(), ComponentState::Initialized);

        comp.start();
        assert_eq!(comp.state(), ComponentState::Running);

        comp.stop();
        assert_eq!(comp.state(), ComponentState::Stopped);
    }

    #[test]
    fn test_component_subscribe_and_receive() {
        let bus = MessageBus::new();
        let clock = Box::new(TestClock::new());
        let logger = Logger::new("subscriber");
        let comp = Component::new(10, "Subscriber", clock, bus.clone(), logger);

        let received = Rc::new(RefCell::new(false));
        let received_clone = Rc::clone(&received);

        comp.subscribe(
            "test.events",
            Box::new(move |msg| {
                if downcast::<ComponentStateChanged>(msg).is_some() {
                    *received_clone.borrow_mut() = true;
                }
            }),
            0,
        );

        bus.publish("test.events", &ComponentStateChanged {
            component_id: 99,
            component_name: "publisher".to_string(),
            state: ComponentState::Running,
        });

        assert!(*received.borrow());
    }

    // --- Actor tests ---

    #[test]
    fn test_actor_message_passing() {
        // This is the primary integration test: Actor A receives Actor B's
        // state change event via MessageBus.
        let bus = MessageBus::new();
        let mut clock_a = TestClock::new();
        let mut clock_b = TestClock::new();

        // Actor A (receiver) subscribes to Actor B's state topic
        let actor_a = GenericActor::new(
            TraderId::new("trader1"),
            1,
            "ActorA",
            Box::new(clock_a),
            bus.clone(),
        );

        let received_state = Rc::new(RefCell::new(None::<ComponentState>));
        let received_state_clone = Rc::clone(&received_state);

        actor_a.component().subscribe(
            "events.system.ActorB.*",
            Box::new(move |msg| {
                if let Some(c) = downcast::<ComponentStateChanged>(msg) {
                    *received_state_clone.borrow_mut() = Some(c.state);
                }
            }),
            0,
        );

        // Actor B (sender) publishes its state changes
        let mut actor_b = GenericActor::new(
            TraderId::new("trader2"),
            2,
            "ActorB",
            Box::new(clock_b),
            bus.clone(),
        );

        actor_b.component_mut().initialize();
        actor_b.component_mut().start();

        // Advance clock so events propagate
        let topic = format!("events.system.ActorB.{}", actor_b.component().id);
        bus.publish(
            &topic,
            &ComponentStateChanged {
                component_id: actor_b.component().id,
                component_name: "ActorB".to_string(),
                state: ComponentState::Running,
            },
        );

        assert_eq!(
            *received_state.borrow(),
            Some(ComponentState::Running),
            "Actor A should have received Actor B's Running state"
        );
    }

    #[test]
    fn test_component_state_changed_event_published() {
        let bus = MessageBus::new();
        let clock = Box::new(TestClock::new());
        let logger = Logger::new("state_tester");
        let mut comp = Component::new(42, "StateTester", clock, bus.clone(), logger);

        let received_events = Rc::new(RefCell::new(Vec::new()));
        let received_events_clone = Rc::clone(&received_events);

        bus.subscribe(
            "events.system.StateTester.42",
            99,
            Box::new(move |msg| {
                if let Some(c) = downcast::<ComponentStateChanged>(msg) {
                    received_events_clone.borrow_mut().push(c.state);
                }
            }),
            0,
        );

        comp.initialize();
        comp.start();
        comp.stop();

        let states: Vec<_> = received_events.borrow().clone().into_iter().collect();
        assert!(states.contains(&&ComponentState::Initialized));
        assert!(states.contains(&&ComponentState::Starting));
        assert!(states.contains(&&ComponentState::Running));
        assert!(states.contains(&&ComponentState::Stopping));
        assert!(states.contains(&&ComponentState::Stopped));
    }

    #[test]
    fn test_actor_all_handlers_compile() {
        // Verify all on_* handlers are callable on GenericActor without panic.
        let trader_id = TraderId::new("TESTER-001");
        let mut actor = GenericActor::new(
            trader_id,
            1,
            "test-actor",
            Box::new(TestClock::new()),
            MessageBus::new(),
        );

        // Lifecycle handlers
        let _ = actor.on_save();
        actor.on_load(&HashMap::new());
        actor.on_resume();
        actor.on_reset();
        actor.on_dispose();
        actor.on_degrade();
        actor.on_fault();

        // Data handlers — exercise on_* handlers with concrete event types.
        // Stub types (TradeTick, QuoteTick, Bar, OrderBook, Instrument) have no fields
        // so we call them with a marker; real implementations will construct real data.
        let _status = InstrumentStatus {
            instrument_id: InstrumentId::new("BTCUSDT", "BINANCE"),
            status: "OPEN".to_string(),
            ts_event: 0,
        };
        actor.on_instrument_status(&_status);
        let _close = InstrumentClose {
            instrument_id: InstrumentId::new("BTCUSDT", "BINANCE"),
            close_price: 100.0,
            ts_event: 0,
        };
        actor.on_instrument_close(&_close);
        let _funding = FundingRateUpdate {
            instrument_id: InstrumentId::new("BTCUSDT", "BINANCE"),
            rate: 0.0001,
            ts_event: 0,
        };
        actor.on_funding_rate(&_funding);
        let _mark = MarkPriceUpdate {
            instrument_id: InstrumentId::new("BTCUSDT", "BINANCE"),
            mark_price: 100.0,
            ts_event: 0,
        };
        actor.on_mark_price(&_mark);
        let _index = IndexPriceUpdate {
            instrument_id: InstrumentId::new("BTCUSDT", "BINANCE"),
            index_price: 99.0,
            ts_event: 0,
        };
        actor.on_index_price(&_index);
    }
}
