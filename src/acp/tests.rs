use super::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use tempfile::tempdir;

async fn test_client(prompt_script: &str) -> (tempfile::TempDir, Client) {
    let directory = tempdir().unwrap();
    let adapter = directory.path().join("fake-acp.sh");
    let script = [
        r#"#!/bin/sh
prompts=0
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{}}'
      ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"test"}}'
      ;;
    *'"method":"session/prompt"'*)
      prompts=$((prompts + 1))
"#,
        prompt_script,
        "\n;;\nesac\ndone\n",
    ]
    .concat();
    fs::write(&adapter, script).unwrap();
    let launch = Launch {
        runtime_name: "Fake",
        program: "/bin/sh".into(),
        args: vec![adapter.into_os_string()],
        env: Default::default(),
        session_meta: None,
    };
    let client = Client::start(launch, directory.path(), Vec::new(), None)
        .await
        .unwrap();
    (directory, client)
}

#[tokio::test]
async fn permanent_permissions_survive_restart_and_can_be_revoked() {
    let (directory, client) = test_client(r#"
printf '%s\n' '{"jsonrpc":"2.0","id":77,"method":"session/request_permission","params":{"toolCall":{"toolCallId":"bash-1","title":"bash","_meta":{"toolName":"bash"}},"options":[{"optionId":"once","kind":"allow_once","name":"Allow once"},{"optionId":"allow_always","kind":"allow_always","name":"Always allow bash"}]}}'
IFS= read -r answer
printf '%s\n' "$answer" >> answers.jsonl
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
"#).await;
    fs::create_dir(directory.path().join(".agents")).unwrap();
    fs::write(
        directory.path().join(".agents/agent.json"),
        json!({
            "name": "test", "slug": "test", "description": "test", "harness": "pi"
        })
        .to_string(),
    )
    .unwrap();
    drop(client);
    for (index, decision) in [
        Some("allow_always"),
        Some(crate::permissions::REMEMBER),
        None,
        Some("once"),
    ]
    .into_iter()
    .enumerate()
    {
        let launch = Launch {
            runtime_name: "Fake",
            program: "/bin/sh".into(),
            args: vec![directory.path().join("fake-acp.sh").into_os_string()],
            env: Default::default(),
            session_meta: None,
        };
        let mut client = Client::start(launch, directory.path(), Vec::new(), None)
            .await
            .unwrap();
        client.remember_permissions(directory.path(), crate::harness::get("pi").unwrap());
        let mut prompts = 0;
        client
            .prompt("test", |event| {
                if let StreamEvent::PermissionRequest { options, .. } = event {
                    prompts += 1;
                    assert!(options
                        .iter()
                        .any(|option| option.id == crate::permissions::REMEMBER));
                    return Ok(decision.map(str::to_owned));
                }
                Ok(None)
            })
            .await
            .unwrap();
        assert_eq!(prompts, usize::from(index != 2));
        let config = crate::config::AgentConfig::load(directory.path()).unwrap();
        assert_eq!(
            config.permissions.len(),
            usize::from(matches!(index, 1 | 2))
        );
        if index == 2 {
            let request = json!({"jsonrpc":"2.0","id":99,"method":"session/request_permission",
                "params":{"toolCall":{"title":"bash","_meta":{"toolName":"bash"}},
                "options":[{"optionId":"once","kind":"allow_once"}]}});
            client
                .handle_request(
                    &request,
                    &mut Updates::new(|event| {
                        if let StreamEvent::PermissionRequest { cancelled, .. } = event {
                            assert!(cancelled);
                        }
                        Ok(Some(crate::permissions::REMEMBER.into()))
                    }),
                    true,
                )
                .await
                .unwrap();
            crate::config::save_permissions(directory.path(), &[]).unwrap();
        }
        drop(client);
    }
    let answers = fs::read_to_string(directory.path().join("answers.jsonl")).unwrap();
    let ids = answers
        .lines()
        .map(|line| {
            serde_json::from_str::<Value>(line).unwrap()["result"]["outcome"]["optionId"].clone()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            json!("allow_always"),
            json!("once"),
            json!("once"),
            json!("once")
        ]
    );
}

#[tokio::test]
async fn full_access_applies_immediately_and_ask_mode_restores_prompts() {
    let (directory, mut client) = test_client(r#"
response_id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
printf '%s\n' '{"jsonrpc":"2.0","id":77,"method":"session/request_permission","params":{"toolCall":{"title":"bash"},"options":[{"optionId":"once","kind":"allow_once","name":"Allow once"}]}}'
IFS= read -r answer
printf '%s\n' "$answer" >> answers.jsonl
printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$response_id"
"#).await;
    fs::create_dir(directory.path().join(".agents")).unwrap();
    fs::write(
        directory.path().join(".agents/agent.json"),
        json!({
            "name": "test", "slug": "test", "description": "test", "harness": "pi"
        })
        .to_string(),
    )
    .unwrap();
    client.remember_permissions(directory.path(), crate::harness::get("pi").unwrap());
    for mode in [
        crate::permissions::Mode::FullAccess,
        crate::permissions::Mode::Ask,
    ] {
        crate::config::save_permission_mode(directory.path(), mode).unwrap();
        let mut prompts = 0;
        client
            .prompt("test", |event| {
                if matches!(event, StreamEvent::PermissionRequest { .. }) {
                    prompts += 1;
                    return Ok(Some("once".into()));
                }
                Ok(None)
            })
            .await
            .unwrap();
        assert_eq!(prompts, usize::from(mode == crate::permissions::Mode::Ask));
        assert!(crate::config::AgentConfig::load(directory.path())
            .unwrap()
            .permissions
            .is_empty());
    }
    let answers = fs::read_to_string(directory.path().join("answers.jsonl")).unwrap();
    assert_eq!(answers.lines().count(), 2);
    assert!(answers.lines().all(
        |line| serde_json::from_str::<Value>(line).unwrap()["result"]["outcome"]["optionId"]
            == "once"
    ));
    crate::config::save_permission_mode(directory.path(), crate::permissions::Mode::FullAccess)
        .unwrap();
    let request = json!({"jsonrpc":"2.0","id":99,"method":"session/request_permission",
        "params":{"toolCall":{"title":"bash"},"options":[{"optionId":"once","kind":"allow_once"}]}});
    let mut denied = false;
    client
        .handle_request(
            &request,
            &mut Updates::new(|event| {
                if let StreamEvent::PermissionDecision { allowed, .. } = event {
                    denied = !allowed;
                }
                Ok(Some("once".into()))
            }),
            true,
        )
        .await
        .unwrap();
    assert!(denied);
}

#[tokio::test]
async fn retains_harness_usage_for_status() {
    let (_directory, mut client) = test_client(r#"
printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"usage_update","used":1234,"size":10000,"cost":{"amount":0.02,"currency":"USD"}}}}'
printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn","usage":{"inputTokens":100,"outputTokens":20,"totalTokens":120}}}'
"#).await;
    assert!(client.usage.is_none());
    assert!(client.context_usage.is_none());
    client.prompt("hello", |_| Ok(None)).await.unwrap();
    assert_eq!(client.context_usage.as_ref().unwrap()["used"], 1234);
    assert_eq!(client.usage.as_ref().unwrap()["totalTokens"], 120);
}

