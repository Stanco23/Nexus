//! Minimal TVCB debug test
use std::path::Path;
use tvc::tvcb::{Bar, TvcbWriter, TvcbReader, AnchorBar, anchor_bar_to_bytes, ANCHOR_BAR_SIZE};

fn main() {
    let path = "/tmp/test_tvcb_debug.tvcb";
    let precision = 5u8;
    let timeframe_ns = 15 * 60 * 1_000_000_000u64;
    let _ = std::fs::remove_file(path);

    let bar1 = Bar::from_floats(
        1704067200000000000, 42283.58, 42488.09, 42261.02, 42488.00, 42488.00,
        431.7108, 259.02648, 172.68432, 12345, precision,
    );
    let bar2 = Bar::from_floats(
        1704068100000000000, 42488.00, 42554.57, 42412.02, 42419.73, 42419.73,
        392.2489, 235.34934, 156.89956, 12350, precision,
    );
    let bar3 = Bar::from_floats(
        1704069000000000000, 42419.73, 42554.57, 42354.19, 42441.32, 42441.32,
        319.9064, 191.94384, 127.96256, 12355, precision,
    );

    println!("Bar 1: ts={}, close={}", bar1.ts_event, bar1.close);
    println!("Bar 2: ts={}, close={}", bar2.ts_event, bar2.close);
    println!("Bar 3: ts={}, close={}", bar3.ts_event, bar3.close);
    println!("ts diff 1->2: {} sec", (bar2.ts_event - bar1.ts_event) / 1_000_000_000);
    println!("ts diff 2->3: {} sec", (bar3.ts_event - bar2.ts_event) / 1_000_000_000);

    let instrument_hash = 0u32;
    let anchor_interval = 10u32;
    let year = 2024u64;

    let mut writer = TvcbWriter::new(path, instrument_hash, anchor_interval, precision, year, timeframe_ns)
        .map_err(|e| std::io::Error::other(e.to_string())).unwrap();
    
    println!("\nwrite_bar bar 1...");
    writer.write_bar(&bar1).unwrap();
    println!("write_bar bar 2...");
    writer.write_bar(&bar2).unwrap();
    println!("write_bar bar 3...");
    writer.write_bar(&bar3).unwrap();
    
    let digest = writer.finalize().map_err(|e| std::io::Error::other(e.to_string())).unwrap();
    println!("Finalized. Digest: {:02x?}", digest);

    // Read back and decode bar by bar
    let reader = TvcbReader::open(path).map_err(|e| std::io::Error::other(e.to_string())).unwrap();
    println!("\n--- Decoding ---");
    let num_bars = reader.num_bars();
    println!("num_bars: {}", num_bars);
    
    for i in 0..num_bars {
        match reader.decode_bar_at(i) {
            Ok(bar) => println!("bar {}: ts={}, close={}", i, bar.ts_event, bar.close),
            Err(e) => println!("bar {} ERROR: {:?}", i, e),
        }
    }
    
    std::fs::remove_file(path).ok();
}
