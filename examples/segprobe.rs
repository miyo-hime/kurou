// counts tantivy files inside turso's internal fts dir table - the segment bloat check.
// cargo run --release --example segprobe -- /tmp/ledger-copy.db

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("db path");
    let db = turso::Builder::new_local(&path).experimental_index_method(true).build().await?;
    let conn = db.connect()?;

    let mut rows = conn.query("select name from sqlite_master where name like '%fts%'", ()).await?;
    let mut tables = Vec::new();
    while let Some(row) = rows.next().await? {
        if let turso::Value::Text(name) = row.get_value(0)? { tables.push(name); }
    }
    println!("fts-ish objects in sqlite_master: {tables:?}");

    for table in &tables {
        if !table.starts_with("__turso_internal_fts_dir") { continue; }
        let mut rows = conn.query(&format!("select count(*) from \"{table}\""), ()).await?;
        if let Some(row) = rows.next().await? { println!("{table}: {:?} rows", row.get_value(0)?); }
        let mut rows = conn.query(&format!("select * from \"{table}\" limit 3"), ()).await?;
        while let Some(row) = rows.next().await? {
            let cells: Vec<String> = (0..8).filter_map(|i| row.get_value(i).ok()).map(|v| { let s = format!("{v:?}"); if s.len() > 80 { s[..80].to_string() } else { s } }).collect();
            println!("  sample: {cells:?}");
        }
    }
    Ok(())
}