#[tokio::test]
async fn preserves_partial_response_across_inactivity_timeout() {
    let (_directory, mut client) = test_client(r#"
if [ "$prompts" -eq 1 ]; then
  printf '%s' '{"jsonrpc":"2.0","id":3,'
  sleep 0.1
  printf '%s\n' '"result":{"stopReason":"cancelled"}}'
else
  printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Recovered"}}}}'
  printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}'
fi
"#).await;
    let error = client
        .prompt_with_timeout("Wait", |_| Ok(None), Some(Duration::from_millis(30)))
        .await
        .unwrap_err();
    assert!(error
        .downcast_ref::<PromptTimeout>()
        .unwrap()
        .connection_ready());
    assert_eq!(
        client.prompt("Again", |_| Ok(None)).await.unwrap(),
        "Recovered"
    );
}

#[tokio::test]
async fn cancellation_deadline_does_not_restart_for_updates_or_permissions() {
    let (_directory, mut client) = test_client(r#"
sleep 0.1
printf '%s\n' '{"jsonrpc":"2.0","id":77,"method":"session/request_permission","params":{"toolCall":{"title":"Late write"},"options":[]}}'
while :; do
  printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Still running"}}}}'
  sleep 0.02
done
"#).await;
    let started = Instant::now();
    let mut late_updates = 0;
    let mut late_permissions = 0;
    let error = timeout(
        Duration::from_secs(7),
        client.prompt_with_timeout(
            "Wait",
            |event| {
                match event {
                    StreamEvent::PermissionRequest { cancelled, .. } => {
                        assert!(cancelled);
                        late_permissions += 1;
                    }
                    StreamEvent::Assistant(_) => late_updates += 1,
                    _ => {}
                }
                Ok(None)
            },
            Some(Duration::from_millis(30)),
        ),
    )
    .await
    .expect("cancellation did not meet its deadline")
    .unwrap_err();
    assert!(!error
        .downcast_ref::<PromptTimeout>()
        .unwrap()
        .connection_ready());
    assert!(started.elapsed() < Duration::from_secs(6));
    assert!(late_updates > 0);
    assert_eq!(late_permissions, 1);
}

#[test]
fn selects_the_advertised_continuation_method() {
    assert_eq!(
        continuation_method(&json!({
            "agentCapabilities": {"sessionCapabilities": {"resume": {}}}
        })),
        Some("session/resume")
    );
    assert_eq!(
        continuation_method(&json!({
            "agentCapabilities": {"loadSession": true}
        })),
        Some("session/load")
    );
    assert_eq!(continuation_method(&json!({})), None);
}

#[tokio::test]
async fn runs_a_streaming_session_with_tools_and_permission() {
    let directory = tempdir().unwrap();
    let adapter = directory.path().join("fake-acp.sh");
    fs::write(
        &adapter,
        r##"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"agentCapabilities":{}}}'
      ;;
    *'"method":"session/new"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"fake-session"}}'
      ;;
    *'"method":"session/prompt"'*)
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_thought_chunk","content":{"text":"Thinking"}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call","toolCallId":"tool-1","title":"Read file","status":"pending"}}}'
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"tool_call_update","toolCallId":"tool-1","title":"Read file","status":"completed","rawOutput":"done"}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":77,"method":"session/request_permission","params":{"toolCall":{"title":"Write file","rawInput":{"command":"write workspace/file"}},"options":[{"optionId":"allow","name":"Allow once","kind":"allow_once"},{"optionId":"reject","name":"Reject once","kind":"reject_once"}]}}'
      IFS= read -r permission
      printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Hello"}}}}'
      printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}'
      ;;
    *'"method":"session/close"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{}}'
      exit 0
      ;;
  esac
