//! TVCB encoding/decoding for delta bars.

use std::io::Cursor;

pub fn encode_ts_delta(delta_s: u64, buf: &mut Vec<u8>) {
    // 4-byte varint: up to 2^32 seconds (136 years)
    buf.push((delta_s & 0xFF) as u8);
    buf.push(((delta_s >> 8) & 0xFF) as u8);
    buf.push(((delta_s >> 16) & 0xFF) as u8);
    buf.push(((delta_s >> 24) & 0xFF) as u8);
}

pub fn decode_ts_delta(data: &[u8], pos: &mut usize) -> Result<u64, DecodeError> {
    if data.len() < *pos + 4 {
        return Err(DecodeError::UnexpectedEndOfFile);
    }
    let val = u32::from_le_bytes([data[*pos], data[*pos+1], data[*pos+2], data[*pos+3]]);
    *pos += 4;
    Ok(val as u64)
}

pub fn encode_zigzag_i64(val: i64, buf: &mut Vec<u8>) {
    let encoded = ((val << 1) ^ (val >> 63)) as u64;
    let mut v = encoded;
    let mut first = true;
    for _ in 0..9 {
        let byte = (v & 0x7F) as u8;
        v >>= 7;
        if v == 0 {
            buf.push(byte);
            break;
        } else {
            buf.push(byte | 0x80);
        }
    }
}

pub fn decode_zigzag_i64(data: &[u8], pos: &mut usize) -> Result<i64, DecodeError> {
    let mut result: i64 = 0;
    let mut shift: i32 = 0;
    loop {
        if *pos >= data.len() {
            return Err(DecodeError::UnexpectedEndOfFile);
        }
        let byte = data[*pos];
        *pos += 1;
        result |= ((byte & 0x7F) as i64) << shift;
        if byte & 0x80 == 0 {
            return Ok(((result as i64) >> 1) ^ -((result & 1) as i64));
        }
        shift += 7;
        if shift > 63 {
            return Err(DecodeError::ValueOutOfRange);
        }
    }
}

/// Volume zigzag encoding.
///
/// 4-byte encoding: standard little-endian u32 when the first byte of the
/// 4-byte representation is not 0xFF (avoids collision with overflow marker).
/// 9-byte encoding: 0xFF marker + 8-byte little-endian u64.
///
/// The overflow check must examine the actual byte representation, not just
/// the mathematical value range — zigzag-encoding of negative volume deltas
/// can produce values whose 4-byte LE first byte is 0xFF even when bit 31 is
/// not set in the mathematical sense.
pub fn encode_vol_zigzag(delta: i64, buf: &mut Vec<u8>) {
    let val = (delta << 1) ^ (delta >> 63);
    // Compute 4-byte representation to check for 0xFF collision
    let val_bytes = (val as u32).to_le_bytes();
    if val_bytes[0] == 0xFF {
        // Collision: 4-byte encoding's first byte collides with overflow marker.
        // Use 9-byte overflow encoding (0xFF + 8-byte u64).
        buf.push(0xFF);
        buf.extend_from_slice(&val.to_le_bytes());
    } else {
        // 4-byte encoding is safe — no 0xFF collision.
        buf.extend_from_slice(&val_bytes);
    }
}

pub fn decode_vol_zigzag(data: &[u8], pos: &mut usize) -> Result<i64, DecodeError> {
    if data.len() < *pos + 4 {
        return Err(DecodeError::UnexpectedEndOfFile);
    }
    if data[*pos] == 0xFF {
        if data.len() < *pos + 9 {
            return Err(DecodeError::UnexpectedEndOfFile);
        }
        let val = u64::from_le_bytes([
            data[*pos + 1], data[*pos + 2], data[*pos + 3], data[*pos + 4],
            data[*pos + 5], data[*pos + 6], data[*pos + 7], data[*pos + 8],
        ]);
        *pos += 9;
        return Ok(((val as i64) >> 1) ^ -((val & 1) as i64));
    }
    let val = u32::from_le_bytes([data[*pos], data[*pos+1], data[*pos+2], data[*pos+3]]);
    *pos += 4;
    Ok(((val as i64) >> 1) ^ -((val & 1) as i64))
}

