use super::*;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn next_event(rx: &mut mpsc::UnboundedReceiver<AppEvent>) -> AppEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn failure_retains_accepted_turn_even_before_caller_consumes_reply() {
    let temp = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let server = IpcServer::bind(temp.path(), tx).unwrap().unwrap();
    let (notifications, mut notification_rx) = mpsc::unbounded_channel();
    let client = get_persistent_client(temp.path().to_str().unwrap(), &notifications)
        .await
        .unwrap();
    let request = serde_json::json!({"id":42,"op":"post_prompt","params":{"text":"hello"}});
    client
        .writer
        .lock()
        .await
        .as_mut()
        .unwrap()
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    loop {
        if let AppEvent::Ipc(IpcCommand::Request { reply, .. }) = next_event(&mut rx).await {
            reply
                .send(Ok(
                    serde_json::json!({"accepted":true,"mode":"prompt","turn_id":"accepted-turn"}),
                ))
                .unwrap();
            break;
        }
    }
    // Deliberately do not consume incoming replies. Reader-side correlation
    // must still be recorded before the server disappears.
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.accepted_turn_id.lock().unwrap().as_deref() != Some("accepted-turn") {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(server);
    match next_event(&mut notification_rx).await {
        AppEvent::IpcNotification { event, .. } => {
            assert_eq!(event["event"], "connection.failed");
            assert_eq!(event["payload"]["turn_id"], "accepted-turn");
            assert!(event["payload"].get("pending_input_dropped").is_none());
            assert_eq!(event["payload"]["accepted_work_may_continue"], true);
        }
        event => panic!("unexpected event: {event:?}"),
    }
}

#[tokio::test]
async fn persistent_exchanges_serialize_and_timeout_without_replay() {
    let temp = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _server = IpcServer::bind(temp.path(), tx).unwrap().unwrap();
    let (notifications, _notifications_rx) = mpsc::unbounded_channel();
    let client = get_persistent_client(temp.path().to_str().unwrap(), &notifications)
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for n in 0..2 {
        let client = client.clone();
        tasks.push(tokio::spawn(async move {
            persistent_request(&client, "get_state", serde_json::json!({"n":n})).await
        }));
    }
    for _ in 0..2 {
        loop {
            if let AppEvent::Ipc(IpcCommand::Request { params, reply, .. }) =
                next_event(&mut rx).await
            {
                reply.send(Ok(params)).unwrap();
                break;
            }
        }
    }
    for (n, task) in tasks.into_iter().enumerate() {
        assert_eq!(task.await.unwrap().unwrap()["n"], n);
    }
    let timed_client = client.clone();
    let timeout = tokio::spawn(async move {
        persistent_request_with_timeout(
            &timed_client,
            "post_prompt",
            serde_json::json!({"text":"hello"}),
            Duration::from_millis(50),
        )
        .await
    });
    let held_reply = loop {
        if let AppEvent::Ipc(IpcCommand::Request { reply, .. }) = next_event(&mut rx).await {
            break reply;
        }
    };
    assert!(timeout.await.unwrap().unwrap_err().starts_with("timeout:"));
    assert!(client.closed.load(Ordering::Relaxed));
    assert!(
        persistent_request(&client, "post_prompt", serde_json::json!({}))
            .await
            .unwrap_err()
            .contains("closed")
    );
    drop(held_reply);
}

#[tokio::test]
async fn cancellation_closes_pending_dispatch_and_fresh_request_has_own_reply() {
    let temp = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _server = IpcServer::bind(temp.path(), tx).unwrap().unwrap();
    let (notifications, mut notification_rx) = mpsc::unbounded_channel();
    let cwd = temp.path().to_str().unwrap();
    let client = get_persistent_client(cwd, &notifications).await.unwrap();
    let request_client = client.clone();
    let request = tokio::spawn(async move {
        persistent_request(
            &request_client,
            "post_prompt",
            serde_json::json!({"text":"cancel me"}),
        )
        .await
    });
    let held_reply = loop {
        if let AppEvent::Ipc(IpcCommand::Request { reply, .. }) = next_event(&mut rx).await {
            break reply;
        }
    };
    request.abort();
    assert!(request.await.unwrap_err().is_cancelled());
    assert!(client.closed.load(Ordering::Relaxed));
    assert!(client.writer.lock().await.is_none());
    assert!(
        matches!(next_event(&mut notification_rx).await, AppEvent::IpcNotification { event, .. } if event["event"] == "connection.failed")
    );
    loop {
        if matches!(
            next_event(&mut rx).await,
            AppEvent::Ipc(IpcCommand::Disconnect { .. })
        ) {
            break;
        }
    }
    assert!(held_reply.is_closed());
    assert!(
        held_reply
            .send(Ok(serde_json::json!({"stale":true})))
            .is_err()
    );
    let fresh = get_persistent_client(cwd, &notifications).await.unwrap();
    assert!(!Arc::ptr_eq(&fresh, &client));
    let task = tokio::spawn(async move {
        persistent_request(&fresh, "get_state", serde_json::json!({})).await
    });
    loop {
        if let AppEvent::Ipc(IpcCommand::Request { reply, .. }) = next_event(&mut rx).await {
            reply.send(Ok(serde_json::json!({"fresh":true}))).unwrap();
            break;
        }
    }
    assert_eq!(task.await.unwrap().unwrap()["fresh"], true);
}

#[tokio::test]
async fn queued_events_are_delivered_while_application_reply_is_pending() {
    let temp = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _server = IpcServer::bind(temp.path(), tx).unwrap().unwrap();
    let stream = connect_session(temp.path().to_str().unwrap())
        .await
        .unwrap();
    let (read, mut write) = split_stream(stream);
    let mut reader = BufReader::new(read);
    let events = match next_event(&mut rx).await {
        AppEvent::Ipc(IpcCommand::Subscribe { events, .. }) => events,
        event => panic!("{event:?}"),
    };
    write
        .write_all(b"{\"id\":99,\"op\":\"post_prompt\"}\n")
        .await
        .unwrap();
    let reply = match next_event(&mut rx).await {
        AppEvent::Ipc(IpcCommand::Request { reply, .. }) => reply,
        event => panic!("{event:?}"),
    };
    events
        .send(control_revoked_event("session", Some("turn")))
        .unwrap();
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["event"],
        "control.revoked"
    );
    drop(reader);
    drop(write);
    assert!(matches!(
        next_event(&mut rx).await,
        AppEvent::Ipc(IpcCommand::Disconnect { .. })
    ));
    assert!(reply.is_closed());
}

