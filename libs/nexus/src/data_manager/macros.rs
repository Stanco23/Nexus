//! Utility macros for the data layer.

/// Compile-time helper to create a `chrono::NaiveDate` from `YYYY-MM-DD`.
///
/// Example: `let d = naive_date!(2025, 1, 2);`
#[macro_export]
macro_rules! naive_date {
    ($y:expr, $m:expr, $d:expr) => {{
        ::chrono::NaiveDate::from_ymd_opt($y, $m, $d)
            .expect("valid date in naive_date! macro")
    }};
}

/// Macro to create a NaiveDate from a `YYYY-MM-DD` string literal.
/// Example: `naive_date_iso!("2025-01-02")`
#[macro_export]
macro_rules! naive_date_iso {
    ($s:literal) => {{
        ::chrono::NaiveDate::parse_from_str($s, "%Y-%m-%d")
            .expect("valid YYYY-MM-DD in naive_date_iso! macro")
    }};
}