/// Encode a delta bar (relative to previous bar).
pub fn encode_delta_bar(prev: &crate::tvcb::types::Bar, next: &crate::tvcb::types::Bar) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);

    // ts_delta: seconds since previous bar
    let ts_delta_ns = next.ts_event.saturating_sub(prev.ts_event);
    let ts_delta_s = ts_delta_ns / 1_000_000_000;
    encode_ts_delta(ts_delta_s, &mut buf);

    // Price deltas: relative to prev.close
    let price_base = prev.close;
    encode_zigzag_i64(next.close - price_base, &mut buf);
    encode_zigzag_i64(next.high - price_base, &mut buf);
    encode_zigzag_i64(next.low - price_base, &mut buf);
    encode_zigzag_i64(next.open - price_base, &mut buf);
    encode_zigzag_i64(next.vwap - price_base, &mut buf);

    // Volume deltas
    encode_vol_zigzag(next.volume - prev.volume, &mut buf);
    encode_vol_zigzag(next.buy_volume - prev.buy_volume, &mut buf);
    encode_vol_zigzag(next.sell_volume - prev.sell_volume, &mut buf);

    buf
}

/// Decode a delta bar into a full Bar, given the previous bar.
pub fn decode_delta_bar(data: &[u8], prev_bar: &crate::tvcb::types::Bar) -> Result<(crate::tvcb::types::Bar, usize), DecodeError> {
    let mut pos = 0;

    // ts_delta
    let ts_delta_s = decode_ts_delta(data, &mut pos)?;
    let ts_event = prev_bar.ts_event + ts_delta_s * 1_000_000_000;
    
    // Price deltas
    let price_base = prev_bar.close;
    let close = price_base + decode_zigzag_i64(data, &mut pos)?;
    let high = price_base + decode_zigzag_i64(data, &mut pos)?;
    let low = price_base + decode_zigzag_i64(data, &mut pos)?;
    let open = price_base + decode_zigzag_i64(data, &mut pos)?;
    let vwap = price_base + decode_zigzag_i64(data, &mut pos)?;
    
    // Volume deltas
    let volume = prev_bar.volume + decode_vol_zigzag(data, &mut pos)?;
    let buy_volume = prev_bar.buy_volume + decode_vol_zigzag(data, &mut pos)?;
    let sell_volume = prev_bar.sell_volume + decode_vol_zigzag(data, &mut pos)?;
    
    // tick_count is not delta-encoded (stored as absolute in anchor only)
    let tick_count = prev_bar.tick_count;
    
    let bar = crate::tvcb::types::Bar::new(
        ts_event, open, high, low, close, vwap,
        volume, buy_volume, sell_volume, tick_count,
    );
    
    Ok((bar, pos))
}

#[derive(Debug, PartialEq)]
pub enum DecodeError {
    UnexpectedEndOfFile,
    ValueOutOfRange,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tvcb::types::Bar;

    #[test]
    fn test_zigzag_encode_decode() {
        let cases = [0i64, 1, -1, 127, -127, 128, -128, 16383, -16383, 16384, -16384, 
                     1000000000i64, -1000000000, 4248800000000i64, -6827000000];
        for val in cases {
            let mut buf = Vec::new();
            encode_zigzag_i64(val, &mut buf);
            let mut pos = 0;
            let decoded = decode_zigzag_i64(&buf, &mut pos).unwrap();
            assert_eq!(decoded, val, "zigzag failed for {}", val);
        }
    }

    #[test]
    fn test_vol_zigzag() {
        let deltas = [0i64, 1, -1, 1000, -1000, 20000000, -20000000, 800000000, -800000000];
        for delta in deltas {
            let mut buf = Vec::new();
            encode_vol_zigzag(delta, &mut buf);
            let mut pos = 0;
            let decoded = decode_vol_zigzag(&buf, &mut pos).unwrap();
            assert_eq!(decoded, delta, "vol zigzag failed for delta={}", delta);
        }
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::UnexpectedEndOfFile => write!(f, "unexpected end of file"),
            DecodeError::ValueOutOfRange => write!(f, "value out of range"),
        }
    }
}