#[test]
fn endpoint_is_stable_distinct_and_resolves_aliases() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("Worktree Ω");
    let second = temp.path().join("other");
    std::fs::create_dir(&first).unwrap();
    std::fs::create_dir(&second).unwrap();
    let name = windows::endpoint(&first).unwrap();
    assert!(name.starts_with(r"\\.\pipe\xi-session-v1-"));
    assert_eq!(name, windows::endpoint(&first.join(".")).unwrap());
    assert_eq!(
        name,
        windows::endpoint(&std::fs::canonicalize(&first).unwrap()).unwrap()
    );
    let differently_cased = temp.path().join("WORKTREE Ω");
    // A case-sensitive directory may legitimately distinguish these names.
    if differently_cased.exists() {
        assert_eq!(name, windows::endpoint(&differently_cased).unwrap());
    }
    assert_ne!(name, windows::endpoint(&second).unwrap());
    assert!(windows::endpoint(&temp.path().join("missing")).is_err());
}

#[test]
fn security_descriptor_grants_only_current_user() {
    use interprocess::os::windows::security_descriptor::AsSecurityDescriptorExt;
    let descriptor = windows::security_descriptor().unwrap();
    // DACL_SECURITY_INFORMATION = 4. Inspect the actual owned descriptor,
    // not just the input SDDL, including its protected-DACL flag.
    let sddl = descriptor
        .serialize(4, |value| value.to_string_lossy())
        .unwrap();
    assert!(sddl.starts_with("D:P"), "{sddl}");
    assert_eq!(sddl.matches("(A;").count(), 1, "{sddl}");
    assert!(sddl.contains(";;;S-1-"), "{sddl}");
    assert!(!sddl.contains(";;;WD)"), "{sddl}");
}

#[tokio::test]
async fn ownership_and_drop_release_endpoint() {
    let temp = tempfile::tempdir().unwrap();
    let (tx, _rx) = mpsc::unbounded_channel();
    let server = IpcServer::bind(temp.path(), tx.clone()).unwrap().unwrap();
    assert!(IpcServer::bind(temp.path(), tx.clone()).unwrap().is_none());
    let task = server.accept_task.abort_handle();
    drop(server);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !task.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(IpcServer::bind(temp.path(), tx).unwrap().is_some());
}

