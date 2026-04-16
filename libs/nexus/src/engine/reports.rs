//! Execution reports — fill reports, order reports, and commission types.
//!
//! Nautilus Source: `execution/reports.py`

use serde::{Deserialize, Serialize};

/// Commission side — maker or taker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommissionSide {
    Maker,
    Taker,
}

/// Commission amount, currency, and side.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Commission {
    pub amount: f64,
    pub currency: String,
    pub side: CommissionSide,
}

impl Commission {
    /// Maker fee: 0.02%, Taker fee: 0.06%
    pub fn compute_commission(side: CommissionSide, price: f64, size: f64) -> Self {
        let rate = match side {
            CommissionSide::Maker => 0.0002,
            CommissionSide::Taker => 0.0006,
        };
        let amount = price * size * rate;
        Self {
            amount,
            currency: "USDT".to_string(),
            side,
        }
    }
}

/// Order report status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderReportStatus {
    Pending,
    PartiallyFilled,
    Filled,
    Cancelled,
    Rejected,
}

/// A fill report — single partial fill of an order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FillReport {
    pub fill_id: String,
    pub timestamp_ns: u64,
    pub order_id: String,
    pub instrument_id: String,
    pub venue: String,
    pub side: String,
    pub size: f64,
    pub fill_price: f64,
    pub market_price: f64,
    pub slippage_bps: f64,
    pub commission: Commission,
    pub queue_position: Option<u32>,
    pub vpin_at_fill: Option<f64>,
    pub latency_ns: Option<u64>,
}

impl FillReport {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        fill_id: String,
        timestamp_ns: u64,
        order_id: String,
        instrument_id: String,
        venue: String,
        side: String,
        size: f64,
        fill_price: f64,
        market_price: f64,
        commission: Commission,
    ) -> Self {
        let slippage_bps = (fill_price - market_price).abs() / market_price * 10000.0;
        Self {
            fill_id,
            timestamp_ns,
            order_id,
            instrument_id,
            venue,
            side,
            size,
            fill_price,
            market_price,
            slippage_bps,
            commission,
            queue_position: None,
            vpin_at_fill: None,
            latency_ns: None,
        }
    }

    /// Effective price = fill_price ± commission/size.
    /// Buy: add commission (you pay more), Sell: subtract commission (you receive less).
    pub fn effective_price(&self) -> f64 {
        let comm = self.commission.amount / self.size;
        if self.side == "Buy" || self.side == "buy" {
            self.fill_price + comm
        } else {
            self.fill_price - comm
        }
    }
}

/// An order report — tracks order lifecycle from submission to finalization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderReport {
    pub submission_order_id: String,
    pub instrument_id: String,
    pub venue: String,
    pub side: String,
    pub size: f64,
    pub submission_price: f64,
    pub fills: Vec<FillReport>,
    pub status: OrderReportStatus,
    pub total_filled: f64,
    pub total_commission: f64,
    pub avg_fill_price: f64,
    pub slippage_bps: f64,
    pub ts_submitted: u64,
    pub ts_finalized: Option<u64>,
}

impl OrderReport {
    pub fn new(
        submission_order_id: String,
        instrument_id: String,
        venue: String,
        side: String,
        size: f64,
        submission_price: f64,
        ts_submitted: u64,
    ) -> Self {
        Self {
            submission_order_id,
            instrument_id,
            venue,
            side,
            size,
            submission_price,
            fills: Vec::new(),
            status: OrderReportStatus::Pending,
            total_filled: 0.0,
            total_commission: 0.0,
            avg_fill_price: 0.0,
            slippage_bps: 0.0,
            ts_submitted,
            ts_finalized: None,
        }
    }

    pub fn add_fill(&mut self, fill: FillReport) {
        self.total_filled += fill.size;
        self.total_commission += fill.commission.amount;
        self.fills.push(fill);
    }

