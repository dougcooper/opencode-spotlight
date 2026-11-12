//! Unit and integration tests for the OpenCode HTTP client layer.

use super::*;
use base64::Engine;
use httpmock::prelude::*;

// ---------------------------------------------------------------------------
// Secret redaction
// ---------------------------------------------------------------------------

#[test]
fn secret_never_prints() {
    let secret = Secret("hunter2-supersecret".to_string());
    assert!(!format!("{secret:?}").contains("hunter2"));
    assert!(!format!("{secret}").contains("hunter2"));
    assert_eq!(secret.expose(), "hunter2-supersecret");
}

#[test]
fn registration_parses_and_validates() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("service.json");
    std::fs::write(
        &path,
        r#"{"id":"x","version":"2.2.7","url":"http://127.0.0.1:5001","pid":1,"password":"pw"}"#,
    )
    .unwrap();
    let reg = Registration::load(&path).unwrap();
    assert_eq!(reg.url, "http://127.0.0.1:5001");
    assert_eq!(reg.password.expose(), "pw");

    std::fs::write(&path, r#"{"url":"","password":""}"#).unwrap();
    assert!(Registration::load(&path).is_err());

    std::fs::write(&path, "not json").unwrap();
    assert!(Registration::load(&path).is_err());
}

// ---------------------------------------------------------------------------
// Endpoints against a mock server
// ---------------------------------------------------------------------------

fn mock_client(url: &str) -> Client {
    Client::new(&Registration {
        url: url.to_string(),
        password: Secret("correct-horse".into()),
        pid: None,
        version: None,
    })
    .unwrap()
}

fn base64_header_value() -> String {
    format!("Basic {}", B64.encode("opencode:correct-horse"))
}

/// Tests that the create-session call carries the scratch location and title,
/// by matching the exact request body.
#[tokio::test]
async fn create_session_writes_location_and_title() {
    let svc = MockServer::start();
    let create = svc.mock(|when, then| {
        when.method(POST)
            .path("/api/session")
            .json_body_includes(json!({"location": {"directory": "/scratch/dir"}}).to_string())
            .json_body_includes(json!({"title": "Quick chat"}).to_string());
        then.status(200).json_body(json!({
            "data": { "id": "ses_1", "title": "Quick chat", "time": { "created": 1.0, "updated": 2.0 } }
        }));
    });

    let client = mock_client(&svc.url(""));
    let session = client
        .create_session(Some("Quick chat"), "/scratch/dir")
        .await
        .expect("create call succeeds");
    assert_eq!(session.id, "ses_1");
    assert_eq!(session.title.as_deref(), Some("Quick chat"));
    create.assert_calls(1);
}

/// Auth is Basic auth from the registration password; the authorization
/// header is part of the match, so a successful call proves it was sent.
#[tokio::test]
async fn client_sends_basic_auth_and_parses_info() {
    let svc = MockServer::start();
    let auth = base64_header_value();
    let info = svc.mock(|when, then| {
        when.method(GET)
            .path("/api/info")
            .header("authorization", auth.as_str());
        then.status(200).json_body(json!({"version": "2.2.7", "pid": 1234}));
    });

    let client = mock_client(&svc.url(""));
    let got = client.info().await.expect("info call");
    assert_eq!(got.version, "2.2.7");
    info.assert_calls(1);
}

#[tokio::test]
async fn prompt_carries_text_and_inline_image() {
    let svc = MockServer::start();
    let prompt = svc.mock(|when, then| {
        when.method(POST)
            .path("/api/session/ses_1/prompt")
            .body_includes(r#""text":"what am I looking at?""#)
            .body_includes(r#""type":"inline""#)
            .body_includes(r#""name":"clipboard.png""#);
        then.status(200).json_body(json!({ "id": "msg1" }));
    });

    let client = mock_client(&svc.url(""));
    let files = vec![FileAttachment::inline_data(
        "data:image/png;base64,QUJD".into(),
        Some("clipboard.png".into()),
    )];
    client.prompt("ses_1", "what am I looking at?", &files).await.expect("prompt call");
    prompt.assert_calls(1);
}

#[tokio::test]
async fn busy_error_is_recognized() {
    let svc = MockServer::start();
    svc.mock(|when, then| {
        when.method(POST).path("/api/session/ses_1/prompt");
        then.status(409)
            .json_body(json!({ "_tag": "SessionBusyError", "sessionID": "ses_1" }));
    });

    let client = mock_client(&svc.url(""));
    let err = client.prompt("ses_1", "again", &[]).await.unwrap_err();
    assert!(matches!(err, ApiError::Busy), "got {err:?}");
}

#[tokio::test]
async fn permission_reply_sends_decision() {
    let svc = MockServer::start();
    let reply = svc.mock(|when, then| {
        when.method(POST)
            .path("/api/session/ses_1/permission/p1/reply")
            .json_body_includes(json!({"decision": "always"}).to_string());
        then.status(200).json_body(json!({}));
    });

    let client = mock_client(&svc.url(""));
    client.reply_permission("ses_1", "p1", "always", None).await.expect("reply call");
    reply.assert_calls(1);
}

#[tokio::test]
async fn list_sessions_asks_newest_in_directory() {
    let svc = MockServer::start();
    let list = svc.mock(|when, then| {
        when.method(GET)
            .path("/api/session")
            .query_param("order", "desc")
            .query_param("archived", "false")
            .query_param("directory", "/scratch/dir");
        then.status(200).json_body(json!({ "data": [
            { "id": "ses_new", "title": "Newest", "time": { "created": 2.0, "updated": 9.0 } },
            { "id": "ses_old", "title": "Old", "time": { "created": 1.0, "updated": 3.0 } }
        ] }));
    });

    let client = mock_client(&svc.url(""));
    let sessions = client.list_sessions(Some("/scratch/dir")).await.expect("list call");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].id, "ses_new");
    list.assert_calls(1);
}

#[tokio::test]
async fn messages_follow_pagination_cursor() {
    let svc = MockServer::start();
    let page1 = svc.mock(|when, then| {
        when.method(GET).path("/api/session/ses_1/message").query_param_missing("cursor");
        then.status(200).json_body(json!({
            "data": [ { "id": "m1", "type": "user" } ],
            "cursor": { "previous": null, "next": "page2" }
        }));
    });
    let page2 = svc.mock(|when, then| {
        when.method(GET).path("/api/session/ses_1/message").query_param("cursor", "page2");
        then.status(200).json_body(json!({
            "data": [ { "id": "m2", "type": "assistant" } ],
            "cursor": { "previous": "page2", "next": null }
        }));
    });

    let client = mock_client(&svc.url(""));
    let msgs = client.messages("ses_1").await.expect("messages call");
    assert_eq!(msgs.len(), 2);
    assert_eq!(page1.calls(), 1);
    assert_eq!(page2.calls(), 1);
}

#[tokio::test]
async fn wrong_password_is_rejected_at_http_level() {
    let svc = MockServer::start();
    svc.mock(|when, then| {
        when.method(GET).path("/api/info");
        then.status(401).json_body(json!({ "message": "unauthorized" }));
    });

    let client = mock_client(&svc.url(""));
    // The probe in ensure_connection treats this as a failed health check.
    let err = client.info().await.unwrap_err();
    assert!(matches!(err, ApiError::Http { status: 401, .. }), "{err:?}");
}