#[tokio::test]
async fn request_reply_invalid_request_events_and_disconnect() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_str().unwrap().to_owned();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _server = IpcServer::bind(temp.path(), tx).unwrap().unwrap();
    let stream = connect_session(&cwd).await.unwrap();
    let (read, mut write) = split_stream(stream);
    let mut reader = BufReader::new(read);
    let events = match next_event(&mut rx).await {
        AppEvent::Ipc(IpcCommand::Subscribe { events, .. }) => events,
        event => panic!("unexpected event: {event:?}"),
    };
    write.write_all(b"not json\n").await.unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    let error: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(error["error"]["code"], "invalid_request");
    write
        .write_all(b"{\"id\":7,\"op\":\"inspect_session\"}\n")
        .await
        .unwrap();
    match next_event(&mut rx).await {
        AppEvent::Ipc(IpcCommand::Request { op, reply, .. }) => {
            assert_eq!(op, "inspect_session");
            reply.send(Ok(serde_json::json!({"test":42}))).unwrap();
        }
        event => panic!("unexpected event: {event:?}"),
    }
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    let response: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["id"], 7);
    assert_eq!(response["result"]["test"], 42);
    events
        .send(control_revoked_event("test-session", Some("test-turn")))
        .unwrap();
    line.clear();
    reader.read_line(&mut line).await.unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&line).unwrap()["event"],
        "control.revoked"
    );
    drop(reader);
    drop(write);
    assert!(matches!(
        next_event(&mut rx).await,
        AppEvent::Ipc(IpcCommand::Disconnect { .. })
    ));
}

#[tokio::test]
async fn one_shot_client_and_multiple_connections() {
    let temp = tempfile::tempdir().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _server = IpcServer::bind(temp.path(), tx).unwrap().unwrap();
    let mut tasks = Vec::new();
    for _ in 0..3 {
        let cwd = temp.path().to_str().unwrap().to_owned();
        tasks.push(tokio::spawn(async move {
            client_call(&cwd, "get_state", serde_json::json!({})).await
        }));
    }
    let mut replied = 0;
    while replied < 3 {
        if let AppEvent::Ipc(IpcCommand::Request { reply, .. }) = next_event(&mut rx).await {
            reply
                .send(Ok(serde_json::json!({"status":"idle"})))
                .unwrap();
            replied += 1;
        }
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().unwrap()["status"], "idle");
    }
}

#[tokio::test]
async fn persistent_client_receives_completion_and_recovers_from_eof() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_str().unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let server = IpcServer::bind(temp.path(), tx.clone()).unwrap().unwrap();
    let (notification_tx, mut notifications) = mpsc::unbounded_channel();
    let client = get_persistent_client(cwd, &notification_tx).await.unwrap();
    let events = match next_event(&mut rx).await {
        AppEvent::Ipc(IpcCommand::Subscribe { events, .. }) => events,
        event => panic!("unexpected event: {event:?}"),
    };
    let request_client = client.clone();
    let request = tokio::spawn(async move {
        persistent_request(
            &request_client,
            "post_prompt",
            serde_json::json!({"text":"hello"}),
        )
        .await
    });
    match next_event(&mut rx).await {
        AppEvent::Ipc(IpcCommand::Request { reply, .. }) => {
            reply
                .send(Ok(serde_json::json!({"accepted":true})))
                .unwrap();
        }
        event => panic!("unexpected event: {event:?}"),
    }
    assert_eq!(request.await.unwrap().unwrap()["accepted"], true);
    events
        .send(completion_event(
            "session",
            "turn",
            "completed",
            Path::new("log"),
            None,
            None,
        ))
        .unwrap();
    assert!(
        matches!(next_event(&mut notifications).await, AppEvent::IpcNotification { event, .. } if event["event"] == "agent.completed")
    );
    let key = client_key(Path::new(cwd)).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while CLIENTS.get().unwrap().lock().unwrap().contains_key(&key) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(client);
    let replacement = get_persistent_client(cwd, &notification_tx).await.unwrap();
    loop {
        if matches!(
            next_event(&mut rx).await,
            AppEvent::Ipc(IpcCommand::Subscribe { .. })
        ) {
            break;
        }
    }
    // Drop terminates all Windows server connections; the reader must evict EOF.
    let task = server.accept_task.abort_handle();
    drop(server);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !task.is_finished() || CLIENTS.get().unwrap().lock().unwrap().contains_key(&key) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    drop(replacement);
    let _server = IpcServer::bind(temp.path(), tx).unwrap().unwrap();
    assert!(get_persistent_client(cwd, &notification_tx).await.is_ok());
}

#[tokio::test]
async fn unavailable_and_busy_connections_are_bounded() {
    let temp = tempfile::tempdir().unwrap();
    let cwd = temp.path().to_str().unwrap();
    assert!(
        connect_session(cwd)
            .await
            .unwrap_err()
            .contains("unavailable")
    );
    let endpoint = windows::endpoint(temp.path()).unwrap();
    let listener = PipeListenerOptions::new()
        .path(endpoint.as_str())
        .create_tokio_duplex::<Bytes>()
        .unwrap();
    let first = connect_session(cwd).await.unwrap();
    // Do not accept: no replacement pipe instance is available to a second client.
    assert!(
        tokio::time::timeout(Duration::from_secs(5), connect_session(cwd))
            .await
            .unwrap()
            .is_err()
    );
    drop(first);
    drop(listener);
}
