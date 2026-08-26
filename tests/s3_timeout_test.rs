//! Proves a stalled S3 request fails instead of hanging.
//!
//! Reproduces the incident's exact shape: a TCP connect that **succeeds**, followed by a server
//! that never sends a response byte. That distinction matters -- the AWS SDK already applies a
//! 3.1s connect timeout, so a test that merely pointed at a closed port would pass without
//! proving anything about our deadlines. By accepting the connection and then going silent, the
//! only thing that can end the request is `s3.request_timeout`.
//!
//! Needs no network, no S3, no ClickHouse, and no new dev-dependencies.

use std::net::TcpListener;
use std::time::{Duration, Instant};

use chbackup::config::S3Config;
use chbackup::storage::s3::is_timeout_error;
use chbackup::storage::S3Client;

/// Bind an ephemeral port and accept connections without ever replying.
///
/// The accepted sockets are parked in a `Vec` rather than dropped: dropping them would close the
/// connection and hand the SDK an error, which is precisely the outcome this test must avoid.
fn spawn_black_hole() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    let port = listener.local_addr().expect("local_addr").port();

    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            match stream {
                Ok(s) => held.push(s),
                Err(_) => break,
            }
        }
    });

    port
}

fn black_hole_config(port: u16, request_timeout: &str) -> S3Config {
    S3Config {
        bucket: "test-bucket".to_string(),
        region: "us-east-1".to_string(),
        endpoint: format!("http://127.0.0.1:{port}"),
        force_path_style: true,
        access_key: "test-access-key".to_string(),
        secret_key: "test-secret-key".to_string(),
        request_timeout: request_timeout.to_string(),
        ..Default::default()
    }
}

/// A bodyless request against a silent endpoint must fail on our deadline, not hang.
///
/// `request_timeout` is deliberately 2s -- below the SDK's 3.1s connect timeout -- so a pass
/// proves *our* deadline fired rather than the SDK's.
#[tokio::test]
async fn head_object_times_out_instead_of_hanging() {
    let port = spawn_black_hole();
    let client = S3Client::new(&black_hole_config(port, "2s"))
        .await
        .expect("client construction must not require a reachable endpoint");

    let started = Instant::now();
    let err = client
        .head_object("some/key")
        .await
        .expect_err("a silent endpoint must produce an error, not a hang");
    let elapsed = started.elapsed();

    assert!(
        is_timeout_error(&err),
        "should be classified as our deadline expiry, got: {err:#}"
    );
    assert!(
        elapsed < Duration::from_secs(6),
        "should give up promptly, took {elapsed:?}"
    );
    // Below the SDK's 3.1s connect timeout: proves our deadline fired, not the SDK's.
    assert!(
        elapsed < Duration::from_millis(3100),
        "our 2s deadline should fire before the SDK's 3.1s connect timeout, took {elapsed:?}"
    );
}

/// A listing must be bounded too -- it is the call that would otherwise park a killed restore.
#[tokio::test]
async fn list_objects_times_out_instead_of_hanging() {
    let port = spawn_black_hole();
    let client = S3Client::new(&black_hole_config(port, "2s"))
        .await
        .expect("client construction");

    let started = Instant::now();
    let err = client
        .list_objects("some/prefix/")
        .await
        .expect_err("a silent endpoint must produce an error");
    let elapsed = started.elapsed();

    assert!(is_timeout_error(&err), "got: {err:#}");
    assert!(elapsed < Duration::from_secs(6), "took {elapsed:?}");
}

/// The retried copy path must stay bounded in total, which is the property that prevents the
/// 22-hour worst case: N attempts each bounded, plus backoff, rather than one unbounded wait.
///
/// `allow_streaming = false` so the failure does not fall through to a streaming download.
#[tokio::test]
async fn copy_object_retries_stay_bounded() {
    let port = spawn_black_hole();
    let client = S3Client::new(&black_hole_config(port, "2s"))
        .await
        .expect("client construction");

    let started = Instant::now();
    let err = client
        .copy_object_with_retry("src-bucket", "src/key.bin", "dest/key.bin", false)
        .await
        .expect_err("a silent endpoint must produce an error");
    let elapsed = started.elapsed();

    assert!(
        is_timeout_error(&err),
        "the underlying cause should still be classified as a timeout, got: {err:#}"
    );
    // 3 attempts x 2s deadline, plus ~2.1s of fixed backoff, plus slack.
    assert!(
        elapsed < Duration::from_secs(20),
        "retries must be bounded in total, took {elapsed:?}"
    );
}

/// `request_timeout = "0s"` is the documented escape hatch that restores the old behaviour.
/// Verify it really does disable the deadline rather than being treated as "zero seconds".
#[tokio::test]
async fn zero_request_timeout_disables_the_deadline() {
    let port = spawn_black_hole();
    let client = S3Client::new(&black_hole_config(port, "0s"))
        .await
        .expect("client construction");

    // With deadlines off, the request must NOT fail on our timeout. It will eventually fail on
    // some SDK-level condition; the assertion is only that it is not our deadline, and a short
    // outer bound keeps the test from hanging if that ever stops being true.
    let outcome =
        tokio::time::timeout(Duration::from_secs(15), client.head_object("some/key")).await;

    match outcome {
        Err(_elapsed) => { /* still waiting after 15s: deadlines are indeed disabled */ }
        Ok(Ok(_)) => panic!("a silent endpoint cannot return success"),
        Ok(Err(e)) => assert!(
            !is_timeout_error(&e),
            "with request_timeout=0s the failure must not be our deadline, got: {e:#}"
        ),
    }
}
