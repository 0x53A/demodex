//! Responses <-> Messages conversion. Unknown content/tools fail explicitly.
use anyhow::{Context, Result, bail, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Clone)]
pub(super) struct Tool {
    pub name: String,
    pub namespace: Option<String>,
    pub custom: bool,
    pub alias: String,
    pub schema: Value,
    pub description: String,
}
/// Name Claude sees and calls. Anthropic names are flat `[A-Za-z0-9_-]{1,64}`, so a
/// namespaced tool becomes `namespace__name` (the same shape Claude Code uses for MCP
/// tools). The description repeats exactly this string, so what the model reads is
/// what it must call. Names that cannot be expressed fall back to a stable hash.
fn alias(namespace: Option<&str>, name: &str) -> String {
    let readable = match namespace {
        Some(n) => format!("{n}__{name}"),
        None => name.to_owned(),
    };
    if !readable.is_empty()
        && readable.len() <= 64
        && readable
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return readable;
    }
    let key = format!("{}:{name}", namespace.unwrap_or(""));
    format!(
        "tool_{}",
        Sha256::digest(key.as_bytes())
            .iter()
            .take(16)
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}
fn tools(rows: &[Value], namespace: Option<&str>, out: &mut HashMap<String, Tool>) -> Result<()> {
    for v in rows {
        let kind = v["type"].as_str().unwrap_or("function");
        // Responses web search runs on OpenAI's servers, not in Codex.
        // Omit it only from Claude's tool catalogue until a search adapter exists.
        if matches!(kind, "web_search" | "web_search_preview") {
            continue;
        }

        if kind == "namespace" {
            tools(
                v["tools"].as_array().context("Namespace has no tools")?,
                v["name"].as_str(),
                out,
            )?;
            continue;
        }
        ensure!(
            matches!(kind, "function" | "custom"),
            "Unsupported Claude tool type: {kind}"
        );
        let name = v["name"].as_str().context("Tool missing name")?.to_owned();
        let id = alias(namespace, &name);
        let mut schema = if kind == "custom" {
            json!({"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false})
        } else {
            v.get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type":"object","properties":{}}))
        };
        strip_schema_encryption(&mut schema);
        ensure!(
            !out.contains_key(&id),
            "Two Claude tools share the name {id}"
        );
        out.insert(
            id.clone(),
            Tool {
                name,
                namespace: namespace.map(str::to_owned),
                custom: kind == "custom",
                alias: id,
                schema,
                description: v["description"].as_str().unwrap_or("").to_owned(),
            },
        );
    }
    Ok(())
}
fn blocks(content: &Value) -> Result<Vec<Value>> {
    if let Some(s) = content.as_str() {
        return Ok(vec![json!({"type":"text","text":s})]);
    }
    let mut out = Vec::new();
    for v in content.as_array().context("Invalid message content")? {
        match v["type"].as_str() {
            Some("input_text" | "output_text" | "text") => {
                out.push(json!({"type":"text","text":v["text"]}))
            }
            Some("input_image") => {
                let url = v["image_url"].as_str().context("Image needs a URL")?;
                let (mime, data) = url
                    .strip_prefix("data:")
                    .and_then(|s| s.split_once(";base64,"))
                    .context("Claude bridge accepts inline images only")?;
                ensure!(
                    matches!(
                        mime,
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    ),
                    "Unsupported image format"
                );
                out.push(json!({"type":"image","source":{"type":"base64","media_type":mime,"data":data}}));
            }
            other => bail!("Unsupported Claude content block: {other:?}"),
        }
    }
    Ok(out)
}
fn push(messages: &mut Vec<Value>, role: &str, content: Vec<Value>) {
    if content.is_empty() {
        return;
    }
    if let Some(last) = messages.last_mut().filter(|m| m["role"] == role) {
        last["content"].as_array_mut().unwrap().extend(content);
    } else {
        messages.push(json!({"role":role,"content":content}));
    }
}
pub(super) struct Request {
    pub body: Value,
    pub tools: HashMap<String, Tool>,
}
pub(super) fn request(input: &Value, model_info: &Value) -> Result<Request> {
    ensure!(
        input["previous_response_id"].is_null(),
        "Claude bridge requires complete input history"
    );
    let model = input["model"].as_str().context("Missing model")?;
    let mut catalog = HashMap::new();
    tools(
        input["tools"].as_array().map(Vec::as_slice).unwrap_or(&[]),
        None,
        &mut catalog,
    )?;
    let mut system = vec![
        json!({"type":"text","text":"You are Claude Code, Anthropic's official CLI for Claude."}),
    ];
    if let Some(s) = input["instructions"].as_str().filter(|s| !s.is_empty()) {
        system.push(json!({"type":"text","text":s}));
    }
    let mut messages = Vec::new();
    let string_input;
    let rows = if let Some(s) = input["input"].as_str() {
        string_input = vec![json!({"role":"user","content":s})];
        &string_input
    } else {
        input["input"].as_array().context("Missing input history")?
    };
    for v in rows {
        match v["type"].as_str().unwrap_or("message") {
            "agent_message" => {
                let mut content = vec![
                    json!({"type":"text","text":format!("Agent message from {} to {}:",v["author"],v["recipient"])}),
                ];
                content.extend(blocks(&v["content"])?);
                push(&mut messages, "user", content);
            }
            "message" => {
                let role = v["role"].as_str().context("Missing message role")?;
                let content = blocks(&v["content"])?;
                if matches!(role, "system" | "developer") {
                    ensure!(
                        content.iter().all(|v| v["type"] == "text"),
                        "System content must be text"
                    );
                    system.extend(content);
                } else {
                    ensure!(matches!(role, "user" | "assistant"), "Unsupported role");
                    push(&mut messages, role, content);
                }
            }
            "function_call" | "custom_tool_call" => {
                let name = v["name"].as_str().context("Missing tool name")?;
                let a = alias(v["namespace"].as_str(), name);
                let args = if v["type"] == "custom_tool_call" {
                    json!({"input":v["input"]})
                } else {
                    serde_json::from_str(v["arguments"].as_str().context("Missing tool arguments")?)
                        .context("Invalid tool arguments")?
                };
                push(
                    &mut messages,
                    "assistant",
                    vec![json!({"type":"tool_use","id":v["call_id"],"name":a,"input":args})],
                );
            }
            "function_call_output" | "custom_tool_call_output" => {
                let c = if v["output"].is_string() {
                    v["output"].clone()
                } else {
                    json!(blocks(&v["output"])?)
                };
                push(
                    &mut messages,
                    "user",
                    vec![json!({"type":"tool_result","tool_use_id":v["call_id"],"content":c})],
                );
            }
            "reasoning" => {
                if let Some(encoded) = v["encrypted_content"]
                    .as_str()
                    .and_then(|s| s.strip_prefix("demodex-anthropic-v1:"))
                {
                    let b: Value = serde_json::from_slice(
                        &STANDARD
                            .decode(encoded)
                            .context("Invalid saved Claude reasoning")?,
                    )?;
                    ensure!(
                        matches!(b["type"].as_str(), Some("thinking" | "redacted_thinking")),
                        "Invalid Claude reasoning block"
                    );
                    push(&mut messages, "assistant", vec![b]);
                }
                // Provider-specific encrypted GPT reasoning cannot be replayed to Claude.
            }
            "additional_tools" => tools(
                v["tools"].as_array().context("Missing additional tools")?,
                None,
                &mut catalog,
            )?,
            other => bail!("Unsupported Claude history item: {other}"),
        }
    }
    ensure!(!messages.is_empty(), "Empty Claude conversation");
    let mut tool_list:Vec<_>=catalog.values().map(|t|json!({"name":t.alias,"description":if t.alias==t.name||t.namespace.as_deref().is_some_and(|n|t.alias==format!("{n}__{}",t.name)){t.description.clone()}else{format!("{}: {}",t.name,t.description)},"input_schema":t.schema})).collect();
    tool_list.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    if let Some(last) = system.last_mut() {
        last["cache_control"] = json!({"type":"ephemeral"});
    }
    if let Some(last) = tool_list.last_mut() {
        last["cache_control"] = json!({"type":"ephemeral"});
    }
    if let Some(last) = messages
        .last_mut()
        .and_then(|m| m["content"].as_array_mut())
        .and_then(|b| b.last_mut())
    {
        if !matches!(
            last["type"].as_str(),
            Some("thinking" | "redacted_thinking")
        ) {
            last["cache_control"] = json!({"type":"ephemeral"});
        }
    }
    let budget = match input["reasoning"]["effort"].as_str() {
        Some("low" | "minimal") => 1024,
        Some("high" | "xhigh" | "max") => 8192,
        _ => 4096,
    };
    let limit = model_info["max_tokens"]
        .as_u64()
        .filter(|n| *n >= 2048)
        .unwrap_or(8192);
    let max = input["max_output_tokens"]
        .as_u64()
        .unwrap_or(limit.min(32000));
    ensure!(
        (1024..=limit).contains(&max),
        "Claude output budget exceeds the model limit"
    );
    let mut body =
        json!({"model":model,"max_tokens":max,"system":system,"messages":messages,"stream":true});
    if input["reasoning"]["effort"].as_str() != Some("none") {
        if model_info["capabilities"]["thinking"]["types"]["adaptive"]["supported"] == true {
            body["thinking"] = json!({"type":"adaptive"});
            body["output_config"] =
                json!({"effort":input["reasoning"]["effort"].as_str().unwrap_or("medium")});
        } else if model_info["capabilities"]["thinking"]["supported"] != false {
            body["thinking"] = json!({"type":"enabled","budget_tokens":budget.min(max-1)});
        }
    }
    if !tool_list.is_empty() {
        body["tools"] = json!(tool_list);
    }
    match input["tool_choice"].as_str() {
        None | Some("auto") => {}
        Some("none") => {
            body.as_object_mut().unwrap().remove("tools");
        }
        Some("required") => {
            body["tool_choice"] = json!({"type":"any"});
            body.as_object_mut().unwrap().remove("thinking");
        }
        _ => bail!("Unsupported Claude tool_choice"),
    }
    Ok(Request {
        body,
        tools: catalog,
    })
}

pub(super) struct Stream {
    pub tools: HashMap<String, Tool>,
    pub output: Vec<Value>,
    blocks: HashMap<u64, (usize, Value, String)>,
    pub id: String,
    sequence: u64,
    pub usage: Value,
    pub finished: bool,
    stop: String,
}
impl Stream {
    pub fn new(tools: HashMap<String, Tool>) -> Self {
        Self {
            tools,
            output: Vec::new(),
            blocks: HashMap::new(),
            id: format!("resp_{}", uuid::Uuid::new_v4().simple()),
            sequence: 0,
            usage: json!({}),
            finished: false,
            stop: String::new(),
        }
    }
    pub fn event(&mut self, mut v: Value) -> String {
        v["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        format!(
            "event: {}\ndata: {}\n\n",
            v["type"].as_str().unwrap_or("error"),
            v
        )
    }
    fn response(&self, status: &str) -> Value {
        let mut usage = self.usage.clone();
        usage["input_tokens"] = json!(
            usage["total_tokens"]
                .as_u64()
                .unwrap_or(0)
                .saturating_sub(usage["output_tokens"].as_u64().unwrap_or(0))
        );
        json!({"id":self.id,"object":"response","status":status,"output":self.output,"usage":usage})
    }
    pub fn accept(&mut self, v: Value) -> Result<Vec<String>> {
        let mut events = Vec::new();
        match v["type"].as_str() {
            Some("message_start") => {
                self.merge_usage(&v["message"]["usage"]);
                events.push(
                    json!({"type":"response.created","response":self.response("in_progress")}),
                );
            }
            Some("content_block_start") => {
                let index = v["index"].as_u64().context("Missing content index")?;
                let block = v["content_block"].clone();
                let pos = self.output.len();
                let id = format!("item_{}_{}", self.id, pos);
                let mut item = match block["type"].as_str() {
                    Some("text") => {
                        json!({"type":"message","id":id,"role":"assistant","status":"in_progress","content":[]})
                    }
                    Some("thinking" | "redacted_thinking") => {
                        json!({"type":"reasoning","id":id,"summary":[]})
                    }
                    Some("tool_use") => {
                        let called = block["name"].as_str().context("Missing tool name")?;
                        if let Some(t) = self.tools.get(called) {
                            let mut item = if t.custom {
                                json!({"type":"custom_tool_call","id":id,"call_id":block["id"],"name":t.name,"input":""})
                            } else {
                                json!({"type":"function_call","id":id,"call_id":block["id"],"name":t.name,"arguments":"","encrypted_function_args":[]})
                            };
                            if let Some(n) = &t.namespace {
                                item["namespace"] = json!(n);
                            }
                            item
                        } else {
                            // Not in this request's catalogue (typically the model used a
                            // bare or invented name). Never guess a target: surface the call
                            // exactly as emitted so Codex rejects it with an ordinary
                            // tool error the model sees and can correct, instead of
                            // aborting the stream.
                            let mut names: Vec<&str> =
                                self.tools.keys().map(String::as_str).collect();
                            names.sort_unstable();
                            tracing::error!(
                                tool = called,
                                catalog_size = names.len(),
                                catalog = ?names,
                                "Claude called a tool outside the catalogue; returning it as an unsupported call"
                            );
                            json!({"type":"function_call","id":id,"call_id":block["id"],"name":called,"arguments":"","encrypted_function_args":[]})
                        }
                    }
                    other => bail!("Unsupported Claude output block {other:?}"),
                };
                item["status"] = json!("in_progress");
                self.output.push(item.clone());
                self.blocks.insert(index, (pos, block, String::new()));
                events.push(
                    json!({"type":"response.output_item.added","output_index":pos,"item":item}),
                );
                if item["type"] == "message" {
                    events.push(json!({"type":"response.content_part.added","output_index":pos,"item_id":id,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}}));
                }
            }
            Some("content_block_delta") => {
                let (pos, b, raw) = self
                    .blocks
                    .get_mut(&v["index"].as_u64().context("Missing delta index")?)
                    .context("Unknown delta index")?;
                let d = &v["delta"];
                let item = &self.output[*pos];
                match d["type"].as_str() {
                    Some("text_delta") => {
                        let s = d["text"].as_str().context("Missing text delta")?;
                        raw.push_str(s);
                        events.push(json!({"type":"response.output_text.delta","output_index":pos,"item_id":item["id"],"content_index":0,"delta":s}));
                    }
                    Some("thinking_delta") => {
                        let s = d["thinking"].as_str().context("Missing thinking delta")?;
                        raw.push_str(s);
                        events.push(json!({"type":"response.reasoning_summary_text.delta","output_index":pos,"item_id":item["id"],"summary_index":0,"delta":s}));
                    }
                    Some("signature_delta") => {
                        let old = b["signature"].as_str().unwrap_or("");
                        b["signature"] = json!(format!(
                            "{old}{}",
                            d["signature"].as_str().context("Missing signature")?
                        ));
                    }
                    Some("input_json_delta") => {
                        let s = d["partial_json"]
                            .as_str()
                            .context("Missing argument delta")?;
                        raw.push_str(s);
                        if item["type"] == "function_call" {
                            events.push(json!({"type":"response.function_call_arguments.delta","output_index":pos,"item_id":item["id"],"delta":s}));
                        }
                    }
                    other => bail!("Unsupported Claude stream delta {other:?}"),
                }
            }
            Some("content_block_stop") => {
                let (pos, mut b, raw) = self
                    .blocks
                    .remove(&v["index"].as_u64().context("Missing stop index")?)
                    .context("Unknown stop index")?;
                let item = &mut self.output[pos];
                item["status"] = json!("completed");
                match b["type"].as_str() {
                    Some("text") => {
                        item["content"] =
                            json!([{"type":"output_text","text":raw,"annotations":[]}]);
                        events.push(json!({"type":"response.output_text.done","output_index":pos,"item_id":item["id"],"content_index":0,"text":raw}));
                    }
                    Some("thinking" | "redacted_thinking") => {
                        if b["type"] == "thinking" {
                            b["thinking"] = json!(raw);
                            item["summary"] = json!([{"type":"summary_text","text":raw}]);
                        }
                        item["encrypted_content"] = json!(format!(
                            "demodex-anthropic-v1:{}",
                            STANDARD.encode(serde_json::to_vec(&b)?)
                        ));
                    }
                    Some("tool_use") => {
                        let args: Value = if raw.is_empty() {
                            b["input"].clone()
                        } else {
                            serde_json::from_str(&raw).context("Invalid streamed tool arguments")?
                        };
                        if item["type"] == "custom_tool_call" {
                            item["input"] = args["input"].clone();
                            ensure!(
                                item["input"].is_string(),
                                "Custom tool input must be a string"
                            );
                        } else {
                            item["arguments"] = json!(args.to_string());
                        }
                    }
                    _ => unreachable!(),
                }
                events.push(
                    json!({"type":"response.output_item.done","output_index":pos,"item":item}),
                );
            }
            Some("message_delta") => {
                self.merge_usage(&v["usage"]);
                self.stop = v["delta"]["stop_reason"].as_str().unwrap_or("").into();
            }
            Some("message_stop") => {
                ensure!(
                    self.blocks.is_empty(),
                    "Claude stream ended with unfinished blocks"
                );
                ensure!(
                    matches!(
                        self.stop.as_str(),
                        "end_turn" | "tool_use" | "stop_sequence"
                    ),
                    "Claude stopped before completing the response ({})",
                    self.stop
                );
                self.finished = true;
                events.push(
                    json!({"type":"response.completed","response":self.response("completed")}),
                );
            }
            Some("ping") => {}
            Some("error") => bail!(
                "Claude stream reported {}",
                v["error"]["type"].as_str().unwrap_or("an upstream error")
            ),
            other => bail!("Unsupported Claude stream event {other:?}"),
        }
        Ok(events.into_iter().map(|v| self.event(v)).collect())
    }
    fn merge_usage(&mut self, v: &Value) {
        for key in [
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ] {
            if let Some(n) = v[key].as_u64() {
                self.usage[key] = json!(n);
            }
        }
        let n = |k: &str| self.usage[k].as_u64().unwrap_or(0);
        let input =
            n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
        let output = n("output_tokens");
        let cached = n("cache_read_input_tokens");
        self.usage["total_tokens"] = json!(input + output);
        self.usage["input_tokens_details"] = json!({"cached_tokens":cached});
        // Anthropic input_tokens excludes cached tokens; keep raw until completion.
    }
    pub fn failure(&mut self, error: &str) -> String {
        self.event(json!({"type":"response.failed","response":{"id":self.id,"status":"failed","error":{"code":"provider_error","message":error}}}))
    }
}

// OpenAI encrypted string schemas are provider-specific. Mixed-agent tasks must
// remain readable by the receiving provider; reasoning ciphertext stays separate.
pub(super) fn strip_schema_encryption(v: &mut Value) {
    match v {
        Value::Object(map) => {
            map.remove("encrypted");
            for (key, value) in map.iter_mut() {
                if matches!(
                    key.as_str(),
                    "properties" | "$defs" | "definitions" | "patternProperties"
                ) {
                    if let Some(properties) = value.as_object_mut() {
                        for schema in properties.values_mut() {
                            strip_schema_encryption(schema);
                        }
                    }
                } else {
                    strip_schema_encryption(value);
                }
            }
        }
        Value::Array(rows) => {
            for value in rows {
                strip_schema_encryption(value);
            }
        }
        _ => {}
    }
}
