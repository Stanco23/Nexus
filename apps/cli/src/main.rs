use clap::Parser;
use std::path::PathBuf;

#[derive(clap::Parser)]
#[command(name = "nexus")]
#[command(about = "Nexus backtesting CLI")]
enum Cmd {
    /// Calibrate VPIN slippage constants from historical fill data.
    CalibrateSlippage {
        /// Input CSV with columns: order_size_ticks, vpin, actual_impact_bps
        #[arg(long)]
        input: PathBuf,
        /// Output JSON file for the calibrated SlippageConfig
        #[arg(long)]
        output: PathBuf,
    },
}

fn main() {
    let cmd = Cmd::parse();
    match cmd {
        Cmd::CalibrateSlippage { input, output } => {
            let result = nexus::calibrate::calibrate_vpin(
                input.to_str().unwrap(),
                output.to_str().unwrap(),
            );
            if !result.holdout_pass {
                std::process::exit(1);
            }
        }
    }
}
