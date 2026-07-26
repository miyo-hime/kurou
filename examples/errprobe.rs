// correlates failed-insert snowflakes (journalctl dump) against the archive copy:
// how close was the nearest successful insert, and did the failed row ever land?
// cargo run --release --example errprobe -- /tmp/ledger-copy.db /tmp/kurou-errors.txt

const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

fn ms_of(id: i64) -> i64 { (id >> 22) + DISCORD_EPOCH_MS }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let db_path = std::env::args().nth(1).expect("db path");
    let err_path = std::env::args().nth(2).expect("errors file");

    let db = turso::Builder::new_local(&db_path).build().await?;
    let conn = db.connect()?;
    let mut rows = conn.query("select message_id from messages order by message_id", ()).await?;
    let mut stored = Vec::new();
    while let Some(row) = rows.next().await? {
        if let turso::Value::Integer(id) = row.get_value(0)? { stored.push(id); }
    }
    let stored_times: Vec<i64> = stored.iter().map(|&id| ms_of(id)).collect();

    let errors: Vec<(i64, i64)> = std::fs::read_to_string(&err_path)?
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
        })
        .collect();

    let mut landed_anyway = 0u64;
    let mut buckets = [0u64; 6]; // <200ms, <1s, <5s, <30s, <5min, lonelier
    for &(failed_id, _channel) in &errors {
        if stored.binary_search(&failed_id).is_ok() { landed_anyway += 1; continue; }
        let t = ms_of(failed_id);
        let idx = stored_times.partition_point(|&s| s < t);
        let before = idx.checked_sub(1).map(|i| t - stored_times[i]);
        let after = stored_times.get(idx).map(|&s| s - t);
        let nearest = [before, after].into_iter().flatten().min().unwrap_or(i64::MAX);
        let bucket = match nearest {
            n if n < 200 => 0,
            n if n < 1_000 => 1,
            n if n < 5_000 => 2,
            n if n < 30_000 => 3,
            n if n < 300_000 => 4,
            _ => 5,
        };
        buckets[bucket] += 1;
    }

    println!("errors: {} | later landed in db anyway: {landed_anyway}", errors.len());
    let labels = ["<200ms", "<1s", "<5s", "<30s", "<5min", ">=5min"];
    println!("nearest successful insert (arrival-time distance) for the lost ones:");
    for (label, count) in labels.iter().zip(buckets) {
        println!("  {label:>7}  {count}");
    }

    // error-vs-error clustering: do failures come in runs?
    let mut err_ids: Vec<i64> = errors.iter().map(|e| e.0).collect();
    err_ids.sort();
    let mut runs = 0u64;
    for pair in err_ids.windows(2) {
        if ms_of(pair[1]) - ms_of(pair[0]) < 5_000 { runs += 1; }
    }
    println!("error pairs within 5s of another error: {runs}");
    Ok(())
}
