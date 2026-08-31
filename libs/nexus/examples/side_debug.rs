use std::path::PathBuf;
use nexus::buffer::RingBuffer;
use nexus::instrument::InstrumentId;
use tvc::{TradeTick, TvcWriter};

fn main() {
    let path = PathBuf::from("/tmp/side_debug.tvc");
    let _ = std::fs::remove_file(&path);
    let mut w = TvcWriter::new(&path, 0x1234, 100, 9).unwrap();
    for i in 0..10u64 {
        w.write_tick(&TradeTick {
            timestamp_ns: 1_000_000_000 + i * 1_000_000,
            price_int: 94_500_000_000_000,
            size_int: 1_000_000,
            side: (i % 2) as u8,
            flags: 1,
            sequence: i as u32,
        }).unwrap();
    }
    w.finalize().unwrap();
    drop(w);

    let rb = RingBuffer::open(&path, InstrumentId::new("BTCUSDT", "BINANCE")).unwrap();
    for (i, t) in rb.iter().enumerate() {
        println!("tick {}: side={} flags={} ts={}", i, t.side, t.flags, t.timestamp_ns);
    }
}
