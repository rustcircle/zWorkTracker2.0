//! Gzip of a whole file, with no new dependency.
//!
//! One gzip member (RFC 1952) around `miniz_oxide`'s raw deflate, with the CRC-32 checked on reading, so any unzip tool
//! opens a saved version and a copy that arrived only in part is never taken for a whole one.

use miniz_oxide::deflate::compress_to_vec;
use miniz_oxide::inflate::{TINFLStatus, decompress_to_vec_with_limit};

const ID: [u8; 2] = [0x1f, 0x8b];
const DEFLATE: u8 = 8;
/// Unknown, as RFC 1952 names it: the bytes are the same on every system.
const OS_UNKNOWN: u8 = 255;
/// zlib's default: most of the gain of the slowest level, at a fraction of its time.
const LEVEL: u8 = 6;
const FHCRC: u8 = 0x02;
const FEXTRA: u8 = 0x04;
const FNAME: u8 = 0x08;
const FCOMMENT: u8 = 0x10;
const RESERVED: u8 = 0xe0;
/// The CRC-32 and the length that end every member.
const TRAILER: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GzError {
    #[error("it isn't a gzip file")]
    NotGzip,
    #[error("the file is incomplete or damaged")]
    Damaged,
    #[error("it is unreasonably large once unpacked")]
    TooLarge,
}

/// CRC-32 as gzip and zip use it: reflected polynomial 0xEDB88320.
const TABLE: [u32; 256] = table();

#[expect(
    clippy::indexing_slicing,
    reason = "the loop bound is the table's length"
)]
const fn table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 == 1 {
                0xedb8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

#[must_use]
#[expect(clippy::indexing_slicing, reason = "a byte indexes a table of 256")]
pub fn crc32(bytes: &[u8]) -> u32 {
    !bytes.iter().fold(!0u32, |crc, &b| {
        TABLE[usize::from(crc.to_le_bytes()[0] ^ b)] ^ (crc >> 8)
    })
}

/// The whole of `data` as one gzip member, with no name and no time in its header, so the same data gives the same
/// bytes.
#[must_use]
pub fn compress(data: &[u8]) -> Vec<u8> {
    let body = compress_to_vec(data, LEVEL);
    let mut out = Vec::with_capacity(body.len() + 10 + TRAILER);
    out.extend_from_slice(&[ID[0], ID[1], DEFLATE, 0, 0, 0, 0, 0, 0, OS_UNKNOWN]);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(data).to_le_bytes());
    // ISIZE is the length modulo 2^32.
    out.extend_from_slice(&((data.len() as u64 & 0xffff_ffff) as u32).to_le_bytes());
    out
}

/// Where the deflate data starts, past the optional fields a header may have.
fn body_start(gz: &[u8]) -> Result<usize, GzError> {
    if gz.get(..2) != Some(&ID[..]) {
        return Err(GzError::NotGzip);
    }
    let (Some(&method), Some(&flags)) = (gz.get(2), gz.get(3)) else {
        return Err(GzError::Damaged);
    };
    if method != DEFLATE || flags & RESERVED != 0 {
        return Err(GzError::NotGzip);
    }
    let mut at = 10usize;
    if flags & FEXTRA != 0 {
        let len = gz
            .get(at..at + 2)
            .and_then(|b| <[u8; 2]>::try_from(b).ok())
            .ok_or(GzError::Damaged)?;
        at += 2 + usize::from(u16::from_le_bytes(len));
    }
    for flag in [FNAME, FCOMMENT] {
        if flags & flag != 0 {
            let rest = gz.get(at..).ok_or(GzError::Damaged)?;
            let end = rest.iter().position(|&b| b == 0).ok_or(GzError::Damaged)?;
            at += end + 1;
        }
    }
    if flags & FHCRC != 0 {
        at += 2;
    }
    Ok(at)
}

fn le32(bytes: Option<&[u8]>) -> Result<u32, GzError> {
    bytes
        .and_then(|b| <[u8; 4]>::try_from(b).ok())
        .map(u32::from_le_bytes)
        .ok_or(GzError::Damaged)
}

