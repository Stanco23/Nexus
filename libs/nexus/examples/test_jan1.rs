use std::path::PathBuf;
use crate::buffer::ring_buffer::RingBuffer;
use crate::instrument::InstrumentId;

fn main() {
    let path = PathBuf::from("/home/shadowarch/Nexus/data/BTCUSDT_2025-01-01.tvc");
    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");
    
    let rb = RingBuffer::open(&path, instrument).expect("failed to open");
    println!("num_ticks={}, anchor_interval={}, num_anchors={}", 
             rb.num_ticks(), rb.anchor_interval(), rb.num_anchors());
    
    let mut iter = rb.iter();
    let mut count = 0;
    let mut last_seq: u32 = 0;
    let mut prev_ts = 0u64;
    while let Some(tick) = iter.next() {
        if count < 5 || tick.sequence != last_seq + 1 || tick.timestamp_ns < prev_ts {
            println!("iter[{}]: seq={}, ts={}, price={}", 
                     count, tick.sequence, tick.timestamp_ns, tick.price(2));
        }
        last_seq = tick.sequence;
        prev_ts = tick.timestamp_ns;
        count += 1;
    }
    println!("iterated {} ticks, last_seq={}", count, last_seq);
    println!("expected num_ticks={}", rb.num_ticks());
}
