//! Database integration tests.
//!
//! These use an in-memory SQLite database — no filesystem footprint,
//! run in CI as fast as unit tests.

use everevo_db::models::MessageRow;
use everevo_db::Database;
use uuid::Uuid;

/// Create a fresh in-memory database for each test.
async fn setup_db() -> Database {
    Database::connect(std::path::Path::new(":memory:"))
        .await
        .expect("Failed to create in-memory DB")
}

#[tokio::test]
async fn test_create_and_list_sessions() {
    let db = setup_db().await;

    let s1 = db.create_session("Test Session 1").await.unwrap();
    let _s2 = db.create_session("Test Session 2").await.unwrap();

    let list = db.list_sessions().await.unwrap();
    assert_eq!(list.len(), 2);
    // Most recent first
    assert_eq!(list[0].title, "Test Session 2");
    assert_eq!(list[1].title, "Test Session 1");

    // Get by ID
    let found = db.get_session(s1.id).await.unwrap().unwrap();
    assert_eq!(found.title, "Test Session 1");
}

#[tokio::test]
async fn test_add_and_get_messages() {
    let db = setup_db().await;
    let session = db.create_session("Chat").await.unwrap();

    let msg = MessageRow::new(session.id, "user", "Hello, agent!", None, None);
    db.add_message(&msg).await.unwrap();

    let history = db.get_messages(session.id, None).await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].content, "Hello, agent!");
}

#[tokio::test]
async fn test_delete_session_cascades_messages() {
    let db = setup_db().await;
    let session = db.create_session("To Delete").await.unwrap();

    let msg = MessageRow::new(session.id, "user", "temp", None, None);
    db.add_message(&msg).await.unwrap();

    db.delete_session(session.id).await.unwrap();

    // Session gone
    assert!(db.get_session(session.id).await.unwrap().is_none());
    // Messages gone
    let history = db.get_messages(session.id, None).await.unwrap();
    assert!(history.is_empty());
}

#[tokio::test]
async fn test_session_not_found() {
    let db = setup_db().await;
    let result = db.get_session(Uuid::new_v4()).await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn test_search_sessions() {
    let db = setup_db().await;
    let s1 = db.create_session("Rust Programming").await.unwrap();

    let msg = MessageRow::new(s1.id, "user", "How do I use async in Rust?", None, None);
    db.add_message(&msg).await.unwrap();

    // Search by title
    let results = db.search_sessions("Rust").await.unwrap();
    assert!(!results.is_empty());

    // Search by message content
    let results = db.search_sessions("async").await.unwrap();
    assert!(!results.is_empty());

    // No match
    let results = db.search_sessions("zzz_nonexistent_zzz").await.unwrap();
    assert!(results.is_empty());
}

#[tokio::test]
async fn test_update_session_title() {
    let db = setup_db().await;
    let session = db.create_session("Old Title").await.unwrap();

    db.update_session_title(session.id, "New Title").await.unwrap();

    let updated = db.get_session(session.id).await.unwrap().unwrap();
    assert_eq!(updated.title, "New Title");
}
