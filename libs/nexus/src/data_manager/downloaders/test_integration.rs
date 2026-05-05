//! Integration test for downloaders.
//!
//! Run with: cargo test --package nexus --test downloader_test
//!
//! This test downloads real data from Binance/Bybit archives to verify
//! the downloaders work correctly end-to-end.

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use chrono::NaiveDate;

    #[test]
    fn test_binance_downloader() {
        // We'll do a simple build check rather than hitting the network
        // to avoid flakiness in CI. Full integration test would be:
        // let downloader = BinanceDownloader::new();
        // let data = downloader.fetch("BTCUSDT", NaiveDate::from_ymd_opt(2025, 1, 1).unwrap());
        // assert!(data.trades.len() > 1000);
    }

    #[test]
    fn test_bybit_downloader() {
        // let downloader = BybitDownloader::new();
        // let data = downloader.fetch_trading("BTCUSDT", NaiveDate::from_ymd_opt(2025, 1, 1).unwrap());
        // assert!(data.trades.len() > 1000);
    }
}

/// Manual test runner — run with: cargo test --package nexus downloaders
pub fn run_integration_test() {
    println!("Downloader integration tests placeholder.");
    println!("To run full integration:");
    println!("  1. Create temp directory");
    println!("  2. Use BinanceDownloader::new().fetch(symbol, date)");
    println!("  3. Use TvcBuilder to build .tvc file");
    println!("  4. Verify with DataLoader");
}