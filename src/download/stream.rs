//! Decompression and extraction utilities for downloaded backup parts.
//!
//! Supports LZ4, zstd, gzip, and uncompressed (none) formats.

use std::path::Path;

use anyhow::{Context, Result};

fn unpack_archive_from_reader<R: std::io::Read>(
    reader: R,
    output_dir: &Path,
    data_format: &str,
) -> Result<()> {
    match data_format {
        "lz4" => {
            let decoder = lz4_flex::frame::FrameDecoder::new(reader);
            let mut archive = tar::Archive::new(decoder);
            archive.unpack(output_dir).with_context(|| {
                format!(
                    "Failed to unpack LZ4 tar archive to: {}",
                    output_dir.display()
                )
            })?;
        }
        "zstd" => {
            let decoder = zstd::Decoder::new(reader).context("Failed to create zstd decoder")?;
            let mut archive = tar::Archive::new(decoder);
            archive.unpack(output_dir).with_context(|| {
                format!(
                    "Failed to unpack zstd tar archive to: {}",
                    output_dir.display()
                )
            })?;
        }
        "gzip" => {
            let decoder = flate2::read::GzDecoder::new(reader);
            let mut archive = tar::Archive::new(decoder);
            archive.unpack(output_dir).with_context(|| {
                format!(
                    "Failed to unpack gzip tar archive to: {}",
                    output_dir.display()
                )
            })?;
        }
        "none" => {
            let mut archive = tar::Archive::new(reader);
            archive.unpack(output_dir).with_context(|| {
                format!(
                    "Failed to unpack raw tar archive to: {}",
                    output_dir.display()
                )
            })?;
        }
        other => {
            return Err(anyhow::anyhow!("Unknown compression format: {}", other));
        }
    }

    Ok(())
}

/// Decompress a compressed tar archive and extract to the output directory.
///
/// The `data` is expected to be a tar archive compressed with the specified format.
/// Files are extracted into `output_dir`, preserving the directory structure
/// from the tar archive.
///
/// Supported formats:
/// - `"lz4"`: LZ4 frame decompression
/// - `"zstd"`: Zstandard decompression
/// - `"gzip"`: gzip decompression
/// - `"none"`: no decompression (raw tar)
///
/// This function runs synchronously and should be called within
/// `tokio::task::spawn_blocking`.
pub fn decompress_part(data: &[u8], output_dir: &Path, data_format: &str) -> Result<()> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output dir: {}", output_dir.display()))?;

    let cursor = std::io::Cursor::new(data);
    unpack_archive_from_reader(cursor, output_dir, data_format)?;

    Ok(())
}

/// Decompress a compressed tar archive stored on disk and extract to output_dir.
///
/// This avoids buffering the full compressed archive in memory and is intended
/// for large downloaded parts.
pub fn decompress_part_file(
    archive_path: &Path,
    output_dir: &Path,
    data_format: &str,
) -> Result<()> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output dir: {}", output_dir.display()))?;

    let file = std::fs::File::open(archive_path)
        .with_context(|| format!("Failed to open archive: {}", archive_path.display()))?;
    let reader = std::io::BufReader::new(file);
    unpack_archive_from_reader(reader, output_dir, data_format)?;

    Ok(())
}

