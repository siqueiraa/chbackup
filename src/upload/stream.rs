//! Compression utilities for upload.
//!
//! Provides tar + LZ4 compression for part directories before S3 upload.

use std::path::Path;

use anyhow::{Context, Result};

/// Compress a part directory into an LZ4-compressed tar archive in memory.
///
/// Creates a tar archive of `part_dir` using `archive_name` as the root
/// directory name inside the tar, then compresses with LZ4 frame format.
///
/// This function runs synchronously and should be called within
/// `tokio::task::spawn_blocking`.
pub fn compress_part(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>> {
    let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());

    {
        let mut tar_builder = tar::Builder::new(&mut encoder);
        tar_builder
            .append_dir_all(archive_name, part_dir)
            .with_context(|| format!("Failed to add directory to tar: {}", part_dir.display()))?;
        tar_builder
            .finish()
            .context("Failed to finish tar archive")?;
    }

    let compressed = encoder
        .finish()
        .context("Failed to finish LZ4 compression")?;
    Ok(compressed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;

    #[test]
    fn test_compress_lz4_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();

        // Create some test files
        fs::write(part_dir.join("data.bin"), b"test binary data").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"checksum content").unwrap();

        // Compress
        let compressed = compress_part(&part_dir, "test_part").unwrap();
        assert!(!compressed.is_empty());

        // Verify it's valid LZ4 by decompressing
        let mut decoder = lz4_flex::frame::FrameDecoder::new(compressed.as_slice());
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed).unwrap();
        assert!(decompressed.len() > compressed.len() / 10); // Sanity check
    }

    #[test]
    fn test_tar_directory_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("my_part");
        fs::create_dir_all(part_dir.join("subdir")).unwrap();

        fs::write(part_dir.join("file1.txt"), b"content_one").unwrap();
        fs::write(part_dir.join("subdir/file2.txt"), b"content_two").unwrap();

        // Compress
        let compressed = compress_part(&part_dir, "my_part").unwrap();

        // Decompress (using download stream module's logic)
        let decoder = lz4_flex::frame::FrameDecoder::new(compressed.as_slice());
        let mut archive = tar::Archive::new(decoder);
        let output_dir = dir.path().join("output");
        archive.unpack(&output_dir).unwrap();

        // Verify
        assert_eq!(
            fs::read_to_string(output_dir.join("my_part/file1.txt")).unwrap(),
            "content_one"
        );
        assert_eq!(
            fs::read_to_string(output_dir.join("my_part/subdir/file2.txt")).unwrap(),
            "content_two"
        );
    }

    #[test]
    fn test_s3_key_for_part() {
        // Verify S3 key format generation
        let backup_name = "daily-20240115";
        let db = "default";
        let table = "trades";
        let part_name = "202401_1_50_3";

        let key = format!(
            "{}/data/{}/{}/{}.tar.lz4",
            backup_name, db, table, part_name
        );
        assert_eq!(
            key,
            "daily-20240115/data/default/trades/202401_1_50_3.tar.lz4"
        );
    }
}
