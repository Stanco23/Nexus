//! Example strategy implementations.

pub mod ema_cross;
pub mod rsi_strategy;
pub mod sma_cross_trailing;
pub mod trend_following;
pub mod vwap_momentum;

pub use ema_cross::EmaCrossStrategy;
pub use rsi_strategy::RsiStrategy;
pub use sma_cross_trailing::SmaCrossTrailingStrategy;
pub use trend_following::TrendFollowingStrategy;
pub use vwap_momentum::VwapMomentumStrategy;
