//! Compression utilities for upload.
//!
//! Provides tar + compression for part directories before S3 upload.
//! Supports LZ4, zstd, gzip, and uncompressed (none) formats.
//!
//! Two modes are available:
//! - **Buffered** (`compress_part`): compresses entire part to `Vec<u8>` in memory
//! - **Streaming** (`compress_part_streaming`): produces fixed-size chunks via channel,
//!   suitable for streaming multipart upload of large parts (>256 MiB)

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result};

/// Minimum S3 multipart chunk size (5 MiB).
pub const MIN_MULTIPART_CHUNK: usize = 5 * 1024 * 1024;

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

/// Create a tar archive of `part_dir` into `writer`, using `archive_name` as the
/// root directory name inside the archive.
///
/// This is the shared tar-creation logic used by all buffered compression functions.
/// The writer is borrowed mutably, so the caller retains ownership and can finalize
/// any encoder wrapping it after this returns.
fn tar_into_writer<W: std::io::Write>(
    writer: &mut W,
    part_dir: &Path,
    archive_name: &str,
) -> Result<()> {
    let mut tar_builder = tar::Builder::new(writer);
    tar_builder
        .append_dir_all(archive_name, part_dir)
        .with_context(|| format!("Failed to add directory to tar: {}", part_dir.display()))?;
    tar_builder
        .finish()
        .context("Failed to finish tar archive")?;
    Ok(())
}

/// Compress with LZ4 frame format.
fn compress_lz4(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>> {
    let mut encoder = lz4_flex::frame::FrameEncoder::new(Vec::new());
    tar_into_writer(&mut encoder, part_dir, archive_name)?;
    let compressed = encoder
        .finish()
        .context("Failed to finish LZ4 compression")?;
    Ok(compressed)
}

/// Compress with Zstandard format.
fn compress_zstd(part_dir: &Path, archive_name: &str, compression_level: u32) -> Result<Vec<u8>> {
    let level = compression_level.min(22) as i32;
    let mut encoder =
        zstd::Encoder::new(Vec::new(), level).context("Failed to create zstd encoder")?;
    tar_into_writer(&mut encoder, part_dir, archive_name)?;
    let compressed = encoder
        .finish()
        .context("Failed to finish zstd compression")?;
    Ok(compressed)
}

/// Compress with gzip format.
fn compress_gzip(part_dir: &Path, archive_name: &str, compression_level: u32) -> Result<Vec<u8>> {
    let mut encoder =
        flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(compression_level));
    tar_into_writer(&mut encoder, part_dir, archive_name)?;
    encoder.flush().context("Failed to flush gzip encoder")?;
    let compressed = encoder
        .finish()
        .context("Failed to finish gzip compression")?;
    Ok(compressed)
}

/// No compression -- raw tar only.
fn compress_none(part_dir: &Path, archive_name: &str) -> Result<Vec<u8>> {
    let mut buffer = Vec::new();
    tar_into_writer(&mut buffer, part_dir, archive_name)?;
    Ok(buffer)
}

/// A writer that buffers bytes and sends fixed-size chunks through a channel.
///
/// When the internal buffer reaches `chunk_size` bytes, the full chunk is sent
/// through the `mpsc::Sender`. On `flush()` or `Drop`, any remaining bytes in the
/// buffer are sent as the final (possibly smaller) chunk.
struct ChunkedWriter {
    buffer: Vec<u8>,
    chunk_size: usize,
    sender: mpsc::Sender<Result<Vec<u8>>>,
}

impl ChunkedWriter {
    fn new(chunk_size: usize, sender: mpsc::Sender<Result<Vec<u8>>>) -> Self {
        Self {
            buffer: Vec::with_capacity(chunk_size),
            chunk_size,
            sender,
        }
    }

    /// Send the current buffer as a chunk and reset.
    fn send_buffer(&mut self) -> std::io::Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buffer, Vec::with_capacity(self.chunk_size));
        self.sender
            .send(Ok(chunk))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "receiver dropped"))
    }
}

impl Write for ChunkedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut written = 0;
        while written < buf.len() {
            let remaining_capacity = self.chunk_size - self.buffer.len();
            let to_copy = std::cmp::min(remaining_capacity, buf.len() - written);
            self.buffer
                .extend_from_slice(&buf[written..written + to_copy]);
            written += to_copy;

            if self.buffer.len() >= self.chunk_size {
                self.send_buffer()?;
            }
        }
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.send_buffer()
    }
}

impl Drop for ChunkedWriter {
    fn drop(&mut self) {
        // Best-effort send of remaining data
        let _ = self.send_buffer();
    }
}

