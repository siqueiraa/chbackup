//! CRC64 checksum computation for backup part verification.
//!
//! Uses CRC-64/XZ algorithm for ClickHouse compatibility.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use crc::Crc;

/// CRC-64/XZ algorithm instance (matches ClickHouse checksum format).
const CRC64_XZ: Crc<u64> = Crc::<u64>::new(&crc::CRC_64_XZ);

/// Compute CRC64/XZ checksum of a file's contents.
///
/// Reads the file in 64KB chunks to avoid loading the entire file into memory.
pub fn compute_crc64(path: &Path) -> Result<u64> {
    let mut file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open file for CRC64: {}", path.display()))?;

    let mut digest = CRC64_XZ.digest();
    let mut buf = [0u8; 65536];

    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("Failed to read file for CRC64: {}", path.display()))?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
    }

    Ok(digest.finalize())
}

/// Compute CRC64/XZ checksum of a byte slice.
pub fn compute_crc64_bytes(data: &[u8]) -> u64 {
    CRC64_XZ.checksum(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_compute_crc64_known_value() {
        // Compute CRC64 of known byte sequence
        let checksum = compute_crc64_bytes(b"Hello, ClickHouse!");
        assert_ne!(checksum, 0, "CRC64 of non-empty data should not be zero");
    }

    #[test]
    fn test_compute_crc64_deterministic() {
        let data = b"test data for crc64 computation";
        let c1 = compute_crc64_bytes(data);
        let c2 = compute_crc64_bytes(data);
        assert_eq!(c1, c2, "CRC64 must be deterministic");
    }

    #[test]
    fn test_compute_crc64_empty() {
        let checksum = compute_crc64_bytes(b"");
        // CRC64/XZ of empty data is 0
        assert_eq!(checksum, 0);
    }

    #[test]
    fn test_compute_crc64_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test_checksums.txt");

        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(b"columns format version: 1\n").unwrap();
        f.write_all(b"2 columns:\n").unwrap();
        f.write_all(b"`id` UInt64\n").unwrap();
        f.write_all(b"`name` String\n").unwrap();
        drop(f);

        let checksum = compute_crc64(&path).unwrap();
        assert_ne!(checksum, 0);

        // Re-read should produce same checksum
        let checksum2 = compute_crc64(&path).unwrap();
        assert_eq!(checksum, checksum2);
    }

    #[test]
    fn test_compute_crc64_file_matches_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("match_test.txt");
        let content = b"some test content here";

        std::fs::write(&path, content).unwrap();

        let file_checksum = compute_crc64(&path).unwrap();
        let bytes_checksum = compute_crc64_bytes(content);
        assert_eq!(file_checksum, bytes_checksum);
    }
}
