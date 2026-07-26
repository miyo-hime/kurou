// runs against a THROWAWAY copy of the live ledger - it writes junk rows into it.
// measures: insert latency, rss growth (per-insert / per-connection / idle), lock repro.
// cargo run --release --example leakprobe -- /tmp/probe.db

use std::time::{Duration, Instant};

fn rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: f64 = status.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    kb / 1024.0
}

fn junk_row(i: i64) -> String {
    format!("insert or ignore into messages (message_id, guild_id, channel_id, author_id, author_name, content, mention_ids, timestamp, payload) values ({}, 'g', 'probe-chan', 'probe', 'probe', 'leak probe filler message number {} with some words to index', '', '2026-07-26T00:00:00Z', '{{}}')", 9_000_000_000_000_000_000i64 - i, i)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: leakprobe <throwaway db copy>");
    println!("rss at start: {:.1}MB", rss_mb());

    let db = turso::Builder::new_local(&path).experimental_index_method(true).build().await?;
    println!("rss after open: {:.1}MB", rss_mb());

    // phase 1: single reused connection, timed inserts
    let conn = db.connect()?;
    for i in 0..10 {
        let t = Instant::now();
        conn.execute(&junk_row(i), ()).await?;
        println!("phase1 insert {i}: {:?}  rss {:.1}MB", t.elapsed(), rss_mb());
    }

    // phase 2: fresh connection per insert (the kurou production pattern)
    for i in 100..115 {
        let t = Instant::now();
        let conn = db.connect()?;
        let connected = t.elapsed();
        conn.execute(&junk_row(i), ()).await?;
        println!("phase2 insert {i}: connect {connected:?} total {:?}  rss {:.1}MB", t.elapsed(), rss_mb());
    }

    // phase 3: one fts search per fresh connection
    for i in 0..5 {
        let t = Instant::now();
        let conn = db.connect()?;
        let mut rows = conn.query("select message_id from messages where fts_match(content, 'database') limit 5", ()).await?;
        let mut n = 0;
        while let Some(_row) = rows.next().await? { n += 1; }
        println!("phase3 search {i}: {:?} ({n} hits)  rss {:.1}MB", t.elapsed(), rss_mb());
    }

    // phase 4: concurrent connect-per-op inserts - the lock repro
    let mut tasks = Vec::new();
    for i in 200..210 {
        let db = db.clone();
        tasks.push(tokio::spawn(async move {
            let conn = db.connect()?;
            conn.execute(&junk_row(i), ()).await.map(|_| ())
        }));
    }
    let mut locked = 0;
    for task in tasks {
        if let Err(error) = task.await? {
            let text = format!("{error:#}");
            if text.contains("locked") { locked += 1; } else { println!("phase4 other error: {text}"); }
        }
    }
    println!("phase4 concurrent x10: {locked} locked errors  rss {:.1}MB", rss_mb());

    // phase 5: idle - does rss move with zero traffic?
    for i in 0..6 {
        tokio::time::sleep(Duration::from_secs(30)).await;
        println!("phase5 idle {}s: rss {:.1}MB", (i + 1) * 30, rss_mb());
    }

    Ok(())
}