done
"##,
    )
    .unwrap();
    fs::set_permissions(&adapter, fs::Permissions::from_mode(0o755)).unwrap();
    let launch = Launch {
        runtime_name: "Fake",
        program: "/bin/sh".into(),
        args: vec![adapter.into_os_string()],
        env: Default::default(),
        session_meta: None,
    };
    let mut client = Client::start(launch, directory.path(), Vec::new(), None)
        .await
        .unwrap();
    let mut events = Vec::new();
    let answer = client
        .prompt("Hi", |event| {
            let selection = match &event {
                StreamEvent::PermissionRequest { options, .. } => {
                    options.first().map(|option| option.id.clone())
                }
                _ => None,
            };
            events.push(event);
            Ok(selection)
        })
        .await
        .unwrap();
    assert_eq!(answer, "Hello");
    assert!(events.iter().any(|event| matches!(event,
        StreamEvent::PermissionRequest { tool_call: Some(call), .. }
            if call["rawInput"]["command"] == "write workspace/file")));

    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::Reasoning(_))));
    assert!(events.iter().any(|event| matches!(
        event,
        StreamEvent::Tool {
            status: Some(ToolStatus::Completed),
            ..
        }
    )));
    assert!(events
        .iter()
        .any(|event| matches!(event, StreamEvent::PermissionDecision { allowed: true, .. })));
    client.close().await;
}

#[tokio::test]
async fn drains_a_cancelled_turn_before_the_next_prompt() {
    let (_directory, mut client) = test_client(r#"
      if [ "$prompts" -eq 1 ]; then
        IFS= read -r cancel
        printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"stopReason":"cancelled"}}'
      else
        printf '%s\n' '{"jsonrpc":"2.0","method":"session/update","params":{"update":{"sessionUpdate":"agent_message_chunk","content":{"text":"Recovered"}}}}'
        printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{"stopReason":"end_turn"}}'
      fi
"#).await;
    let error = client
        .prompt_with_timeout("Wait", |_| Ok(None), Some(Duration::from_millis(10)))
        .await
        .unwrap_err();
    assert!(error.downcast_ref::<PromptTimeout>().is_some());
    let answer = client.prompt("Again", |_| Ok(None)).await.unwrap();
    assert_eq!(answer, "Recovered");
}

#[tokio::test]
async fn marks_the_connection_unusable_when_cancellation_does_not_finish() {
    let (_directory, mut client) = test_client("sleep 1").await;

    let error = client
        .prompt_with_timeout("Wait", |_| Ok(None), Some(Duration::from_millis(10)))
        .await
        .unwrap_err();
    let timeout = error.downcast_ref::<PromptTimeout>().unwrap();
    assert!(!timeout.connection_ready());
    drop(client);
}
