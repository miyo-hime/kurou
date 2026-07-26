// the cure test: OPTIMIZE INDEX on a throwaway ledger copy, insert latency before/after.
// cargo run --release --example optprobe -- /tmp/probe2.db

use std::time::Instant;

fn rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: f64 = status.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    kb / 1024.0
}

fn junk_row(i: i64) -> String {
    format!("insert or ignore into messages (message_id, guild_id, channel_id, author_id, author_name, content, mention_ids, timestamp, payload) values ({}, 'g', 'probe-chan', 'probe', 'probe', 'optimize probe filler message number {} with some words to index', '', '2026-07-26T00:00:00Z', '{{}}')", 9_100_000_000_000_000_000i64 - i, i)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: optprobe <throwaway db copy>");
    let db = turso::Builder::new_local(&path).experimental_index_method(true).build().await?;
    let conn = db.connect()?;

    let t = Instant::now();
    conn.execute(&junk_row(0), ()).await?;
    println!("insert before optimize: {:?}  rss {:.1}MB", t.elapsed(), rss_mb());

    let t = Instant::now();
    conn.execute("optimize index msg_fts", ()).await?;
    println!("OPTIMIZE INDEX: {:?}  rss {:.1}MB", t.elapsed(), rss_mb());

    for i in 1..6 {
        let t = Instant::now();
        conn.execute(&junk_row(i), ()).await?;
        println!("insert after optimize: {:?}  rss {:.1}MB", t.elapsed(), rss_mb());
    }

    let t = Instant::now();
    let mut rows = conn.query("select message_id from messages where fts_match(content, 'database') limit 5", ()).await?;
    let mut n = 0;
    while let Some(_row) = rows.next().await? { n += 1; }
    println!("search after optimize: {:?} ({n} hits)", t.elapsed());

    let data = std::fs::read(&path)?;
    println!("segment_id count in file: {}", data.windows(12).filter(|w| w == b"\"segment_id\"").count());
    Ok(())
}