/// Streaming compression: produces chunks suitable for multipart upload.
///
/// Spawns a background thread that tars+compresses `part_dir` and sends
/// fixed-size chunks (at least 5MB each for S3 multipart) via a channel.
/// Returns a receiver that yields `Vec<u8>` chunks.
///
/// The `chunk_size` parameter controls how large each chunk is. It must be
/// at least `MIN_MULTIPART_CHUNK` (5 MiB) for S3 multipart compatibility.
///
/// This function spawns a `std::thread` internally. The receiver is consumed
/// by async code via `spawn_blocking` or by iterating in a blocking context.
///
/// # Errors
///
/// Returns an error if `chunk_size` is less than `MIN_MULTIPART_CHUNK`.
/// Compression/tar errors are sent through the channel as `Err` values.
pub fn compress_part_streaming(
    part_dir: &Path,
    archive_name: &str,
    data_format: &str,
    compression_level: u32,
    chunk_size: usize,
) -> Result<mpsc::Receiver<Result<Vec<u8>>>> {
    if chunk_size < MIN_MULTIPART_CHUNK {
        anyhow::bail!(
            "chunk_size ({}) must be at least MIN_MULTIPART_CHUNK ({} bytes)",
            chunk_size,
            MIN_MULTIPART_CHUNK
        );
    }

    let (sender, receiver) = mpsc::channel();

    // Clone owned data for the spawned thread
    let part_dir_owned: PathBuf = part_dir.to_path_buf();
    let archive_name_owned: String = archive_name.to_string();
    let data_format_owned: String = data_format.to_string();

    std::thread::spawn(move || {
        let result = streaming_compress_inner(
            &part_dir_owned,
            &archive_name_owned,
            &data_format_owned,
            compression_level,
            chunk_size,
            &sender,
        );

        if let Err(e) = result {
            // Send the error through the channel so the receiver can observe it
            let _ = sender.send(Err(e));
        }
        // sender is dropped here, closing the channel
    });

    Ok(receiver)
}

