//! Live execution module — REST/WebSocket adapters and ExecutionClient.
//!
//! This module provides the live execution pipeline:
//! Strategy -> MsgBus SubmitOrder -> ExecutionClient -> Binance REST/WS -> Fill -> Cache

pub mod exchange;
pub mod execution_client;
pub mod http_adapter;
pub mod normalizer;
pub mod ws_adapter;
pub mod bybit_http_adapter;
pub mod bybit_ws_adapter;
pub mod bybit_market_adapter;
pub mod okx_http_adapter;
pub mod okx_ws_adapter;
pub mod okx_market_adapter;
pub mod binance_market_adapter;
pub mod matching_core;

pub use exchange::{
    AccountInfoResponse, AssetBalance, BinanceExecutionReport, BybitExecutionReport,
    Exchange, ExchangeError, ExchangeType, ExchangeWs, MarketDataAdapter, MarketDataMessage,
    NormalizedTrade, OkxExecutionReport, WsError, WsMessage,
};
pub use execution_client::ExecutionClient;
pub use http_adapter::{BinanceApiError, BinanceHttpAdapter};
pub use normalizer::{BinanceSymbolNormalizer, BybitSymbolNormalizer, OkxSymbolNormalizer, SymbolNormalizer};
pub use ws_adapter::{BinanceWsAdapter, WsMessage as BinanceWsMessage, ExecutionReport};
pub use binance_market_adapter::BinanceMarketDataAdapter;
pub use crate::ingestion::adapters::binance::BinanceVenue;
