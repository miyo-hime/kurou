// the exorcism test: drop the fts index from a throwaway ledger copy, measure the after.
// cargo run --release --example dropprobe -- /tmp/probe3.db

use std::time::Instant;

fn rss_mb() -> f64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let kb: f64 = status.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0.0);
    kb / 1024.0
}

fn junk_row(i: i64) -> String {
    format!("insert or ignore into messages (message_id, guild_id, channel_id, author_id, author_name, content, mention_ids, timestamp, payload) values ({}, 'g', 'probe-chan', 'probe', 'probe', 'drop probe filler message number {} with some words', '', '2026-07-26T00:00:00Z', '{{}}')", 9_200_000_000_000_000_000i64 - i, i)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: dropprobe <throwaway db copy>");
    let db = turso::Builder::new_local(&path).experimental_index_method(true).experimental_vacuum(true).build().await?;
    let conn = db.connect()?;

    let t = Instant::now();
    conn.execute("drop index if exists msg_fts", ()).await?;
    println!("drop index: {:?}  rss {:.1}MB", t.elapsed(), rss_mb());

    for i in 0..5 {
        let t = Instant::now();
        conn.execute(&junk_row(i), ()).await?;
        println!("insert after drop: {:?}  rss {:.1}MB", t.elapsed(), rss_mb());
    }

    let t = Instant::now();
    let mut rows = conn.query("select message_id, content from messages where content like '%database%' order by message_id desc limit 5", ()).await?;
    let mut n = 0;
    while let Some(_row) = rows.next().await? { n += 1; }
    println!("like search after drop: {:?} ({n} hits)  rss {:.1}MB", t.elapsed(), rss_mb());

    let mut rows = conn.query("select name from sqlite_master where name like '%fts%'", ()).await?;
    let mut leftovers = Vec::new();
    while let Some(row) = rows.next().await? {
        if let turso::Value::Text(name) = row.get_value(0)? { leftovers.push(name); }
    }
    println!("fts leftovers in sqlite_master: {leftovers:?}");

    let t = Instant::now();
    match conn.execute("vacuum", ()).await {
        Ok(_) => println!("vacuum: {:?}", t.elapsed()),
        Err(error) => println!("vacuum failed: {error:#}"),
    }

    println!("file size after: {} bytes", std::fs::metadata(&path)?.len());
    Ok(())
}
