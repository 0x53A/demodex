use super::*;
use http_body_util::{BodyExt, Full};
use hyper::{
    Request, Response,
    body::{Bytes, Incoming},
    service::service_fn,
};
use hyper_util::rt::TokioIo;

async fn mock() -> (String, Arc<Mutex<Vec<Value>>>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let hits = seen.clone();
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let seen = hits.clone();
            tokio::spawn(async move {
                let svc = service_fn(move |r: Request<Incoming>| {
                    let seen = seen.clone();
                    async move {
                        let path = r.uri().path().to_owned();
                        let auth = r
                            .headers()
                            .get("authorization")
                            .and_then(|v| v.to_str().ok())
                            .unwrap_or("")
                            .to_owned();
                        let bytes = r.into_body().collect().await.unwrap().to_bytes();
                        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                        seen.lock()
                            .await
                            .push(json!({"path":path,"auth":auth,"body":body}));
                        let (content, kind) = if path == "/v1/oauth/token" {
                            (json!({"access_token":"claude-test-access","refresh_token":"claude-test-refresh","expires_in":3600}).to_string(),"application/json")
                        } else if path == "/v1/models" {
                            (
                                json!({"data":[model()],"has_more":false}).to_string(),
                                "application/json",
                            )
                        } else if path == "/v1/messages" {
                            let mut events = vec![
                                json!({"type":"message_start","message":{"usage":{"input_tokens":8,"cache_read_input_tokens":12,"output_tokens":0}}}),
                                json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
                                json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Mock Claude reply"}}),
                                json!({"type":"content_block_stop","index":0}),
                                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":4}}),
                                json!({"type":"message_stop"}),
                            ];
                            if body.to_string().contains("MOCK_SPAWN_GPT")
                                && !body["messages"].as_array().unwrap().iter().any(|m| {
                                    m["content"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .any(|b| b["type"] == "tool_result")
                                })
                            {
                                let exec = body["tools"].as_array().unwrap().iter().find(|t| {
                                    t["name"] == "exec"
                                        || t["description"].as_str().unwrap_or("").starts_with("exec:")
                                        || t["description"]
                                            .as_str()
                                            .unwrap_or("")
                                            .starts_with("functions.exec:")
                                });
                                let (tool, args) = if let Some(exec) = exec {
                                    (exec, json!({"input":spawn_js("claude-test")}))
                                } else {
                                    let tool = body["tools"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .find(|t| is_spawn_agent(t))
                                        .unwrap();
                                    let mut args = json!({"message":"Mock child reply","model":"claude-test","reasoning_effort":"low"});
                                    if tool["input_schema"]["properties"]
                                        .get("task_name")
                                        .is_some()
                                    {
                                        args["task_name"] = json!("child");
                                        args["fork_turns"] = json!("none");
                                    } else {
                                        args["fork_context"] = json!(false);
                                    }
                                    (tool, args)
                                };
                                events = vec![
                                    json!({"type":"message_start","message":{"usage":{"input_tokens":5,"output_tokens":0}}}),
                                    json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"childcall","name":tool["name"],"input":{}}}),
                                    json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":args.to_string()}}),
                                    json!({"type":"content_block_stop","index":0}),
                                    json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":5}}),
                                    json!({"type":"message_stop"}),
                                ];
                            }
                            (
                                events
                                    .iter()
                                    .map(|v| {
                                        format!(
                                            "event: {}\ndata: {v}\n\n",
                                            v["type"].as_str().unwrap()
                                        )
                                    })
                                    .collect::<String>(),
                                "text/event-stream",
                            )
                        } else {
                            let spawn = body.to_string().contains("MOCK_SPAWN_CLAUDE")
                                && !body["input"].as_array().unwrap().iter().any(|v| {
                                    matches!(
                                        v["type"].as_str(),
                                        Some("function_call_output" | "custom_tool_call_output")
                                    )
                                });
                            let item = if spawn {
                                let mut all = Vec::new();
                                if let Some(tools) = body["tools"].as_array() {
                                    flatten_tools(tools, None, &mut all);
                                }
                                for item in body["input"].as_array().unwrap() {
                                    if item["type"] == "additional_tools" {
                                        flatten_tools(
                                            item["tools"].as_array().unwrap(),
                                            None,
                                            &mut all,
                                        );
                                    }
                                }
                                if let Some((namespace, tool)) =
                                    all.iter().find(|(_, t)| t["name"] == "exec")
                                {
                                    let mut item = json!({"type":"custom_tool_call","id":"cc_child","call_id":"childcall","name":"exec","input":spawn_js("claude-test")});
                                    if let Some(n) = namespace {
                                        item["namespace"] = n.clone();
                                    }
                                    let _ = tool;
                                    item
                                } else {
                                    let (namespace, tool) = all
                                        .iter()
                                        .find(|(_, t)| t["name"] == "spawn_agent")
                                        .expect("spawn tool missing");
                                    let args = json!({"message":"Mock child reply","model":"claude-test","reasoning_effort":"low","task_name":"child","fork_turns":"none"});
                                    let mut item = json!({"type":"function_call","id":"fc_child","call_id":"childcall","name":tool["name"],"arguments":args.to_string()});
                                    if let Some(n) = namespace {
                                        item["namespace"] = n.clone();
                                    }
                                    item
                                }
                            } else {
                                json!({"type":"message","id":"msg_test","role":"assistant","status":"completed","content":[{"type":"output_text","text":"Mock GPT reply","annotations":[]}]})
                            };
                            let events = [
                                json!({"type":"response.created","response":{"id":"r","status":"in_progress","output":[]}}),
                                json!({"type":"response.output_item.added","output_index":0,"item":item}),
                                json!({"type":"response.output_item.done","output_index":0,"item":item}),
                                json!({"type":"response.completed","response":{"id":"r","status":"completed","output":[item],"usage":{"input_tokens":5,"output_tokens":5,"total_tokens":10}}}),
                            ];
                            (
                                events.iter().map(|v| format!("data: {v}\n\n")).collect(),
                                "text/event-stream",
                            )
                        };
                        Ok::<_, std::convert::Infallible>(
                            Response::builder()
                                .header("content-type", kind)
                                .body(Full::new(Bytes::from(content)))
                                .unwrap(),
                        )
                    }
                });
                let _ = hyper::server::conn::http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), svc)
                    .await;
            });
        }
    });
    (url, seen, task)
}
fn model() -> Value {
    json!({"id":"claude-test","display_name":"Claude fixture","max_input_tokens":200000,"max_tokens":32000,"capabilities":{"thinking":{"supported":true,"types":{"adaptive":{"supported":true}}}}})
}
fn local(root: &Path, url: &str) -> Arc<Claude> {
    let mut a = Claude::new(root, root.join("profile")).unwrap();
    let c = Arc::get_mut(&mut a).unwrap();
    c.api = url.into();
    c.token_url = format!("{url}/v1/oauth/token");
    a
}
async fn login(c: &Claude) -> Result<()> {
    let start = c.begin().await?;
    let url = reqwest::Url::parse(start["url"].as_str().unwrap())?;
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .into_owned();
    c.complete(
        start["id"].as_str().unwrap(),
        &format!("one-use-code#{state}"),
    )
    .await?;
    Ok(())
}

