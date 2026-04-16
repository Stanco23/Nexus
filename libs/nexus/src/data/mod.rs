//! Data Engine — subscription management and routing for live data.
//!
//! Unlike Nautilus (which decodes raw WebSocket data), Nexus DataEngine
//! reads from pre-decoded ring buffers. Decoding happens once at ingestion
//! (WebSocket adapter → RingBuffer), then DataEngine fans out to subscribers.
//!
//! Key design:
//! - Subscribe/Unsubscribe via MessageBus commands
//! - Clock-driven iteration through RingBuffer (live mode)
//! - Fan-out to multiple subscribed actors

pub mod messages;

mod engine;

pub use engine::DataEngine;