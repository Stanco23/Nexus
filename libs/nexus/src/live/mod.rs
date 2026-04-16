//! Live execution module — REST/WebSocket adapters and ExecutionClient.
//!
//! This module provides the live execution pipeline:
//! Strategy -> MsgBus SubmitOrder -> ExecutionClient -> Binance REST/WS -> Fill -> Cache

pub mod execution_client;
pub mod http_adapter;
pub mod ws_adapter;

pub use execution_client::ExecutionClient;
pub use http_adapter::{BinanceApiError, BinanceHttpAdapter};
pub use ws_adapter::{BinanceWsAdapter, WsMessage, ExecutionReport};