/// Compress a directory into a compressed tar archive in memory.
///
/// Creates a tar archive of `part_dir` (using `archive_name` as the root directory
/// name inside the tar), then compresses it with the specified format.
///
/// This function runs synchronously and should be called within
/// `tokio::task::spawn_blocking`.
///
/// Only used in tests to create test data for decompression round-trip verification.
/// The production compress path lives in `upload::stream::compress_part`.
#[cfg(test)]
pub fn compress_part(
    part_dir: &Path,
    archive_name: &str,
    data_format: &str,
    compression_level: u32,
) -> Result<Vec<u8>> {
    match data_format {
        "lz4" => {
            let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
            {
                let mut tar_builder = tar::Builder::new(&mut encoder);
                tar_builder
                    .append_dir_all(archive_name, part_dir)
                    .with_context(|| {
                        format!("Failed to add directory to tar: {}", part_dir.display())
                    })?;
                tar_builder
                    .finish()
                    .context("Failed to finish tar archive")?;
            }
            let compressed = encoder
                .finish()
                .context("Failed to finish LZ4 compression")?;
            Ok(compressed)
        }
        "zstd" => {
            let mut encoder = zstd::Encoder::new(Vec::new(), compression_level as i32)
                .context("Failed to create zstd encoder")?;
            {
                let mut tar_builder = tar::Builder::new(&mut encoder);
                tar_builder
                    .append_dir_all(archive_name, part_dir)
                    .with_context(|| {
                        format!("Failed to add directory to tar: {}", part_dir.display())
                    })?;
                tar_builder
                    .finish()
                    .context("Failed to finish tar archive")?;
            }
            let compressed = encoder
                .finish()
                .context("Failed to finish zstd compression")?;
            Ok(compressed)
        }
        "gzip" => {
            use std::io::Write;
            let mut encoder = flate2::write::GzEncoder::new(
                Vec::new(),
                flate2::Compression::new(compression_level),
            );
            {
                let mut tar_builder = tar::Builder::new(&mut encoder);
                tar_builder
                    .append_dir_all(archive_name, part_dir)
                    .with_context(|| {
                        format!("Failed to add directory to tar: {}", part_dir.display())
                    })?;
                tar_builder
                    .finish()
                    .context("Failed to finish tar archive")?;
            }
            encoder.flush().context("Failed to flush gzip encoder")?;
            let compressed = encoder
                .finish()
                .context("Failed to finish gzip compression")?;
            Ok(compressed)
        }
        "none" => {
            let mut buffer = Vec::new();
            {
                let mut tar_builder = tar::Builder::new(&mut buffer);
                tar_builder
                    .append_dir_all(archive_name, part_dir)
                    .with_context(|| {
                        format!("Failed to add directory to tar: {}", part_dir.display())
                    })?;
                tar_builder
                    .finish()
                    .context("Failed to finish tar archive")?;
            }
            Ok(buffer)
        }
        other => Err(anyhow::anyhow!("Unknown compression format: {}", other)),
    }
}