/// The data of a gzip file, if it is whole: the length and the CRC-32 in its trailer must match what unpacks, and
/// nothing larger than `limit` is unpacked.
pub fn decompress(gz: &[u8], limit: usize) -> Result<Vec<u8>, GzError> {
    let start = body_start(gz)?;
    let end = gz
        .len()
        .checked_sub(TRAILER)
        .filter(|&end| end >= start)
        .ok_or(GzError::Damaged)?;
    let body = gz.get(start..end).ok_or(GzError::Damaged)?;
    let crc = le32(gz.get(end..end + 4))?;
    let size = le32(gz.get(end + 4..))?;
    // One byte of room past the limit, so that data of exactly the limit isn't mistaken for more.
    let data = decompress_to_vec_with_limit(body, limit.saturating_add(1)).map_err(|e| {
        if e.status == TINFLStatus::HasMoreOutput {
            GzError::TooLarge
        } else {
            GzError::Damaged
        }
    })?;
    if data.len() > limit {
        return Err(GzError::TooLarge);
    }
    if crc32(&data) != crc || (data.len() as u64 & 0xffff_ffff) != u64::from(size) {
        return Err(GzError::Damaged);
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    const LIMIT: usize = 1 << 20;

    /// `printf 'hello\n' | gzip -n`, from the command line tool, header with no name.
    const FROM_GZIP: [u8; 26] = [
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0xe7, 0x02, 0x00, 0x20, 0x30, 0x3a, 0x36, 0x06, 0x00, 0x00, 0x00,
    ];

    /// Test 41.
    #[test]
    fn gzip_round_trip_and_crc_detects_truncation() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
        let data = br#"{"schemaVersion":2,"deviceId":"x","data":{"projects":{}}}"#.repeat(40);
        let gz = compress(&data);
        assert_eq!(gz.get(..3), Some(&[0x1f, 0x8b, 0x08][..]));
        assert!(gz.len() < data.len() / 4, "{} of {}", gz.len(), data.len());
        assert_eq!(decompress(&gz, LIMIT).unwrap(), data);
        assert_eq!(compress(&data), gz, "the same data gives the same bytes");

        // A copy cut short anywhere is never taken for the whole.
        for len in 0..gz.len() {
            assert!(decompress(&gz[..len], LIMIT).is_err(), "cut at {len}");
        }
        // Nor one with a byte changed in its data, its checksum or its length.
        for at in 10..gz.len() {
            let mut bad = gz.clone();
            bad[at] ^= 0x01;
            assert!(decompress(&bad, LIMIT).is_err(), "byte {at} changed");
        }
        assert_eq!(decompress(b"{\"a\":1}", LIMIT), Err(GzError::NotGzip));
        assert_eq!(decompress(&gz, data.len() - 1), Err(GzError::TooLarge));
        assert_eq!(decompress(&gz, data.len()).unwrap(), data);
    }

    #[test]
    fn a_file_made_by_the_gzip_tool_reads_and_so_do_optional_header_fields() {
        assert_eq!(decompress(&FROM_GZIP, LIMIT).unwrap(), b"hello\n");
        // The same member with a file name, a comment, an extra field and a header CRC, as other tools may write.
        let mut fancy = vec![
            0x1f,
            0x8b,
            0x08,
            FEXTRA | FNAME | FCOMMENT | FHCRC,
            0,
            0,
            0,
            0,
            0,
            3,
        ];
        fancy.extend_from_slice(&[3, 0, b'a', b'b', b'c']);
        fancy.extend_from_slice(b"hello.txt\0");
        fancy.extend_from_slice(b"a comment\0");
        fancy.extend_from_slice(&[0xaa, 0xbb]);
        fancy.extend_from_slice(&FROM_GZIP[10..]);
        assert_eq!(decompress(&fancy, LIMIT).unwrap(), b"hello\n");
        // A name that never ends is damage, not a reason to read past the end.
        let open_name = [0x1f, 0x8b, 0x08, FNAME, 0, 0, 0, 0, 0, 3, b'a', b'b'];
        assert_eq!(decompress(&open_name, LIMIT), Err(GzError::Damaged));
        let reserved = [0x1f, 0x8b, 0x08, 0x20, 0, 0, 0, 0, 0, 3, 0, 0];
        assert_eq!(decompress(&reserved, LIMIT), Err(GzError::NotGzip));
    }

    proptest! {
        #[test]
        fn any_bytes_round_trip(data in proptest::collection::vec(any::<u8>(), 0..4096)) {
            prop_assert_eq!(decompress(&compress(&data), LIMIT).unwrap(), data);
        }

        #[test]
        fn damage_never_panics(gz in proptest::collection::vec(any::<u8>(), 0..64)) {
            let _ = decompress(&gz, LIMIT);
        }
    }
}
