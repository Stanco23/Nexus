//! Binance → TVC3 Ingestor CLI
//! ============================
//! Downloads from Binance Data Archive and/or converts local CSV files to TVC3.
//!
//! # Download + ingest from Binance Data Archive
//! ```bash
//! cargo run -p nexus --bin ingest -- \
//!     --symbol BTCUSDT \
//!     --start 2025-01-01 \
//!     --end 2025-01-31 \
//!     --output ./tvc_data
//! ```
//!
//! # Ingest from local CSV directory (Binance Data Archive format)
//! ```bash
//! cargo run -p nexus --bin ingest -- \
//!     --symbol BTCUSDT \
//!     --input-dir ./csv_data \
//!     --output ./tvc_data
//! ```
//!
//! # Ingest from local CSV directory (generic format: timestamp,price,qty,side)
//! ```bash
//! cargo run -p nexus --bin ingest -- \
//!     --symbol BTCUSDT \
//!     --input-dir ./csv_data \
//!     --output ./tvc_data \
//!     --format generic
//! ```

use std::path::PathBuf;
use std::time::Instant;

use nexus::ingestion::{BinanceFileIngestor, GenericCsvIngestor};
use nexus::instrument::fnv1a_hash;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut symbol = "BTCUSDT".to_string();
    let mut format = "binance".to_string();
    let mut input_dir: Option<PathBuf> = None;
    let mut output_dir: PathBuf = PathBuf::from("./tvc_data");
    let mut precision: u8 = 9;
    let mut anchor_interval: u32 = 2;
    let mut start_date: Option<String> = None;
    let mut end_date: Option<String> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
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
            "--start" => start_date = args.next(),
            "--end" => end_date = args.next(),
            _ => {}
        }
    }

    println!("=== Nexus Ingestor ===");
    println!("Symbol:    {}", symbol);
    println!("Format:    {}", format);
    println!("Output:    {:?}", output_dir);

    let start = Instant::now();

    if let (Some(start), Some(end)) = (&start_date, &end_date) {
        println!("Start:     {}", start);
        println!("End:       {}", end);
        download_and_ingest(&symbol, start, end, &output_dir, precision, anchor_interval);
    } else if let Some(ref dir) = input_dir {
        println!("Input:     {:?}", dir);
        ingest_local(&symbol, dir, &output_dir, &format, precision, anchor_interval);
    } else {
        println!("\nUsage:");
        println!("  # Download from Binance Data Archive:");
        println!("  cargo run -p nexus --bin ingest -- \\");
        println!("      --symbol BTCUSDT \\");
        println!("      --start 2025-01-01 --end 2025-01-31 \\");
        println!("      --output ./tvc_data");
        println!();
        println!("  # Ingest local Binance CSVs:");
        println!("  cargo run -p nexus --bin ingest -- \\");
        println!("      --symbol BTCUSDT \\");
        println!("      --input-dir ./csv_data --output ./tvc_data");
        println!();
        println!("  # Ingest generic CSV (timestamp,price,qty,side):");
        println!("  cargo run -p nexus --bin ingest -- \\");
        println!("      --symbol BTCUSDT \\");
        println!("      --input-dir ./csv_data --output ./tvc_data \\");
        println!("      --format generic");
    }

    println!("\nTotal time: {:?}", start.elapsed());
}