/// Inner function that performs the actual tar+compress into a `ChunkedWriter`.
///
/// Creates the appropriate compressor wrapping a `ChunkedWriter`, builds a tar
/// archive, and finalizes both the tar and compressor. Chunks are sent through
/// the channel as the compressor flushes its output.
fn streaming_compress_inner(
    part_dir: &Path,
    archive_name: &str,
    data_format: &str,
    compression_level: u32,
    chunk_size: usize,
    sender: &mpsc::Sender<Result<Vec<u8>>>,
) -> Result<()> {
    // NOTE: Each match arm cannot fully share tar_into_writer because the encoder
    // types (FrameEncoder, zstd::Encoder, GzEncoder, ChunkedWriter) each have
    // different finalization methods (.finish() returning different types) and
    // require different post-tar cleanup. The tar creation itself is shared via
    // tar_into_writer, but each arm must still handle encoder-specific finalization
    // and flushing the final chunk through the ChunkedWriter.
    match data_format {
        "lz4" => {
            let chunked = ChunkedWriter::new(chunk_size, sender.clone());
            let mut encoder = lz4_flex::frame::FrameEncoder::new(chunked);
            tar_into_writer(&mut encoder, part_dir, archive_name)?;
            let mut chunked = encoder
                .finish()
                .context("Failed to finish LZ4 compression")?;
            chunked.flush().context("Failed to flush final chunk")?;
            Ok(())
        }
        "zstd" => {
            let chunked = ChunkedWriter::new(chunk_size, sender.clone());
            let level = compression_level.min(22) as i32;
            let mut encoder =
                zstd::Encoder::new(chunked, level).context("Failed to create zstd encoder")?;
            tar_into_writer(&mut encoder, part_dir, archive_name)?;
            let mut chunked = encoder
                .finish()
                .context("Failed to finish zstd compression")?;
            chunked.flush().context("Failed to flush final chunk")?;
            Ok(())
        }
        "gzip" => {
            let chunked = ChunkedWriter::new(chunk_size, sender.clone());
            let mut encoder =
                flate2::write::GzEncoder::new(chunked, flate2::Compression::new(compression_level));
            tar_into_writer(&mut encoder, part_dir, archive_name)?;
            encoder.flush().context("Failed to flush gzip encoder")?;
            let mut chunked = encoder
                .finish()
                .context("Failed to finish gzip compression")?;
            chunked.flush().context("Failed to flush final chunk")?;
            Ok(())
        }
        "none" => {
            let mut chunked = ChunkedWriter::new(chunk_size, sender.clone());
            tar_into_writer(&mut chunked, part_dir, archive_name)?;
            chunked.flush().context("Failed to flush final chunk")?;
            Ok(())
        }
        other => Err(anyhow::anyhow!("Unknown compression format: {}", other)),
    }
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

    #[test]
    fn test_compress_part_streaming_roundtrip() {
        // Create a temp directory with test data
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("stream_part");
        fs::create_dir_all(part_dir.join("subdir")).unwrap();

        fs::write(part_dir.join("file1.txt"), b"streaming content one").unwrap();
        fs::write(part_dir.join("subdir/file2.txt"), b"streaming content two").unwrap();
        // Write a larger file to ensure multi-chunk behavior
        let large_data: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();
        fs::write(part_dir.join("large.bin"), &large_data).unwrap();

        // Use the streaming path with the minimum chunk size
        let rx = compress_part_streaming(&part_dir, "stream_part", "lz4", 1, MIN_MULTIPART_CHUNK)
            .unwrap();

        // Collect all chunks
        let mut all_compressed = Vec::new();
        for chunk_result in rx {
            let chunk = chunk_result.unwrap();
            all_compressed.extend_from_slice(&chunk);
        }
        assert!(!all_compressed.is_empty());

        // Decompress and verify data integrity
        let decoder = lz4_flex::frame::FrameDecoder::new(all_compressed.as_slice());
        let mut archive = tar::Archive::new(decoder);
        let output_dir = dir.path().join("streaming_output");
        archive.unpack(&output_dir).unwrap();

        assert_eq!(
            fs::read_to_string(output_dir.join("stream_part/file1.txt")).unwrap(),
            "streaming content one"
        );
        assert_eq!(
            fs::read_to_string(output_dir.join("stream_part/subdir/file2.txt")).unwrap(),
            "streaming content two"
        );
        assert_eq!(
            fs::read(output_dir.join("stream_part/large.bin")).unwrap(),
            large_data
        );
    }

    #[test]
    fn test_compress_part_streaming_chunk_sizes() {
        // Create temp data large enough to produce multiple chunks at MIN_MULTIPART_CHUNK size
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("chunk_test_part");
        fs::create_dir_all(&part_dir).unwrap();

        // Write ~15 MiB of data (should produce multiple 5 MiB chunks)
        let data: Vec<u8> = (0..15_000_000).map(|i| (i % 256) as u8).collect();
        fs::write(part_dir.join("big_file.bin"), &data).unwrap();

        let rx = compress_part_streaming(
            &part_dir,
            "chunk_test_part",
            "none", // no compression so output is larger than input (tar overhead)
            0,
            MIN_MULTIPART_CHUNK,
        )
        .unwrap();

        let chunks: Vec<Vec<u8>> = rx.into_iter().map(|r| r.unwrap()).collect();

        // Should have multiple chunks
        assert!(
            chunks.len() > 1,
            "Expected multiple chunks, got {}",
            chunks.len()
        );

        // All chunks except the last must be at least MIN_MULTIPART_CHUNK bytes
        for (i, chunk) in chunks.iter().enumerate() {
            if i < chunks.len() - 1 {
                assert!(
                    chunk.len() >= MIN_MULTIPART_CHUNK,
                    "Chunk {} has {} bytes, expected at least {} bytes",
                    i,
                    chunk.len(),
                    MIN_MULTIPART_CHUNK
                );
            }
        }

        // Verify all chunks except last are exactly chunk_size
        for (i, chunk) in chunks.iter().enumerate() {
            if i < chunks.len() - 1 {
                assert_eq!(
                    chunk.len(),
                    MIN_MULTIPART_CHUNK,
                    "Non-final chunk {} should be exactly {} bytes, got {}",
                    i,
                    MIN_MULTIPART_CHUNK,
                    chunk.len()
                );
            }
        }

        // Last chunk can be smaller
        let last = chunks.last().unwrap();
        assert!(!last.is_empty(), "Last chunk should not be empty");
    }

    #[test]
    fn test_compress_part_streaming_chunk_size_too_small() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("test_part");
        fs::create_dir_all(&part_dir).unwrap();
        fs::write(part_dir.join("data.bin"), b"test").unwrap();

        // Should fail with chunk_size below minimum
        let result = compress_part_streaming(&part_dir, "test_part", "lz4", 1, 1024);
        assert!(result.is_err());
        let err = format!("{}", result.unwrap_err());
        assert!(err.contains("chunk_size"));
        assert!(err.contains("MIN_MULTIPART_CHUNK"));
    }

    #[test]
    fn test_compress_part_streaming_zstd_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let part_dir = dir.path().join("zstd_stream_part");
        fs::create_dir_all(&part_dir).unwrap();

        fs::write(part_dir.join("data.bin"), b"zstd streaming test data").unwrap();
        fs::write(part_dir.join("checksums.txt"), b"checksum for zstd stream").unwrap();

        let rx = compress_part_streaming(
            &part_dir,
            "zstd_stream_part",
            "zstd",
            3,
            MIN_MULTIPART_CHUNK,
        )
        .unwrap();

        let mut all_compressed = Vec::new();
        for chunk_result in rx {
            let chunk = chunk_result.unwrap();
            all_compressed.extend_from_slice(&chunk);
        }

        // Decompress and verify
        let decoder = zstd::Decoder::new(all_compressed.as_slice()).unwrap();
        let mut archive = tar::Archive::new(decoder);
        let output_dir = dir.path().join("zstd_streaming_output");
        archive.unpack(&output_dir).unwrap();

        assert_eq!(
            fs::read(output_dir.join("zstd_stream_part/data.bin")).unwrap(),
            b"zstd streaming test data"
        );
        assert_eq!(
            fs::read(output_dir.join("zstd_stream_part/checksums.txt")).unwrap(),
            b"checksum for zstd stream"
        );
    }
}
