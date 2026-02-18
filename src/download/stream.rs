//! Decompression and extraction utilities for downloaded backup parts.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

/// Decompress an LZ4-compressed tar archive and extract to the output directory.
///
/// The `data` is expected to be a tar archive compressed with LZ4 frame format.
/// Files are extracted into `output_dir`, preserving the directory structure
/// from the tar archive.
///
/// This function runs synchronously and should be called within
/// `tokio::task::spawn_blocking`.
pub fn decompress_part(data: &[u8], output_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("Failed to create output dir: {}", output_dir.display()))?;

    let decoder = lz4_flex::frame::FrameDecoder::new(data);
    let mut archive = tar::Archive::new(decoder);
    archive
        .unpack(output_dir)
        .with_context(|| format!("Failed to unpack tar archive to: {}", output_dir.display()))?;

    Ok(())
}

/// Compress a directory into an LZ4-compressed tar archive in memory.
///
/// Creates a tar archive of `part_dir` (using `archive_name` as the root directory
/// name inside the tar), then compresses it with LZ4 frame format.
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

/// Decompress raw LZ4 frame data to bytes.
pub fn decompress_lz4(data: &[u8]) -> Result<Vec<u8>> {
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
    fn test_compress_decompress_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();

        // Create some test files
        fs::write(part_dir.join("data.bin"), b"some binary data content").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"checksum1\nchecksum2\n").unwrap();

        // Compress
        let compressed = compress_part(&part_dir, "test_part").unwrap();
        assert!(!compressed.is_empty());

        // Decompress
        let output_dir = dir.path().join("output");
        decompress_part(&compressed, &output_dir).unwrap();

        // Verify files
        let data = fs::read(output_dir.join("test_part/data.bin")).unwrap();
        assert_eq!(data, b"some binary data content");

        let checksums = fs::read(output_dir.join("test_part/checksums.txt")).unwrap();
        assert_eq!(checksums, b"checksum1\nchecksum2\n");
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
        let compressed = compress_part(&src_dir, "src_part").unwrap();

        // Extract to new location
        let out_dir = dir.path().join("extracted");
        decompress_part(&compressed, &out_dir).unwrap();

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