#[tokio::test]
async fn auth_state_receipts_refresh_and_disconnect() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (url, hits, task) = mock().await;
    let c = local(root.path(), &url);
    let first = c.begin().await?;
    let second = c.begin().await?;
    assert!(
        c.complete(first["id"].as_str().unwrap(), "code#state")
            .await
            .is_err()
    );
    assert!(
        c.complete(second["id"].as_str().unwrap(), "code#wrong-state")
            .await
            .is_err()
    );
    assert!(hits.lock().await.is_empty());
    login(&c).await?;
    let status = c.status().await.to_string();
    assert!(!status.contains("claude-test-access"));
    assert!(!status.contains("claude-test-refresh"));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(root.path().join("claude/account.json"))?
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    c.state.lock().await.credentials.as_mut().unwrap().expires = 0;
    let (a, b) = tokio::join!(c.access(), c.access());
    assert_eq!(a?, b?);
    assert_eq!(
        hits.lock()
            .await
            .iter()
            .filter(|v| v["body"]["grant_type"] == "refresh_token")
            .count(),
        1
    );
    c.disconnect().await?;
    assert!(c.access().await.is_err());
    assert!(!root.path().join("claude/account.json").exists());
    task.abort();
    Ok(())
}
#[tokio::test]
async fn gateway_routes_and_isolates_credentials() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (url, hits, task) = mock().await;
    let c = local(root.path(), &url);
    login(&c).await?;
    let bridge = bridge::start(c.clone()).await?;
    let client = reqwest::Client::new();
    let body = json!({"model":"claude-test","input":[{"role":"user","content":"Hi"}]});
    assert_eq!(
        client
            .post(format!("{}/responses", bridge.url))
            .body(body.to_string())
            .send()
            .await?
            .status(),
        403
    );
    let response = client
        .post(format!("{}/responses", bridge.url))
        .header("x-demodex-inference", &bridge.key)
        .header("x-demodex-openai-route", "api")
        .header("authorization", "Bearer sk-openai-test")
        .body(body.to_string())
        .send()
        .await?
        .text()
        .await?;
    assert!(response.contains("Mock Claude reply"), "{response}");
    assert!(response.contains("response.completed"));
    assert!(response.contains("\"input_tokens\":20"));
    let before = hits.lock().await.len();
    let rejected = client
        .post(format!("{}/responses", bridge.url))
        .header("x-demodex-inference", &bridge.key)
        .body(json!({"model":"gpt-test","input":[{"role":"user","content":"Hi"}]}).to_string())
        .send()
        .await?;
    assert_eq!(rejected.status(), 502);
    assert_eq!(hits.lock().await.len(), before);
    let seen = hits.lock().await;
    assert_eq!(
        seen.iter().find(|v| v["path"] == "/v1/messages").unwrap()["auth"],
        "Bearer claude-test-access"
    );
    assert!(
        !seen
            .iter()
            .any(|v| v["body"].to_string().contains(&bridge.key))
    );
    drop(seen);
    drop(bridge);
    task.abort();
    Ok(())
}
#[tokio::test]
async fn runtime_settings_keep_bridge_key_off_the_command_line() -> Result<()> {
    let root = tempfile::tempdir()?;
    let (url, _hits, task) = mock().await;
    let c = local(root.path(), &url);
    login(&c).await?;
    c.runtime_catalog().await?.context("catalogue")?;
    let settings = c.runtime_settings().await?;
    assert!(settings.key.len() >= 32);
    assert!(settings.overrides.iter().all(|o| !o.contains(&settings.key)));
    assert!(settings.overrides.iter().any(|o| o.contains("env_http_headers") && o.contains(super::INFERENCE_KEY_ENV)));
    c.shutdown().await;
    task.abort();
    Ok(())
}
#[test]
fn tool_and_reasoning_roundtrip() -> Result<()> {
    let request = json!({"model":"claude-test","tools":[{"type":"namespace","name":"functions","tools":[{"type":"function","name":"shell","parameters":{"type":"object"}},{"type":"custom","name":"patch"}]}],"input":[{"role":"user","content":[{"type":"input_text","text":"Edit"},{"type":"input_image","image_url":"data:image/png;base64,AA=="}]}]});
    let converted = translate::request(&request, &model())?;
    assert_eq!(converted.body["thinking"]["type"], "adaptive");
    let tool = converted
        .tools
        .values()
        .find(|t| t.name == "patch")
        .unwrap()
        .clone();
    let mut stream = translate::Stream::new(converted.tools);
    stream.accept(json!({"type":"message_start","message":{"usage":{"input_tokens":2,"cache_read_input_tokens":3,"cache_creation_input_tokens":5}}}))?;
    for v in [
        json!({"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":"","signature":""}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Plan"}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"signature_delta","signature":"signed"}}),
        json!({"type":"content_block_stop","index":0}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"call1","name":tool.alias,"input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"input\":\"patch text\"}"}}),
        json!({"type":"content_block_stop","index":1}),
    ] {
        stream.accept(v)?;
    }
    assert_eq!(stream.output[1]["namespace"], "functions");
    assert_eq!(stream.output[1]["input"], "patch text");
    let mut replay = request;
    replay["input"]
        .as_array_mut()
        .unwrap()
        .extend(stream.output.clone());
    replay["input"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"custom_tool_call_output","call_id":"call1","output":"done"}));
    let converted = translate::request(&replay, &model())?;
    assert_eq!(
        converted.body["messages"][1]["content"][0]["signature"],
        "signed"
    );
    assert_eq!(
        converted.body["messages"][1]["content"][1]["name"],
        tool.alias
    );
    assert_eq!(
        converted.body["messages"][2]["content"][0]["tool_use_id"],
        "call1"
    );
    stream.accept(json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"},"usage":{"output_tokens":4}}))?;
    assert!(stream.accept(json!({"type":"message_stop"})).is_err());
    Ok(())
}

