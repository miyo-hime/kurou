use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::archive::{NewMessage, ScanQuery};
use crate::discord::types::{
    RenderedAttachment, RenderedEmbed, RenderedMessage, RenderedReaction, RenderedReply, RenderedSticker,
};
use crate::ledger::Ledger;

struct TestDb {
    root: PathBuf,
    path: PathBuf,
}

impl TestDb {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("kurou-archive-contract-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("archive.db");
        Self { root, path }
    }

    fn with_missing_parents() -> Self {
        let root = std::env::temp_dir().join(format!("kurou-archive-contract-{}", uuid::Uuid::new_v4()));
        let path = root.join("missing").join("parents").join("archive.db");
        Self { root, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn message(id: i64, channel: &str, author: &str, content: &str, mentions: &[&str]) -> NewMessage {
    NewMessage {
        rendered: RenderedMessage {
            id: id.to_string(),
            author_id: author.to_owned(),
            author_name: format!("name-{author}"),
            timestamp: format!("2026-07-01T00:00:{:02}Z", id.rem_euclid(60)),
            edited_timestamp: None,
            reply: None,
            reactions: Vec::new(),
            attachments: Vec::new(),
            stickers: Vec::new(),
            embeds: Vec::new(),
            content: content.to_owned(),
        },
        guild_id: Some(format!("guild-{channel}")),
        channel_id: channel.to_owned(),
        mention_ids: mentions.iter().map(|id| (*id).to_owned()).collect(),
    }
}

fn ids(messages: &[RenderedMessage]) -> Vec<&str> {
    messages.iter().map(|message| message.id.as_str()).collect()
}

#[tokio::test]
async fn open_creates_every_missing_parent_directory() {
    let db = TestDb::with_missing_parents();
    assert!(!db.path().parent().unwrap().exists());
    Ledger::open(db.path()).await.unwrap();
    assert!(db.path().is_file());
}

#[tokio::test]
async fn reopen_preserves_preexisting_rows() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    store.insert(message(101, "chan", "crow", "kept across reopening", &[])).await.unwrap();
    drop(store);

    let reopened = Ledger::open(db.path()).await.unwrap().archive();
    let hits = reopened.search("kept", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].message_id, "101");
}

#[tokio::test]
async fn non_numeric_message_id_is_an_error_without_a_panicking_task() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    let mut invalid = message(1, "chan", "crow", "invalid snowflake", &[]);
    invalid.rendered.id = "not-a-number".to_owned();

    let outcome = tokio::spawn(async move { store.insert(invalid).await }).await;
    let insertion = outcome.expect("insert task panicked");
    assert!(insertion.is_err());
}

#[tokio::test]
async fn duplicate_id_does_not_overwrite_the_original_message() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    assert!(store.insert(message(101, "chan", "original", "first immutable text", &[])).await.unwrap());
    assert!(!store.insert(message(101, "other", "intruder", "replacement text", &["55"])).await.unwrap());

    let hits = store.search("immutable", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].channel_id, "chan");
    assert_eq!(hits[0].author_id, "original");
    assert_eq!(hits[0].content, "first immutable text");
    assert!(store.search("replacement", 10).await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_inserts_against_one_database_all_succeed() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    let starting_line = Arc::new(tokio::sync::Barrier::new(49));
    let tasks = (0..48)
        .map(|index| {
            let store = store.clone();
            let starting_line = starting_line.clone();
            let content = format!("parallel row {index}");
            let message = message(10_000 + index, "chan", "crow", &content, &[]);
            tokio::spawn(async move {
                starting_line.wait().await;
                store.insert(message).await
            })
        })
        .collect::<Vec<_>>();
    starting_line.wait().await;

    for task in tasks {
        assert!(task.await.expect("parallel insert task panicked").expect("parallel insert failed"));
    }
}

#[tokio::test]
async fn search_is_ascii_case_insensitive() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    store.insert(message(101, "chan", "crow", "DaTaBaSe CROW", &[])).await.unwrap();

    assert_eq!(store.search("database crow", 10).await.unwrap().len(), 1);
    assert_eq!(store.search("DATABASE CROW", 10).await.unwrap().len(), 1);
}

