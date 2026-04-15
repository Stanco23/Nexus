//! Tests for OrderBook walk_book and market order multi-level fills.

use nexus::book::{OrderBook, OrderEmulator, Side};

#[test]
fn test_walk_book_buy_walks_asks_ascending() {
    let mut book = OrderBook::new();
    // add_limit_order takes (price, size, side) — no timestamp
    book.add_limit_order(100.0, 0.5, Side::Sell as u8);
    book.add_limit_order(101.0, 0.5, Side::Sell as u8);
    book.add_limit_order(102.0, 1.0, Side::Sell as u8);

    // Buy market order walks asks ascending (lowest first)
    let fills = book.walk_book(0.8, Side::Buy);

    assert_eq!(fills.len(), 2);
    assert_eq!(fills[0].price_int, 100_000_000_000); // $100 in nanos
    assert!((fills[0].size_filled - 0.5).abs() < 1e-10);
    assert!((fills[0].remaining_at_level - 0.0).abs() < 1e-10);

    assert_eq!(fills[1].price_int, 101_000_000_000); // $101 in nanos
    assert!((fills[1].size_filled - 0.3).abs() < 1e-10);
    assert!((fills[1].remaining_at_level - 0.2).abs() < 1e-10);
}

#[test]
fn test_walk_book_sell_walks_bids_descending() {
    let mut book = OrderBook::new();
    book.add_limit_order(100.0, 0.5, Side::Buy as u8);
    book.add_limit_order(99.0, 0.5, Side::Buy as u8);
    book.add_limit_order(98.0, 1.0, Side::Buy as u8);

    // Sell market order walks bids descending (highest first)
    let fills = book.walk_book(0.8, Side::Sell);

    assert_eq!(fills.len(), 2);
    assert_eq!(fills[0].price_int, 100_000_000_000); // $100 in nanos — highest bid first
    assert!((fills[0].size_filled - 0.5).abs() < 1e-10);

    assert_eq!(fills[1].price_int, 99_000_000_000); // $99 in nanos
    assert!((fills[1].size_filled - 0.3).abs() < 1e-10);
    assert!((fills[1].remaining_at_level - 0.2).abs() < 1e-10);
}

#[test]
fn test_walk_book_partial_fill_across_3_levels() {
    let mut book = OrderBook::new();
    book.add_limit_order(100.0, 0.5, Side::Sell as u8); // level 1
    book.add_limit_order(101.0, 0.5, Side::Sell as u8); // level 2
    book.add_limit_order(102.0, 1.0, Side::Sell as u8); // level 3

    // Market BUY for 1.2 — fills across all 3 ask levels
    let fills = book.walk_book(1.2, Side::Buy);

    assert_eq!(fills.len(), 3);
    assert!((fills[0].size_filled - 0.5).abs() < 1e-10);
    assert_eq!(fills[0].remaining_at_level, 0.0);
    assert!((fills[1].size_filled - 0.5).abs() < 1e-10);
    assert_eq!(fills[1].remaining_at_level, 0.0);
    assert!((fills[2].size_filled - 0.2).abs() < 1e-10);
    assert!((fills[2].remaining_at_level - 0.8).abs() < 1e-10);
}

#[test]
fn test_process_market_order_multi_level_with_fees() {
    let mut emulator = OrderEmulator::new();
    let mut book = OrderBook::new();
    book.add_limit_order(100.0, 0.5, Side::Sell as u8);
    book.add_limit_order(101.0, 0.5, Side::Sell as u8);

    // Market buy for 0.8 with 0.001 taker fee
    let fills = emulator.process_market_order(
        0.8,
        Side::Buy,
        &book,
        2_000_000_000,
        0.001,
    );

    assert_eq!(fills.len(), 2);
    // Each fill should have correct size
    assert!((fills[0].fill_size - 0.5).abs() < 1e-10);
    assert!((fills[1].fill_size - 0.3).abs() < 1e-10);
    // Each fill should have fee = size * taker_fee
    assert!((fills[0].fee - 0.5 * 0.001).abs() < 1e-10);
    assert!((fills[1].fee - 0.3 * 0.001).abs() < 1e-10);
    // is_maker should be false for market orders
    assert!(!fills[0].is_maker);
    assert!(!fills[1].is_maker);
    // All fills should have timestamps
    assert_eq!(fills[0].timestamp_ns, 2_000_000_000);
    assert_eq!(fills[1].timestamp_ns, 2_000_000_000);
}

#[test]
fn test_walk_book_exhausts_all_levels() {
    let mut book = OrderBook::new();
    book.add_limit_order(100.0, 0.5, Side::Sell as u8);
    book.add_limit_order(101.0, 0.5, Side::Sell as u8);

    // Large buy (10x available liquidity) — should consume all and return fills
    let fills = book.walk_book(10.0, Side::Buy);

    assert_eq!(fills.len(), 2);
    let total_filled: f64 = fills.iter().map(|f| f.size_filled).sum();
    assert!((total_filled - 1.0).abs() < 1e-10); // only 1.0 total available
}

#[test]
fn test_level_fill_price_int_correct() {
    let mut book = OrderBook::new();
    book.add_limit_order(99.99, 1.0, Side::Sell as u8);

    let fills = book.walk_book(1.0, Side::Buy);
    assert_eq!(fills.len(), 1);
    assert_eq!(fills[0].price_int, 99_990_000_000); // $99.99 in nanos
}
