// autopsy probe: reads a *copy* of the live ledger, prints volume + burst stats.
// cargo run --release --example dbprobe -- /tmp/ledger-copy.db

use std::collections::HashMap;

const DISCORD_EPOCH_MS: i64 = 1_420_070_400_000;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: dbprobe <db path>");
    let db = turso::Builder::new_local(&path).build().await?;
    let conn = db.connect()?;

    let mut rows = conn.query("select message_id, channel_id from messages order by message_id", ()).await?;
    let mut times = Vec::new();
    let mut per_day: HashMap<String, u64> = HashMap::new();
    let mut per_channel: HashMap<String, u64> = HashMap::new();

    while let Some(row) = rows.next().await? {
        let id = match row.get_value(0)? { turso::Value::Integer(v) => v, other => anyhow::bail!("weird pk: {other:?}") };
        let channel = match row.get_value(1)? { turso::Value::Text(v) => v, other => anyhow::bail!("weird channel: {other:?}") };
        let ms = (id >> 22) + DISCORD_EPOCH_MS;
        times.push(ms);
        let day = day_of(ms);
        *per_day.entry(day).or_default() += 1;
        *per_channel.entry(channel).or_default() += 1;
    }

    println!("total rows: {}", times.len());

    let mut days: Vec<_> = per_day.into_iter().collect();
    days.sort();
    println!("\nrows per day (utc):");
    for (day, count) in days.iter().rev().take(14).rev() {
        println!("  {day}  {count}");
    }

    let mut channels: Vec<_> = per_channel.into_iter().collect();
    channels.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
    println!("\ntop channels:");
    for (channel, count) in channels.iter().take(10) {
        println!("  {channel}  {count}");
    }

    // how often do two archived messages land inside one fts-commit window?
    // rough collision-rate check against the observed lock-error count.
    for window in [100i64, 200, 500, 1000] {
        let mut bursts_total = 0u64;
        let mut bursts_recent: HashMap<String, u64> = HashMap::new();
        for pair in times.windows(2) {
            if pair[1] - pair[0] < window {
                bursts_total += 1;
                *bursts_recent.entry(day_of(pair[1])).or_default() += 1;
            }
        }
        let mut recent: Vec<_> = bursts_recent.into_iter().collect();
        recent.sort();
        let tail: Vec<String> = recent.iter().rev().take(4).rev().map(|(d, c)| format!("{d}:{c}")).collect();
        println!("\ngaps < {window}ms: {bursts_total} total | last days: {}", tail.join(" "));
    }

    Ok(())
}

fn day_of(ms: i64) -> String {
    let days = ms / 86_400_000;
    // civil-from-days, howard hinnant's algorithm
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { year + 1 } else { year };
    format!("{year:04}-{month:02}-{day:02}")
}
