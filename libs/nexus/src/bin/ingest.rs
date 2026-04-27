//! Binance → TVC3 Ingestor CLI
//! ============================
//! Converts Binance trade CSV files to TVC3 format.
//!
//! Supports two CSV formats:
//! 1. **Binance Data Archive** — `id,price,qty,quote_qty,time,is_buyer_maker`
//!    (from data.binance.vision)
//! 2. **Generic trade CSV** — `timestamp,price,quantity,side`
//!    (nanoseconds, BUY/SELL strings)
//!
//! # Usage — Binance Data Archive format
//! ```bash
//! cargo run -p nexus --bin ingest -- \
//!     --exchange binance \
//!     --symbol BTCUSDT \
//!     --input-dir ./csv_data \
//!     --output ./tvc_data
//! ```
//!
//! # Usage — Generic trade CSV (e.g. bench_trades.csv)
//! ```bash
//! cargo run -p nexus --bin ingest -- \
//!     --format generic \
//!     --symbol BTCUSDT \
//!     --input-dir ./csv_data \
//!     --output ./tvc_data
//! ```

use std::path::PathBuf;
use std::time::Instant;

use nexus::ingestion::{
    BinanceFileIngestor, GenericCsvIngestor, IngestError,
};

fn main() {
    let mut args = std::env::args().skip(1);
    let mut exchange = "binance".to_string();
    let mut symbol = "BTCUSDT".to_string();
    let mut format = "binance".to_string();
    let mut input_dir: Option<PathBuf> = None;
    let mut output_dir: PathBuf = PathBuf::from("./tvc_data");
    let mut precision: u8 = 9;
    let mut anchor_interval: u32 = 100;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--exchange" | "-e" => exchange = args.next().unwrap_or(exchange),
            "--symbol" | "-s" => symbol = args.next().unwrap_or(symbol),
            "--format" | "-f" => format = args.next().unwrap_or(format),
            "--input-dir" | "-i" => {
                input_dir = Some(PathBuf::from(args.next().unwrap_or_default()))
            }
            "--output" | "-o" => output_dir = PathBuf::from(args.next().unwrap_or_default()),
            "--precision" | "-p" => {
                precision = args.next().unwrap_or_default().parse().unwrap_or(precision)
            }
            "--anchor" => anchor_interval = args.next().unwrap_or_default().parse().unwrap_or(100),
            _ => {}
        }
    }

    let input_dir = match input_dir {
        Some(d) => d,
        None => {
            eprintln!("ERROR: --input-dir is required");
            std::process::exit(1);
        }
    };

    println!("=== Nexus Ingestor ===");
    println!("Exchange:  {}", exchange);
    println!("Symbol:    {}", symbol);
    println!("Format:    {}", format);
    println!("Input:     {:?}", input_dir);
    println!("Output:    {:?}", output_dir);

    let start = Instant::now();

    match format.as_str() {
        "generic" => {
            let ingestor = GenericCsvIngestor::new(&symbol)
                .with_precision(precision)
                .with_anchor_interval(anchor_interval);

            match ingestor.ingest_directory(&input_dir, &output_dir) {
                Ok(outputs) => {
                    let total = outputs.len();
                    println!(
                        "\n✓ Done — {} files written in {:?}",
                        total,
                        start.elapsed()
                    );
                }
                Err(e) => {
                    eprintln!("ERROR: {}", e);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            // Default: Binance Data Archive format
            let ingestor = BinanceFileIngestor::new(&symbol)
                .with_precision(precision)
                .with_anchor_interval(anchor_interval);

            match ingestor.ingest_directory(&input_dir, &output_dir) {
                Ok(outputs) => {
                    let total = outputs.len();
                    println!(
                        "\n✓ Done — {} files written in {:?}",
                        total,
                        start.elapsed()
                    );
                }
                Err(e) => {
                    eprintln!("ERROR: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}
