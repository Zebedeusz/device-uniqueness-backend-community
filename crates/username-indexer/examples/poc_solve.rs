//! Solve a proof-of-compute puzzle and print the ready-to-send header value.
//!
//! Deliberately dependency-free: pipe the issuance response in rather than
//! adding an HTTP client to this lean read service.
//!
//! ```text
//! curl -sX POST http://localhost:8081/api/v1/poc/issue \
//!   | cargo run -p username-indexer --example poc_solve
//! ```
//!
//! Then replay the printed header against search:
//!
//! ```text
//! curl -sD- -H "Proof-Of-Compute: <printed>" \
//!   'http://localhost:8081/api/v1/usernames/search?prefix=a'
//! ```

use std::io::Read as _;
use std::time::Instant;

use serde::Deserialize;
use username_indexer::poc::solution::mine;
use username_indexer::poc::Solution;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssuedPuzzle {
    session_id: uuid::Uuid,
    timestamp: i64,
    difficulty: u8,
    checksum: String,
}

fn main() -> anyhow::Result<()> {
    let mut body = String::new();
    std::io::stdin().read_to_string(&mut body)?;
    let puzzle: IssuedPuzzle = serde_json::from_str(body.trim())?;

    let started = Instant::now();
    let counter = mine(puzzle.session_id, puzzle.timestamp, puzzle.difficulty);
    let elapsed = started.elapsed();

    let solution = Solution::new(
        puzzle.session_id,
        puzzle.timestamp,
        puzzle.difficulty,
        counter,
        puzzle.checksum,
    );

    eprintln!(
        "mined {} bits in {:.3}s ({counter} hashes)",
        puzzle.difficulty,
        elapsed.as_secs_f64()
    );
    println!("{}", solution.to_header());
    Ok(())
}
