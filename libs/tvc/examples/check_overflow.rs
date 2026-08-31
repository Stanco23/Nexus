use tvc::TradeTick;
use tvc::compression::pack_delta;
use tvc::compression::PackedDelta;

fn main() {
    let prev = tvc::TradeTick::new(1_700_000_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 0);
    let next = tvc::TradeTick::new(1_700_200_000_000, 100_000_000_000i64, 1_000_000i64, 0, 1, 1);
    eprintln!("ts_delta_ms: {}", (next.timestamp_ns - prev.timestamp_ns) / 1_000_000);
    let packed = pack_delta(&prev, &next);
    match packed {
        PackedDelta::Base(_) => eprintln!("Got base (BUG)"),
        PackedDelta::Overflow(b) => eprintln!("Got overflow: {} bytes", b.len()),
    }
}