#[tokio::test]
#[ignore = "requires local Codex binary; disposable profile and mock inference only"]
async fn real_codex_uses_claude_gateway() -> Result<()> {
    run_codex(true, "direct").await
}
#[tokio::test]
#[ignore = "requires local Codex; no account or external inference"]
async fn real_codex_claude_without_openai_login() -> Result<()> {
    run_codex(false, "code_mode_only").await
}
#[tokio::test]
#[ignore = "requires Codex 0.166 complete package; mock inference only"]
async fn real_codex_code_mode_claude_subagents() -> Result<()> {
    run_codex(true, "code_mode_only").await
}
async fn run_codex(openai: bool, mode: &str) -> Result<()> {
    let root = tempfile::tempdir()?;
    let profile = root.path().join("profile");
    std::fs::create_dir(&profile)?;
    let output = tokio::process::Command::new("codex")
        .args(["debug", "models", "--bundled"])
        .env("CODEX_HOME", &profile)
        .output()
        .await?;
    ensure!(
        output.status.success(),
        "Cannot load bundled Codex catalogue"
    );
    let mut catalog: Value = serde_json::from_slice(&output.stdout)?;
    let mut gpt = catalog["models"][0].clone();
    gpt["slug"] = json!("gpt-test");
    gpt["tool_mode"] = json!(mode);
    gpt["use_responses_lite"] = json!(mode != "direct");
    gpt["supports_search_tool"] = json!(false);
    catalog["models"].as_array_mut().unwrap().push(gpt);
    std::fs::write(
        profile.join("models_cache.json"),
        serde_json::to_vec(&catalog)?,
    )?;
    if openai {
        std::fs::write(
            profile.join("auth.json"),
            r#"{"OPENAI_API_KEY":"sk-openai-test"}"#,
        )?;
    }
    let (url, hits, task) = mock().await;
    let c = local(root.path(), &url);
    login(&c).await?;
    let catalogue = c.runtime_catalog().await?.unwrap();
    let mut generated: Value = serde_json::from_slice(&std::fs::read(&catalogue)?)?;
    assert!(
        generated["models"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["slug"].as_str().unwrap().starts_with("claude-"))
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(profile.join("models_cache.json"))?)?,
        catalog
    );
    for row in generated["models"].as_array_mut().unwrap() {
        if row["slug"] == "claude-test" {
            row["tool_mode"] = json!(mode);
        }
    }
    std::fs::write(&catalogue, serde_json::to_vec(&generated)?)?;
    let endpoint = format!("unix://{}", root.path().join("app.sock").display());
    let mut command = tokio::process::Command::new("codex");
    command
        .args(["app-server", "--listen", &endpoint])
        .arg("-c")
        .arg(format!(
            "model_catalog_json={}",
            serde_json::to_string(&catalogue)?
        ))
        .env("CODEX_HOME", &profile)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .current_dir(root.path())
        .kill_on_drop(true)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let settings = c.runtime_settings().await?;
    command.env(super::INFERENCE_KEY_ENV, &settings.key);
    for setting in settings.overrides {
        command.arg("-c").arg(setting);
    }
    let mut child = command.spawn()?;
    for _ in 0..100 {
        if root.path().join("app.sock").exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (rpc, mut events) = crate::rpc::Rpc::connect(&endpoint).await?;
    for (parent, prompt, child_model) in [
        ("claude-test", "Say hello", None),
        ("claude-test", "MOCK_SPAWN_GPT", Some("claude-test")),
    ] {
        if !openai && child_model.is_some() {
            continue;
        }
        let before = hits.lock().await.len();
        let mut params = json!({"model":parent,"cwd":root.path(),"approvalPolicy":"never","sandbox":"read-only","config":{"features":{"apps":false}}});
        let mut config = rpc
            .call("config/read", json!({"includeLayers":false}))
            .await?;
        config["account"] = rpc
            .call("account/read", json!({"refreshToken":false}))
            .await?["account"]
            .clone();
        c.configure(&mut params, &config, false).await?;
        let result = rpc.call("thread/start", params).await?;
        let thread = result["thread"]["id"].as_str().context("No test thread")?;
        let saved = rpc
            .call(
                "thread/read",
                json!({"threadId":thread,"includeTurns":false}),
            )
            .await?;
        assert_eq!(saved["thread"]["modelProvider"], "demodex_mixed");
        rpc.call(
            "turn/start",
            json!({"threadId":thread,"input":[{"type":"text","text":prompt,"text_elements":[]}]}),
        )
        .await?;
        let completed = tokio::time::timeout(Duration::from_secs(25), async {
            loop {
                let event = events.recv().await.context("Disconnected")??;
                if event["method"] == "turn/completed" && event["params"]["threadId"] == thread {
                    return Ok::<_, anyhow::Error>(event);
                }
            }
        })
        .await?;
        let completed = completed?;
        assert_eq!(
            completed["params"]["turn"]["status"], "completed",
            "{completed}"
        );
        if let Some(child_model) = child_model {
            let child_result = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    if hits.lock().await[before..].iter().any(|v| {
                        v["body"]["model"] == child_model
                            && v["body"]["messages"]
                                .to_string()
                                .contains("Message Type: NEW_TASK")
                    }) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            if child_result.is_err() {
                for v in &hits.lock().await[before..] {
                    eprintln!(
                        "MOCK FAILURE {}",
                        serde_json::to_string(
                            &json!({"messages":v["body"]["messages"],"input":v["body"]["input"]})
                        )
                        .unwrap()
                    );
                }
            }
            child_result.context(format!("Child {child_model} did not call mock inference"))?;
            let captured = hits.lock().await;
            let child_request = captured[before..]
                .iter()
                .find(|v| {
                    v["body"]["model"] == child_model
                        && v["body"]["messages"]
                            .to_string()
                            .contains("Mock child reply")
                })
                .unwrap();
            let content = if child_model.starts_with("claude-") {
                &child_request["body"]["messages"]
            } else {
                &child_request["body"]["input"]
            };
            assert!(
                content.to_string().contains("Mock child reply"),
                "Child task must arrive as plaintext: {content}"
            );
            if mode == "code_mode_only" {
                assert!(
                    captured[before..]
                        .iter()
                        .any(|v| v["body"].to_string().contains("ALL_TOOLS.find")),
                    "Parent must execute JavaScript delegation"
                );
            }
        }
        let captured = hits.lock().await;
        let request = captured
            .iter()
            .find(|v| v["path"] == "/v1/messages")
            .unwrap();
        for tool in request["body"]["tools"].as_array().unwrap() {
            if is_spawn_agent(tool) {
                assert!(
                    tool["description"]
                        .as_str()
                        .unwrap()
                        .contains("claude-test"),
                    "Claude missing from child model list"
                );
            }
        }
        drop(captured);
    }
    c.disconnect().await?;
    assert!(c.runtime_catalog().await?.is_none());
    // Logout must not overwrite the catalogue of a still-running app-server.
    assert!(
        !serde_json::from_slice::<Value>(&std::fs::read(&catalogue)?)?["models"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let mut configuration = rpc
        .call("config/read", json!({"includeLayers":false}))
        .await?;
    configuration["account"] = rpc
        .call("account/read", json!({"refreshToken":false}))
        .await?["account"]
        .clone();
    let mut params = json!({"model":"gpt-test","config":{}});
    assert!(
        c.configure(&mut params, &configuration, false)
            .await
            .is_err()
    );
    rpc.close();
    child.kill().await?;
    c.shutdown().await;
    task.abort();
    Ok(())
}

#[test]
fn plaintext_agent_schema_preserves_user_properties() -> Result<()> {
    let mut schema = json!({"type":"object","properties":{"message":{"type":"string","encrypted":true},"encrypted":{"type":"boolean"},"payload":{"type":"object","properties":{"encrypted":false}}}});
    translate::strip_schema_encryption(&mut schema);
    assert!(schema["properties"]["message"].get("encrypted").is_none());
    assert_eq!(schema["properties"]["encrypted"]["type"], "boolean");
    assert_eq!(
        schema["properties"]["payload"]["properties"]["encrypted"],
        false
    );
    let request = json!({"model":"claude-test","input":[{"type":"agent_message","author":"/root","recipient":"/root/child","content":[{"type":"input_text","text":"A task"}]}]});
    assert!(
        translate::request(&request, &model())?
            .body
            .to_string()
            .contains("A task")
    );
    Ok(())
}

#[tokio::test]
async fn authorization_code_is_not_saved_in_receipts() -> Result<()> {
    let root = tempfile::tempdir()?;
    let service = crate::Service::open(crate::Config {
        stt: None,
        data_dir: root.path().join("state"),
        host_workspace: None,
        codex_home: None,
        vm_image: None,
    })?;
    let id = uuid::Uuid::new_v4().to_string();
    let operation = demodex_protocol::Operation::CompleteClaudeLogin {
        login_id: "missing".into(),
        code: "sensitive-one-use-code#state".into(),
    };
    assert!(service.call(&id, operation.clone()).await.is_err());
    let receipt = service.manager.store.receipt(&id)?.unwrap();
    assert!(!receipt.0.contains("sensitive-one-use-code"));
    assert!(receipt.0.contains("code_sha256"));
    assert!(service.call(&id, operation).await.is_err());
    Ok(())
}

#[tokio::test]
async fn account_recovery_does_not_block_the_daemon() -> Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::create_dir(root.path().join("claude"))?;
    std::fs::write(
        root.path().join("claude/account.json"),
        "broken-secret-token-file",
    )?;
    let (url, _, task) = mock().await;
    let mut c = local(root.path(), &url);
    let status = c.status().await;
    assert_eq!(status["connected"], false);
    assert!(status["error"].is_string());
    assert!(!status.to_string().contains("broken-secret-token-file"));
    Arc::get_mut(&mut c).unwrap().api = "http://127.0.0.1:1".into();
    assert!(login(&c).await.is_err());
    assert_eq!(c.status().await["connected"], true);
    Arc::get_mut(&mut c).unwrap().api = url;
    let status = c.refresh_models().await?;
    assert_eq!(status["models"][0]["id"], "claude-test");
    assert!(status["error"].is_null());
    task.abort();
    Ok(())
}

fn flatten_tools(rows: &[Value], namespace: Option<Value>, out: &mut Vec<(Option<Value>, Value)>) {
    for tool in rows {
        if tool["type"] == "namespace" {
            flatten_tools(
                tool["tools"].as_array().unwrap(),
                Some(tool["name"].clone()),
                out,
            );
        } else {
            out.push((namespace.clone(), tool.clone()));
        }
    }
}
fn spawn_js(model: &str) -> String {
    format!(
        r#"const entry = ALL_TOOLS.find(t => t.name.endsWith('spawn_agent'));
if (!entry) throw new Error('spawn_agent missing: ' + JSON.stringify(ALL_TOOLS.map(t => t.name)));
text(await tools[entry.name]({{task_name:'child',message:'Mock child reply',model:{},reasoning_effort:'low',fork_turns:'none'}}));"#,
        serde_json::to_string(model).unwrap()
    )
}

#[test]
fn claude_omits_hosted_search_but_keeps_local_tools() -> Result<()> {
    let input = json!({"model":"claude-test","tools":[
        {"type":"web_search"}, {"type":"web_search_preview"},
        {"type":"function","name":"web_search","parameters":{"type":"object","properties":{}}}
    ],"input":[{"role":"user","content":"Hello"},{"type":"additional_tools","tools":[{"type":"web_search"}]}]});
    let translated = translate::request(&input, &model())?;
    assert_eq!(translated.tools.len(), 1);
    assert_eq!(translated.tools.values().next().unwrap().name, "web_search");
    assert_eq!(translated.body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(input["tools"][0]["type"], "web_search");
    Ok(())
}

#[test]
fn tool_names_are_readable_and_unknown_calls_are_surfaced() -> Result<()> {
    let request = json!({"model":"claude-test","tools":[
        {"type":"function","name":"exec_command","parameters":{"type":"object"}},
        {"type":"namespace","name":"demodex","tools":[{"type":"function","name":"notify","parameters":{"type":"object"}}]},
        {"type":"function","name":"bad name!","parameters":{"type":"object"}}],
        "input":[{"role":"user","content":"hi"}]});
    let converted = translate::request(&request, &model())?;
    let names: Vec<String> = converted.body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(names.contains(&"exec_command".to_owned()));
    assert!(names.contains(&"demodex__notify".to_owned()));
    assert!(names.iter().any(|n| n.starts_with("tool_")));
    // The namespaced tool is only callable by its full alias; a bare name is not guessed.
    let mut stream = translate::Stream::new(converted.tools);
    stream.accept(json!({"type":"message_start","message":{"usage":{}}}))?;
    for v in [
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"c1","name":"notify","input":{}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{}"}}),
        json!({"type":"content_block_stop","index":0}),
    ] {
        stream.accept(v)?;
    }
    assert_eq!(stream.output[0]["type"], "function_call");
    assert_eq!(stream.output[0]["name"], "notify");
    assert!(stream.output[0].get("namespace").is_none());
    stream.accept(json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":1}}))?;
    stream.accept(json!({"type":"message_stop"}))?;
    assert!(stream.finished);
    Ok(())
}

/// Tools are exposed under readable aliases (`spawn_agent` or `namespace__spawn_agent`);
/// only hashed aliases still carry the name as a description prefix.
fn is_spawn_agent(tool: &Value) -> bool {
    let name = tool["name"].as_str().unwrap_or("");
    name == "spawn_agent"
        || name.ends_with("__spawn_agent")
        || tool["description"].as_str().unwrap_or("").contains("spawn_agent:")
}