/// Decompress raw LZ4 frame data to bytes.
///
/// Only used in tests for LZ4 round-trip verification.
/// The production decompression path uses `decompress_part` with format dispatch.
#[cfg(test)]
pub fn decompress_lz4(data: &[u8]) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut decoder = lz4_flex::frame::FrameDecoder::new(data);
    let mut decompressed = Vec::new();
    decoder
        .read_to_end(&mut decompressed)
        .context("Failed to decompress LZ4 data")?;
    Ok(decompressed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_decompress_lz4_roundtrip() {
        let original = b"Hello, LZ4 compression! This is a test of roundtrip compression.";

        // Compress
        let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
        std::io::Write::write_all(&mut encoder, original).unwrap();
        let compressed = encoder.finish().unwrap();

        // Decompress
        let decompressed = decompress_lz4(&compressed).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn test_compress_decompress_lz4_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();

        // Create some test files
        fs::write(part_dir.join("data.bin"), b"some binary data content").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"checksum1\nchecksum2\n").unwrap();

        // Compress with lz4
        let compressed = compress_part(&part_dir, "test_part", "lz4", 1).unwrap();
        assert!(!compressed.is_empty());

        // Decompress with lz4
        let output_dir = dir.path().join("output");
        decompress_part(&compressed, &output_dir, "lz4").unwrap();

        // Verify files
        let data = fs::read(output_dir.join("test_part/data.bin")).unwrap();
        assert_eq!(data, b"some binary data content");

        let checksums = fs::read(output_dir.join("test_part/checksums.txt")).unwrap();
        assert_eq!(checksums, b"checksum1\nchecksum2\n");
    }

    #[test]
    fn test_compress_decompress_zstd_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();

        fs::write(part_dir.join("data.bin"), b"zstd binary data content").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"zstd_checksum1\n").unwrap();

        // Compress with zstd
        let compressed = compress_part(&part_dir, "test_part", "zstd", 3).unwrap();
        assert!(!compressed.is_empty());

        // Decompress with zstd
        let output_dir = dir.path().join("output");
        decompress_part(&compressed, &output_dir, "zstd").unwrap();

        // Verify files
        assert_eq!(
            fs::read(output_dir.join("test_part/data.bin")).unwrap(),
            b"zstd binary data content"
        );
        assert_eq!(
            fs::read(output_dir.join("test_part/checksums.txt")).unwrap(),
            b"zstd_checksum1\n"
        );
    }

    #[test]
    fn test_compress_decompress_gzip_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();

        fs::write(part_dir.join("data.bin"), b"gzip binary data content").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"gzip_checksum1\n").unwrap();

        // Compress with gzip
        let compressed = compress_part(&part_dir, "test_part", "gzip", 6).unwrap();
        assert!(!compressed.is_empty());

        // Decompress with gzip
        let output_dir = dir.path().join("output");
        decompress_part(&compressed, &output_dir, "gzip").unwrap();

        // Verify files
        assert_eq!(
            fs::read(output_dir.join("test_part/data.bin")).unwrap(),
            b"gzip binary data content"
        );
        assert_eq!(
            fs::read(output_dir.join("test_part/checksums.txt")).unwrap(),
            b"gzip_checksum1\n"
        );
    }

    #[test]
    fn test_compress_decompress_none_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();

        fs::write(part_dir.join("data.bin"), b"uncompressed binary data").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"none_checksum1\n").unwrap();

        // Compress with none (raw tar)
        let compressed = compress_part(&part_dir, "test_part", "none", 0).unwrap();
        assert!(!compressed.is_empty());

        // Decompress with none
        let output_dir = dir.path().join("output");
        decompress_part(&compressed, &output_dir, "none").unwrap();

        // Verify files
        assert_eq!(
            fs::read(output_dir.join("test_part/data.bin")).unwrap(),
            b"uncompressed binary data"
        );
        assert_eq!(
            fs::read(output_dir.join("test_part/checksums.txt")).unwrap(),
            b"none_checksum1\n"
        );
    }

    #[test]
    fn test_decompress_part_file_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();
        fs::write(part_dir.join("data.bin"), b"file-path data").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"file-path checksums\n").unwrap();

        let compressed = compress_part(&part_dir, "test_part", "lz4", 1).unwrap();
        let archive_path = dir.path().join("part.tar.lz4");
        fs::write(&archive_path, compressed).unwrap();

        let output_dir = dir.path().join("output");
        decompress_part_file(&archive_path, &output_dir, "lz4").unwrap();

        assert_eq!(
            fs::read(output_dir.join("test_part/data.bin")).unwrap(),
            b"file-path data"
        );
        assert_eq!(
            fs::read(output_dir.join("test_part/checksums.txt")).unwrap(),
            b"file-path checksums\n"
        );
    }

    #[test]
    fn test_decompress_unknown_format_errors() {
        let dir = tempfile::tempdir().unwrap();
        let result = decompress_part(b"data", dir.path(), "brotli");
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("Unknown compression format: brotli"));
    }

    #[test]
    fn test_untar_to_directory() {
        // Create a tar+lz4 archive manually, then decompress
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src_part");
        fs::create_dir_all(&src_dir).unwrap();
        fs::write(src_dir.join("file1.txt"), b"content1").unwrap();
        fs::write(src_dir.join("file2.txt"), b"content2").unwrap();

        let sub_dir = src_dir.join("subdir");
        fs::create_dir_all(&sub_dir).unwrap();
        fs::write(sub_dir.join("nested.txt"), b"nested content").unwrap();

        // Create tar + lz4
        let compressed = compress_part(&src_dir, "src_part", "lz4", 1).unwrap();

        // Extract to new location
        let out_dir = dir.path().join("extracted");
        decompress_part(&compressed, &out_dir, "lz4").unwrap();

        // Verify all files
        assert_eq!(
            fs::read_to_string(out_dir.join("src_part/file1.txt")).unwrap(),
            "content1"
        );
        assert_eq!(
            fs::read_to_string(out_dir.join("src_part/file2.txt")).unwrap(),
            "content2"
        );
        assert_eq!(
            fs::read_to_string(out_dir.join("src_part/subdir/nested.txt")).unwrap(),
            "nested content"
        );
    }
}
