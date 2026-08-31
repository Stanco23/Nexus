//! Shared CSV-parsing utilities for exchange downloaders.
//!
//! Both Binance and Bybit use the same decimal-string-to-i64 conversion logic
//! for price and size fields. Centralized here to avoid duplication.

/// Parse a decimal string ("93530.00000000" or "0.00136") into a nano-integer
/// using the given precision (decimals). Both price and size use the same
/// precision (default 9 for nano-units in this codebase).
///
/// Examples:
/// - `parse_price_to_int("93576.00000000", 9)` → `93_576_000_000_000`
/// - `parse_price_to_int("0.00136000", 9)` → `1_360_000`
/// - `parse_price_to_int("0.003", 9)` → `3_000_000`
pub fn parse_price_to_int(s: &str, precision: u8) -> i64 {
    let s = s.trim();
    // Find decimal point (if any)
    let (int_part, frac_part) = match s.find('.') {
        Some(i) => (&s[..i], &s[i + 1..]),
        None => (s, ""),
    };
    // Combine integer and fractional parts into a single i64
    let int_val: i64 = int_part.parse().unwrap_or(0);
    // Build the fractional part as an integer up to `precision` digits,
    // padding with zeros if the input has fewer.
    let frac_str: String = frac_part
        .chars()
        .take(precision as usize)
        .chain(std::iter::repeat('0'))
        .take(precision as usize)
        .collect();
    let frac_val: i64 = if frac_str.is_empty() {
        0
    } else {
        frac_str.parse().unwrap_or(0)
    };
    let combined_int = int_val
        .checked_mul(10_i64.pow(precision as u32))
        .and_then(|x| x.checked_add(frac_val))
        .unwrap_or(0);

    // Adjust if the fractional part had fewer digits than `precision`
    let actual_frac_len = frac_part.len() as i32;
    let diff = precision as i32 - actual_frac_len;
    if diff >= 0 {
        combined_int * 10_i64.pow(diff as u32)
    } else {
        combined_int / 10_i64.pow((-diff) as u32)
    }
}

/// Alias kept for clarity at call sites that explicitly parse quantity fields.
/// Implementation is identical to `parse_price_to_int`.
pub fn parse_qty_to_int(s: &str, precision: u8) -> i64 {
    parse_price_to_int(s, precision)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_price_to_int() {
        assert_eq!(parse_price_to_int("93576.00000000", 9), 93_576_000_000_000);
        assert_eq!(parse_price_to_int("50000.12345678", 9), 50_000_123_456_780);
        assert_eq!(parse_price_to_int("93530.00", 9), 93_530_000_000_000);
        assert_eq!(parse_price_to_int("50000", 9), 50_000_000_000_000);
    }

    #[test]
    fn test_parse_qty_to_int() {
        assert_eq!(parse_qty_to_int("0.00136000", 9), 1_360_000);
        assert_eq!(parse_qty_to_int("0.003", 9), 3_000_000);
        assert_eq!(parse_qty_to_int("1.5", 9), 1_500_000_000);
    }
}