    pub fn finalize(&mut self, ts_finalized: u64) {
        self.ts_finalized = Some(ts_finalized);

        if self.fills.is_empty() {
            self.status = OrderReportStatus::Cancelled;
            return;
        }

        let mut price_times_size = 0.0;
        for fill in &self.fills {
            price_times_size += fill.fill_price * fill.size;
        }
        self.avg_fill_price = price_times_size / self.total_filled;

        self.slippage_bps =
            (self.avg_fill_price - self.submission_price).abs() / self.submission_price * 10000.0;

        if self.total_filled >= self.size {
            self.status = OrderReportStatus::Filled;
        } else {
            self.status = OrderReportStatus::PartiallyFilled;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fill_report_slippage_bps() {
        let commission = Commission::compute_commission(CommissionSide::Taker, 50_000.0, 1.0);
        let fill = FillReport::new(
            "FILL-001".to_string(),
            1_000_000_000,
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            1.0,
            50_050.0,
            50_000.0,
            commission,
        );
        // slippage = |50050 - 50000| / 50000 * 10000 = 50/50000*10000 = 10 bps
        assert!((fill.slippage_bps - 10.0).abs() < 0.01);
    }

    #[test]
    fn test_fill_report_effective_price_buy() {
        let commission = Commission::compute_commission(CommissionSide::Taker, 50_000.0, 1.0);
        let fill = FillReport::new(
            "FILL-001".to_string(),
            1_000_000_000,
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            1.0,
            50_000.0,
            50_000.0,
            commission,
        );
        // commission = 50000 * 1.0 * 0.0006 = 30
        // effective_price = 50000 + 30/1 = 50030
        assert!((fill.effective_price() - 50_030.0).abs() < 0.01);
    }

    #[test]
    fn test_fill_report_effective_price_sell() {
        let commission = Commission::compute_commission(CommissionSide::Taker, 50_000.0, 1.0);
        let fill = FillReport::new(
            "FILL-001".to_string(),
            1_000_000_000,
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Sell".to_string(),
            1.0,
            50_000.0,
            50_000.0,
            commission,
        );
        // effective_price = 50000 - 30 = 49970
        assert!((fill.effective_price() - 49_970.0).abs() < 0.01);
    }

    #[test]
    fn test_commission_rates() {
        let taker = Commission::compute_commission(CommissionSide::Taker, 100.0, 1.0);
        let maker = Commission::compute_commission(CommissionSide::Maker, 100.0, 1.0);
        // Taker: 100 * 1 * 0.0006 = 0.06
        assert!((taker.amount - 0.06).abs() < 0.0001);
        // Maker: 100 * 1 * 0.0002 = 0.02
        assert!((maker.amount - 0.02).abs() < 0.0001);
    }

    #[test]
    fn test_order_report_add_fill() {
        let mut report = OrderReport::new(
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            1.0,
            50_000.0,
            1_000_000_000,
        );
        let commission = Commission::compute_commission(CommissionSide::Taker, 50_000.0, 0.5);
        let fill = FillReport::new(
            "FILL-001".to_string(),
            1_000_000_000,
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            0.5,
            50_000.0,
            50_000.0,
            commission,
        );
        report.add_fill(fill);
        assert_eq!(report.total_filled, 0.5);
        // commission = 50000 * 0.5 * 0.0006 = 15.0
        assert!((report.total_commission - 15.0).abs() < 0.001);
    }

    #[test]
    fn test_order_report_finalize_filled() {
        let mut report = OrderReport::new(
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            1.0,
            50_000.0,
            1_000_000_000,
        );
        let commission = Commission::compute_commission(CommissionSide::Taker, 50_000.0, 1.0);
        let fill = FillReport::new(
            "FILL-001".to_string(),
            1_000_000_000,
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            1.0,
            50_000.0,
            50_000.0,
            commission,
        );
        report.add_fill(fill);
        report.finalize(2_000_000_000);
        assert_eq!(report.status, OrderReportStatus::Filled);
        assert_eq!(report.avg_fill_price, 50_000.0);
        assert!((report.slippage_bps - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_order_report_finalize_partially_filled() {
        let mut report = OrderReport::new(
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            1.0,
            50_000.0,
            1_000_000_000,
        );
        let commission = Commission::compute_commission(CommissionSide::Taker, 50_000.0, 0.3);
        let fill = FillReport::new(
            "FILL-001".to_string(),
            1_000_000_000,
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            0.3,
            50_100.0,
            50_000.0,
            commission,
        );
        report.add_fill(fill);
        report.finalize(2_000_000_000);
        assert_eq!(report.status, OrderReportStatus::PartiallyFilled);
        assert_eq!(report.total_filled, 0.3);
        // slippage = |50100-50000|/50000*10000 = 20 bps
        assert!((report.slippage_bps - 20.0).abs() < 0.01);
    }

    #[test]
    fn test_order_report_zero_fills_cancelled() {
        let mut report = OrderReport::new(
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            1.0,
            50_000.0,
            1_000_000_000,
        );
        report.finalize(2_000_000_000);
        assert_eq!(report.status, OrderReportStatus::Cancelled);
    }

    #[test]
    fn test_fill_report_json_roundtrip() {
        let commission = Commission::compute_commission(CommissionSide::Taker, 50_000.0, 1.0);
        let fill = FillReport::new(
            "FILL-001".to_string(),
            1_000_000_000,
            "ORD-001".to_string(),
            "BTC-USDT".to_string(),
            "BINANCE".to_string(),
            "Buy".to_string(),
            1.0,
            50_000.0,
            50_000.0,
            commission,
        );
        let json = serde_json::to_string(&fill).unwrap();
        let parsed: FillReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.fill_id, fill.fill_id);
        assert!((parsed.slippage_bps - fill.slippage_bps).abs() < 0.01);
    }
}