#[tokio::test]
async fn search_treats_operator_words_as_literals_and_accepts_punctuation_only_queries() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    store.insert(message(101, "chan", "crow", "alpha beta", &[])).await.unwrap();
    store.insert(message(202, "chan", "crow", "alpha OR beta", &[])).await.unwrap();

    let literal_operator = store.search("alpha OR beta", 10).await.unwrap();
    assert_eq!(literal_operator.iter().map(|hit| hit.message_id.as_str()).collect::<Vec<_>>(), ["202"]);

    for query in ["*", "-", "\"", "()", ":", "^", "NEAR(", "foo:", "{bar}"] {
        assert!(store.search(query, 10).await.is_ok(), "query {query:?} was parsed as syntax");
    }
}

#[tokio::test]
async fn empty_and_whitespace_only_searches_error_cleanly() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();

    assert!(store.search("", 10).await.is_err());
    assert!(store.search(" \t\n ", 10).await.is_err());
}

#[tokio::test]
async fn search_orders_by_descending_id_and_honors_limit() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    for id in [200, 100, 400, 300] {
        store.insert(message(id, "chan", "crow", "shared searchable token", &[])).await.unwrap();
    }

    let hits = store.search("search", 2).await.unwrap();
    assert_eq!(hits.iter().map(|hit| hit.message_id.as_str()).collect::<Vec<_>>(), ["400", "300"]);
}

#[tokio::test]
async fn search_hit_contains_every_stored_projection_field() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    let mut original = message(101, "channel-7", "author-8", "projection needle", &[]);
    original.guild_id = Some("guild-9".to_owned());
    original.rendered.author_name = "The Crow".to_owned();
    original.rendered.timestamp = "2026-08-09T10:11:12.000Z".to_owned();
    store.insert(original).await.unwrap();

    let hits = store.search("needle", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    let hit = &hits[0];
    assert_eq!(hit.message_id, "101");
    assert_eq!(hit.guild_id.as_deref(), Some("guild-9"));
    assert_eq!(hit.channel_id, "channel-7");
    assert_eq!(hit.author_id, "author-8");
    assert_eq!(hit.author_name, "The Crow");
    assert_eq!(hit.content, "projection needle");
    assert_eq!(hit.timestamp, "2026-08-09T10:11:12.000Z");
}

#[tokio::test]
async fn scan_never_leaks_other_channels_and_floor_ignores_filters() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    store.insert(message(100, "wanted", "other-author", "old floor", &[])).await.unwrap();
    store.insert(message(200, "wanted", "selected", "visible", &[])).await.unwrap();
    store.insert(message(50, "foreign", "selected", "must not leak", &[])).await.unwrap();

    let scan = store
        .scan(ScanQuery {
            channel_id: "wanted".into(),
            author_ids: vec!["selected".into()],
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ids(&scan.matches), ["200"]);
    assert_eq!(scan.floor, Some(100));
}

#[tokio::test]
async fn scan_author_filter_matches_any_listed_id() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    store.insert(message(100, "chan", "a", "one", &[])).await.unwrap();
    store.insert(message(200, "chan", "b", "two", &[])).await.unwrap();
    store.insert(message(300, "chan", "c", "three", &[])).await.unwrap();

    let scan = store
        .scan(ScanQuery {
            channel_id: "chan".into(),
            author_ids: vec!["a".into(), "b".into()],
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ids(&scan.matches), ["200", "100"]);
}

#[tokio::test]
async fn scan_mention_filter_matches_any_listed_exact_id() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    store.insert(message(100, "chan", "crow", "only substring", &["55"])).await.unwrap();
    store.insert(message(200, "chan", "crow", "first exact", &["5"])).await.unwrap();
    store.insert(message(300, "chan", "crow", "second exact", &["77"])).await.unwrap();
    store.insert(message(400, "chan", "crow", "unmentioned", &[])).await.unwrap();

    let scan = store
        .scan(ScanQuery {
            channel_id: "chan".into(),
            mention_ids: vec!["5".into(), "77".into()],
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    assert_eq!(ids(&scan.matches), ["300", "200"]);
}

#[tokio::test]
async fn scan_text_is_ascii_case_insensitive() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    store.insert(message(101, "chan", "crow", "Mixed CASE Needle", &[])).await.unwrap();

    let scan = store
        .scan(ScanQuery { channel_id: "chan".into(), text: Some("case needle".into()), limit: 10, ..Default::default() })
        .await
        .unwrap();
    assert_eq!(ids(&scan.matches), ["101"]);
}

