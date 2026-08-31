use std::path::Path;
use tempfile::TempDir;
use nexus::buffer::RingBuffer;
use nexus::instrument::InstrumentId;
use tvc::{TradeTick, TvcWriter};

fn main() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("test.tvc");
    let mut w = TvcWriter::new(&path, 0xDEAD, 1024, 9).unwrap();
    for i in 0..20u64 {
        w.write_tick(&TradeTick {
            timestamp_ns: 1_735_776_000_000_000_000 + i * 1_000_000,
            price_int: 94_500_000_000_000 + i * 50_000,
            size_int: 1_000_000 + i * 100,
            side: (i % 2) as u8,
            flags: 1,
            sequence: i as u32,
        }).unwrap();
    }
    w.finalize().unwrap();
    drop(w);

    let rb = RingBuffer::open(&path, InstrumentId::new("BTCUSDT", "BINANCE")).unwrap();
    for (i, t) in rb.iter().enumerate() {
        let expected = 1_000_000 + i as i64 * 100;
        let status = if t.size_int == expected { "OK" } else { "DRIFT" };
        println!("tick {} size_int = {} (expected {}) {}", i, t.size_int, expected, status);
    }
}
