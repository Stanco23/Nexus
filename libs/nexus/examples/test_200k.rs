use std::path::PathBuf;
use crate::buffer::ring_buffer::RingBuffer;
use crate::instrument::InstrumentId;

fn main() {
    let path = PathBuf::from("/home/shadowarch/Nexus/data/BTCUSDT_2025-01-02.tvc");
    let instrument = InstrumentId::new("BTCUSDT", "BINANCE");
    
    let rb = RingBuffer::open(&path, instrument).expect("failed to open");
    println!("num_ticks={}, anchor_interval={}, num_anchors={}", 
             rb.num_ticks(), rb.anchor_interval(), rb.num_anchors());
    
    let mut iter = rb.iter();
    let mut count = 0;
    let mut last_seq: u32 = 0;
    let mut last_ts = 0u64;
    
    // Iterate 200k ticks and check for gaps
    let mut gap_count = 0;
    while let Some(tick) = iter.next() {
        if count > 0 && tick.sequence != last_seq + 1 {
            println!("GAP at {}: seq {} -> {}, ts={}", count, last_seq, tick.sequence, tick.timestamp_ns);
            gap_count += 1;
        }
        if tick.timestamp_ns < last_ts {
            println!("TS REGRESS at {}: seq={}, ts={} < {}", count, tick.sequence, tick.timestamp_ns, last_ts);
        }
        last_seq = tick.sequence;
        last_ts = tick.timestamp_ns;
        count += 1;
        if count % 100000 == 0 {
            println!("at {} ticks, current_offset={}", count, 0); // can't access private
        }
        if count >= 500000 { break; }
    }
    println!("iterated {} ticks, gap_count={}, last_seq={}", count, gap_count, last_seq);
}
