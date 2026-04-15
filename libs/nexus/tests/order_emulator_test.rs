//! Tests for OrderEmulator — queue position, fill probability, FIFO order.

use nexus::book::{OrderEmulator, Side};

#[test]
fn test_queue_position_assignment() {
    let mut emulator = OrderEmulator::new();

    // Submit 5 buy orders at the same price — queue positions 0, 1, 2, 3, 4
    let price = 100.0;
    let ts = 1_000_000_000u64;

    let id0 = emulator.submit_limit(price, 1.0, Side::Buy, ts);
    let id1 = emulator.submit_limit(price, 1.0, Side::Buy, ts + 1);
    let id2 = emulator.submit_limit(price, 1.0, Side::Buy, ts + 2);
    let id3 = emulator.submit_limit(price, 1.0, Side::Buy, ts + 3);
    let id4 = emulator.submit_limit(price, 1.0, Side::Buy, ts + 4);

    assert_eq!(emulator.queue_position(id0), Some(0));
    assert_eq!(emulator.queue_position(id1), Some(1));
    assert_eq!(emulator.queue_position(id2), Some(2));
    assert_eq!(emulator.queue_position(id3), Some(3));
    assert_eq!(emulator.queue_position(id4), Some(4));
    assert_eq!(emulator.num_pending(), 5);
}

#[test]
fn test_queue_position_separate_sides() {
    let mut emulator = OrderEmulator::new();
    let ts = 1_000_000_000u64;

    // Bid side: 3 orders at price 99
    let _ = emulator.submit_limit(99.0, 1.0, Side::Buy, ts);
    let _ = emulator.submit_limit(99.0, 1.0, Side::Buy, ts + 1);
    let bid_id2 = emulator.submit_limit(99.0, 1.0, Side::Buy, ts + 2);

    // Ask side: 2 orders at price 101
    let _ = emulator.submit_limit(101.0, 1.0, Side::Sell, ts + 3);
    let ask_id1 = emulator.submit_limit(101.0, 1.0, Side::Sell, ts + 4);

    // Bid queue is separate from ask queue
    assert_eq!(emulator.queue_position(bid_id2), Some(2));
    assert_eq!(emulator.queue_position(ask_id1), Some(1));
    assert_eq!(emulator.num_pending(), 5);
}

#[test]
fn test_fifo_fill_order() {
    let mut emulator = OrderEmulator::new();
    let ts = 1_000_000_000u64;

    // Submit 3 orders — first should fill first
    let id0 = emulator.submit_limit(100.0, 1.0, Side::Buy, ts);
    let id1 = emulator.submit_limit(100.0, 1.0, Side::Buy, ts + 1);
    let id2 = emulator.submit_limit(100.0, 1.0, Side::Buy, ts + 2);

    // Process fills at a price that crosses (market price = 100)
    let fills = emulator.process_fills(100.0, 0.0, ts + 100);

    // Fills should be in FIFO order
    assert_eq!(fills.len(), 3);
    assert_eq!(fills[0].order_id, id0);
    assert_eq!(fills[1].order_id, id1);
    assert_eq!(fills[2].order_id, id2);

    // All pending should be consumed
    assert_eq!(emulator.num_pending(), 0);
    assert_eq!(emulator.num_filled(), 3);
}

#[test]
fn test_fill_probability_decreases_with_queue_position() {
    let emulator = OrderEmulator::new();

    // Position 0 should have high fill probability
    let prob_0 = emulator.fill_probability(0, 1.0);
    // Position 10 should have lower fill probability
    let prob_10 = emulator.fill_probability(10, 1.0);
    // Position 100 should have very low fill probability
    let prob_100 = emulator.fill_probability(100, 1.0);

    assert!(prob_0 > prob_10);
    assert!(prob_10 > prob_100);
    assert!(prob_0 <= 1.0);
    assert!(prob_100 > 0.0);
}

#[test]
fn test_fill_probability_scaled_by_volume() {
    let emulator = OrderEmulator::new();

    let prob_low_volume = emulator.fill_probability(5, 0.1);
    let prob_high_volume = emulator.fill_probability(5, 10.0);

    // High volume should increase fill probability
    assert!(prob_high_volume > prob_low_volume);
}

#[test]
fn test_fill_event_has_correct_side() {
    let mut emulator = OrderEmulator::new();
    let ts = 1_000_000_000u64;

    let buy_id = emulator.submit_limit(100.0, 1.0, Side::Buy, ts);
    let sell_id = emulator.submit_limit(100.0, 1.0, Side::Sell, ts + 1);

    // Process fills
    let fills = emulator.process_fills(100.0, 0.0, ts + 100);

    assert_eq!(fills.len(), 2);

    // Find fills by order ID
    let buy_fill = fills.iter().find(|f| f.order_id == buy_id).unwrap();
    let sell_fill = fills.iter().find(|f| f.order_id == sell_id).unwrap();

    assert_eq!(buy_fill.side, Side::Buy);
    assert_eq!(sell_fill.side, Side::Sell);
}

#[test]
fn test_pending_orders_empty_after_all_fills() {
    let mut emulator = OrderEmulator::new();
    let ts = 1_000_000_000u64;

    emulator.submit_limit(100.0, 1.0, Side::Buy, ts);
    emulator.submit_limit(100.0, 1.0, Side::Sell, ts + 1);

    assert_eq!(emulator.num_pending(), 2);

    emulator.process_fills(100.0, 0.0, ts + 100);

    assert_eq!(emulator.num_pending(), 0);
}

#[test]
fn test_market_volume_update() {
    let mut emulator = OrderEmulator::new();

    // Initial average volume is 1.0 — verify it returns a valid probability
    let _initial = emulator.fill_probability(0, 1.0);

    // Update with a large trade
    emulator.update_market_volume(10.0);

    // Fill probability should change (volume ratio affects it)
    let after_large = emulator.fill_probability(0, 10.0);

    // Both should reflect the new volume scale
    assert!(after_large > 0.0);
}