/// Ingest local CSV files into TVC3.
fn ingest_local(
    symbol: &str,
    input_dir: &PathBuf,
    output_dir: &PathBuf,
    format: &str,
    precision: u8,
    anchor_interval: u32,
) {
    std::fs::create_dir_all(output_dir).expect("Cannot create output directory");

    match format {
        "generic" => {
            let ingestor = GenericCsvIngestor::new(symbol)
                .with_precision(precision)
                .with_anchor_interval(anchor_interval);

            match ingestor.ingest_directory(input_dir, output_dir) {
                Ok(outputs) => {
                    let total = outputs.len();
                    let ticks: u64 = outputs.len() as u64;
                    println!("\n✓ {} files written", total);
                }
                Err(e) => {
                    eprintln!("ERROR: {}", e);
                    std::process::exit(1);
                }
            }
        }
        _ => {
            let ingestor = BinanceFileIngestor::new(symbol)
                .with_precision(precision)
                .with_anchor_interval(anchor_interval);

            match ingestor.ingest_directory(input_dir, output_dir) {
                Ok(outputs) => {
                    let total = outputs.len();
                    println!("\n✓ {} files written", total);
                }
                Err(e) => {
                    eprintln!("ERROR: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}

/// Download from Binance Data Archive, then ingest.
fn download_and_ingest(
    symbol: &str,
    start_date: &str,
    end_date: &str,
    output_dir: &PathBuf,
    precision: u8,
    anchor_interval: u32,
) {
    use std::io::{Read, Write};

    // Create temp directory for downloaded ZIPs
    let temp_dir = std::env::temp_dir().join("nexus_ingest");
    let csv_dir = temp_dir.join("csv");
    std::fs::create_dir_all(&csv_dir).expect("Cannot create temp directory");

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .expect("HTTP client build failed");

    // Parse dates
    let start = chrono::NaiveDate::parse_from_str(start_date, "%Y-%m-%d")
        .expect("Invalid start date (use YYYY-MM-DD)");
    let end = chrono::NaiveDate::parse_from_str(end_date, "%Y-%m-%d")
        .expect("Invalid end date (use YYYY-MM-DD)");

    println!("\nDownloading from Binance Data Archive...");

    let mut current = start;
    let mut total_ticks: u64 = 0;
    let mut files_written: usize = 0;

    while current <= end {
        let date_str = current.format("%Y-%m-%d").to_string();
        let zip_url = format!(
            "https://data.binance.vision/data/spot/daily/trades/{}/{}-trades-{}.zip",
            symbol, symbol, date_str
        );
        let zip_path = temp_dir.join(format!("{}-trades-{}.zip", symbol, date_str));

        print!("  {} ... ", date_str);
        std::io::stdout().flush().ok();

        // Download if not already present
        if !zip_path.exists() {
            print!("downloading ");
            std::io::stdout().flush().ok();

            let response = match client.get(&zip_url).send() {
                Ok(r) => r,
                Err(e) => {
                    println!("SKIP (network error: {})", e);
                    current += chrono::Duration::days(1);
                    continue;
                }
            };

            if !response.status().is_success() {
                println!("SKIP (HTTP {})", response.status());
                current += chrono::Duration::days(1);
                continue;
            }

            let mut file = std::fs::File::create(&zip_path).expect("Cannot create zip file");
            let mut bytes = response.bytes().expect("Cannot read response body");
            std::io::Write::write_all(&mut file, &mut bytes).expect("Cannot write zip");
        }

        // Extract ZIP
        print!("extracting ");
        std::io::stdout().flush().ok();

        let zip_file = std::fs::File::open(&zip_path).expect("Cannot open zip");
        let mut archive = zip::ZipArchive::new(zip_file).expect("Invalid ZIP");

        // Expected CSV name inside: SYMBOL-trades-DATE.csv
        let csv_name = format!("{}-trades-{}.csv", symbol, date_str);

        {
            let mut csv_file = std::fs::File::create(csv_dir.join(&csv_name))
                .expect("Cannot create CSV file");

            if let Ok(mut entry) = archive.by_name(&csv_name) {
                std::io::copy(&mut entry, &mut csv_file).expect("Cannot extract CSV");
            }
        }

        // Ingest the CSV
        print!("ingesting ");
        std::io::stdout().flush().ok();

        let csv_path = csv_dir.join(&csv_name);
        let tvc_name = format!("{}_{}.tvc", symbol, date_str);
        let tvc_path = output_dir.join(&tvc_name);

        std::fs::create_dir_all(output_dir).ok();

        let ingestor = BinanceFileIngestor::new(symbol)
            .with_precision(precision)
            .with_anchor_interval(anchor_interval);

        match ingestor.ingest_file(&csv_path, &tvc_path) {
            Ok(result) => {
                total_ticks += result.count;
                files_written += 1;
                println!("{} ticks ✓", result.count);
            }
            Err(e) => {
                println!("ERROR: {}", e);
            }
        }

        // Clean up CSV
        std::fs::remove_file(&csv_path).ok();

        current += chrono::Duration::days(1);
    }

    // Clean up temp files
    std::fs::remove_dir_all(&temp_dir).ok();

    println!(
        "\n✓ Done — {} files, {} total ticks",
        files_written, total_ticks
    );
}
