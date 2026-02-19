//! Compression utilities for upload.
//!
//! Provides tar + compression for part directories before S3 upload.
//! Supports LZ4, zstd, gzip, and uncompressed (none) formats.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};

/// Return the archive file extension for the given compression format.
///
/// Maps format names to their conventional tar extension:
/// - `"lz4"` -> `".tar.lz4"`
/// - `"zstd"` -> `".tar.zstd"`
/// - `"gzip"` -> `".tar.gz"`
/// - `"none"` -> `".tar"`
/// - anything else -> `".tar.lz4"` (default, matching legacy behavior)
pub fn archive_extension(data_format: &str) -> &str {
    match data_format {
        "lz4" => ".tar.lz4",
        "zstd" => ".tar.zstd",
        "gzip" => ".tar.gz",
        "none" => ".tar",
        _ => ".tar.lz4",
    }
}

/// Compress a part directory into a compressed tar archive in memory.
///
/// Creates a tar archive of `part_dir` using `archive_name` as the root
/// directory name inside the tar, then compresses with the specified format.
///
/// Supported formats:
/// - `"lz4"`: LZ4 frame compression (ignores `compression_level`)
/// - `"zstd"`: Zstandard compression (`compression_level` maps to zstd level)
/// - `"gzip"`: gzip compression (`compression_level` maps to flate2 level)
/// - `"none"`: no compression (raw tar)
///
/// This function runs synchronously and should be called within
/// `tokio::task::spawn_blocking`.
pub fn compress_part(
    part_dir: &Path,
    archive_name: &str,
    data_format: &str,
    compression_level: u32,
) -> Result<Vec<u8>> {
    match data_format {
        "lz4" => compress_lz4(part_dir, archive_name),
        "zstd" => compress_zstd(part_dir, archive_name, compression_level),
        "gzip" => compress_gzip(part_dir, archive_name, compression_level),
        "none" => compress_none(part_dir, archive_name),
        other => Err(anyhow::anyhow!("Unknown compression format: {}", other)),
    }
}

/// Compress with LZ4 frame format.
fn compress_lz4(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>> {
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

/// Compress with Zstandard format.
fn compress_zstd(
    part_dir: &Path,
    archive_name: &str,
    compression_level: u32,
) -> Result<Vec<u8>> {
    let mut encoder = zstd::Encoder::new(Vec::new(), compression_level as i32)
        .context("Failed to create zstd encoder")?;

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
        .context("Failed to finish zstd compression")?;
    Ok(compressed)
}

/// Compress with gzip format.
fn compress_gzip(
    part_dir: &Path,
    archive_name: &str,
    compression_level: u32,
) -> Result<Vec<u8>> {
    let mut encoder = flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::new(compression_level),
    );

    {
        let mut tar_builder = tar::Builder::new(&mut encoder);
        tar_builder
            .append_dir_all(archive_name, part_dir)
            .with_context(|| format!("Failed to add directory to tar: {}", part_dir.display()))?;
        tar_builder
            .finish()
            .context("Failed to finish tar archive")?;
    }

    // Flush and finalize the gzip stream
    encoder.flush().context("Failed to flush gzip encoder")?;
    let compressed = encoder
        .finish()
        .context("Failed to finish gzip compression")?;
    Ok(compressed)
}

/// No compression -- raw tar only.
fn compress_none(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();

    {
        let mut tar_builder = tar::Builder::new(&mut buffer);
        tar_builder
            .append_dir_all(archive_name, part_dir)
            .with_context(|| format!("Failed to add directory to tar: {}", part_dir.display()))?;
        tar_builder
            .finish()
            .context("Failed to finish tar archive")?;
    }

    Ok(buffer)
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

        // Compress with lz4 format
        let compressed = compress_part(&part_dir, "test_part", "lz4", 1).unwrap();
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

        // Compress with lz4
        let compressed = compress_part(&part_dir, "my_part", "lz4", 1).unwrap();

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
            "{}/data/{}/{}/{}{}",
            backup_name,
            db,
            table,
            part_name,
            archive_extension("lz4")
        );
        assert_eq!(
            key,
            "daily-20240115/data/default/trades/202401_1_50_3.tar.lz4"
        );
    }

    #[test]
    fn test_archive_extension_mapping() {
        assert_eq!(archive_extension("lz4"), ".tar.lz4");
        assert_eq!(archive_extension("zstd"), ".tar.zstd");
        assert_eq!(archive_extension("gzip"), ".tar.gz");
        assert_eq!(archive_extension("none"), ".tar");
        // Unknown defaults to lz4
        assert_eq!(archive_extension("unknown"), ".tar.lz4");
    }

    #[test]
    fn test_compress_decompress_zstd_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();

        fs::write(part_dir.join("data.bin"), b"test binary data for zstd").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"checksum content zstd").unwrap();

        // Compress with zstd
        let compressed = compress_part(&part_dir, "test_part", "zstd", 3).unwrap();
        assert!(!compressed.is_empty());

        // Decompress with zstd
        let decoder = zstd::Decoder::new(compressed.as_slice()).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let output_dir = dir.path().join("output");
        archive.unpack(&output_dir).unwrap();

        // Verify files
        assert_eq!(
            fs::read(output_dir.join("test_part/data.bin")).unwrap(),
            b"test binary data for zstd"
        );
        assert_eq!(
            fs::read(output_dir.join("test_part/checksums.txt")).unwrap(),
            b"checksum content zstd"
        );
    }

    #[test]
    fn test_compress_decompress_gzip_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();

        fs::write(part_dir.join("data.bin"), b"test binary data for gzip").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"checksum content gzip").unwrap();

        // Compress with gzip
        let compressed = compress_part(&part_dir, "test_part", "gzip", 6).unwrap();
        assert!(!compressed.is_empty());

        // Decompress with gzip
        let decoder = flate2::read::GzDecoder::new(compressed.as_slice());
        let mut archive = tar::Archive::new(decoder);
        let output_dir = dir.path().join("output");
        archive.unpack(&output_dir).unwrap();

        // Verify files
        assert_eq!(
            fs::read(output_dir.join("test_part/data.bin")).unwrap(),
            b"test binary data for gzip"
        );
        assert_eq!(
            fs::read(output_dir.join("test_part/checksums.txt")).unwrap(),
            b"checksum content gzip"
        );
    }

    #[test]
    fn test_compress_decompress_none_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();

        fs::write(part_dir.join("data.bin"), b"test binary data uncompressed").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"checksum content none").unwrap();

        // Compress with none (raw tar)
        let compressed = compress_part(&part_dir, "test_part", "none", 0).unwrap();
        assert!(!compressed.is_empty());

        // Decompress (just untar, no decompressor)
        let cursor = std::io::Cursor::new(compressed.as_slice());
        let mut archive = tar::Archive::new(cursor);
        let output_dir = dir.path().join("output");
        archive.unpack(&output_dir).unwrap();

        // Verify files
        assert_eq!(
            fs::read(output_dir.join("test_part/data.bin")).unwrap(),
            b"test binary data uncompressed"
        );
        assert_eq!(
            fs::read(output_dir.join("test_part/checksums.txt")).unwrap(),
            b"checksum content none"
        );
    }

    #[test]
    fn test_compress_unknown_format_errors() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();
        fs::write(part_dir.join("data.bin"), b"test").unwrap();

        let result = compress_part(&part_dir, "test_part", "brotli", 1);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("Unknown compression format: brotli"));
    }
}