#[tokio::test]
async fn scan_text_treats_like_wildcards_as_literal_text() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    store.insert(message(100, "chan", "crow", "ordinary prose", &[])).await.unwrap();
    store.insert(message(200, "chan", "crow", "100% ready", &[])).await.unwrap();
    store.insert(message(300, "chan", "crow", "under_score", &[])).await.unwrap();

    let percent = store
        .scan(ScanQuery { channel_id: "chan".into(), text: Some("%".into()), limit: 10, ..Default::default() })
        .await
        .unwrap();
    let underscore = store
        .scan(ScanQuery { channel_id: "chan".into(), text: Some("_".into()), limit: 10, ..Default::default() })
        .await
        .unwrap();

    assert_eq!((ids(&percent.matches), ids(&underscore.matches)), (["200"].into(), ["300"].into()));
}

#[tokio::test]
async fn scan_orders_by_descending_id_and_honors_limit() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    for id in [200, 100, 400, 300] {
        store.insert(message(id, "chan", "crow", "row", &[])).await.unwrap();
    }

    let scan = store.scan(ScanQuery { channel_id: "chan".into(), limit: 2, ..Default::default() }).await.unwrap();
    assert_eq!(ids(&scan.matches), ["400", "300"]);
}

#[tokio::test]
async fn scan_round_trips_the_complete_rendered_message() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    let rendered = RenderedMessage {
        id: "909".to_owned(),
        author_id: "author".to_owned(),
        author_name: "Crow Name".to_owned(),
        timestamp: "2026-09-09T09:09:09Z".to_owned(),
        edited_timestamp: Some("2026-09-09T10:10:10Z".to_owned()),
        reply: Some(RenderedReply {
            unavailable: false,
            id: "808".to_owned(),
            author_name: "Parent".to_owned(),
            snippet: "parent snippet".to_owned(),
        }),
        reactions: vec![RenderedReaction { label: "🐦".to_owned(), count: 7 }],
        attachments: vec![RenderedAttachment {
            id: "attachment".to_owned(),
            filename: "crow.png".to_owned(),
            size: 12345,
            content_type: Some("image/png".to_owned()),
            description: Some("a black bird".to_owned()),
            dimensions: Some((640, 480)),
            url: "https://example.invalid/crow.png".to_owned(),
        }],
        stickers: vec![RenderedSticker {
            id: "sticker".to_owned(),
            name: "caw".to_owned(),
            format: "Png".to_owned(),
            url: "https://example.invalid/sticker.png".to_owned(),
        }],
        embeds: vec![RenderedEmbed {
            kind: Some("rich".to_owned()),
            title: Some("Nest".to_owned()),
            description: Some("embed body".to_owned()),
            url: Some("https://example.invalid/nest".to_owned()),
            image: Some("https://example.invalid/image.png".to_owned()),
            thumbnail: Some("https://example.invalid/thumb.png".to_owned()),
        }],
        content: "complete payload".to_owned(),
    };
    let expected = serde_json::to_value(&rendered).unwrap();
    store
        .insert(NewMessage {
            rendered,
            guild_id: Some("guild".to_owned()),
            channel_id: "chan".to_owned(),
            mention_ids: vec!["55".to_owned()],
        })
        .await
        .unwrap();

    let scan = store.scan(ScanQuery { channel_id: "chan".into(), limit: 10, ..Default::default() }).await.unwrap();
    assert_eq!(scan.matches.len(), 1);
    assert_eq!(serde_json::to_value(&scan.matches[0]).unwrap(), expected);
}

#[tokio::test]
async fn scan_floor_is_none_for_a_channel_without_rows() {
    let db = TestDb::new();
    let store = Ledger::open(db.path()).await.unwrap().archive();
    store.insert(message(101, "occupied", "crow", "elsewhere", &[])).await.unwrap();

    let scan = store.scan(ScanQuery { channel_id: "empty".into(), limit: 10, ..Default::default() }).await.unwrap();
    assert!(scan.matches.is_empty());
    assert_eq!(scan.floor, None);
}
