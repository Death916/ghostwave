//! Flood throughput integration benchmark test.
//! Measures the throughput of spawning a command emitting millions of lines
//! and draining it via the background PTY reader.

use ghostwave::pty::{Pty, PtyMsg, WinSize};
use std::time::Instant;

#[test]
fn seq_2m_flood_throughput() {
    let size = WinSize {
        cols: 120,
        rows: 40,
        xpixel: 1200,
        ypixel: 800,
    };

    let start = Instant::now();
    let (pty, child) = Pty::new(size, Some("/bin/sh"), &["-c", "seq 1 2000000"], None)
        .expect("failed to spawn PTY child");

    let rx = pty.spawn_reader().expect("failed to spawn reader thread");

    let mut total_bytes = 0usize;
    let mut chunks = 0usize;

    loop {
        match rx.recv() {
            Ok(PtyMsg::Data(data)) => {
                total_bytes += data.len();
                chunks += 1;
            }
            Ok(PtyMsg::Eof) => break,
            Ok(PtyMsg::Error(e)) => panic!("PTY error during flood: {e}"),
            Err(_) => break,
        }
    }

    let elapsed = start.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();
    let mb = total_bytes as f64 / (1024.0 * 1024.0);
    let mb_per_sec = mb / elapsed_secs;

    println!(
        "\n--- Flood Benchmark Results ---\nTotal data: {:.2} MB ({} bytes in {} chunks)\nElapsed time: {:.3}s ({:.2} ms)\nThroughput: {:.2} MB/s\n-------------------------------",
        mb, total_bytes, chunks, elapsed_secs, elapsed.as_secs_f64() * 1000.0, mb_per_sec
    );

    drop(child);

    // Must be at least 14.5 MB for 2 million numbers
    assert!(
        total_bytes >= 14_000_000,
        "expected >= 14MB, got {total_bytes} bytes"
    );

    // Ensure throughput does not regress (must finish under 1.5s)
    assert!(
        elapsed_secs < 1.5,
        "flood throughput regression: took {:.3}s (expected < 1.5s)",
        elapsed_secs
    );
}
