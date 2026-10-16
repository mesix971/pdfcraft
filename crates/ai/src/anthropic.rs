//! Anthropic's Messages API (Claude).
//!
//! The document goes as a PDF `document` block, so Claude reads its text and sees its pages
//! (scans included). The block is marked for prompt caching: a follow-up question about the same
//! document reads it back from the cache for a fraction of the first request's price, and so is
//! the conversation's newest question.

use serde_json::{Value, json};

use super::{AiError, Call, Parsed, Provider, Reply, Request, Usage};

#[cfg(not(target_arch = "wasm32"))]
const URL: &str = "https://api.anthropic.com/v1/messages";
#[cfg(not(target_arch = "wasm32"))]
const API_VERSION: &str = "2023-06-01";
/// Server-side refusal fallbacks in their `"default"` form: a declined request is re-run on the
/// model Anthropic recommends for that kind of refusal, in the same call.
#[cfg(not(target_arch = "wasm32"))]
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// Room for the answer. A request this size stays well inside the HTTP timeout without streaming.
const MAX_TOKENS: u32 = 16_000;

/// The model asked falls back to another one when it declines.
fn fallbacks(req: &Request) -> bool {
    super::known(Provider::Anthropic, req.model).is_some_and(super::Known::fallbacks)
}

pub(super) fn body(req: &Request) -> Value {
    let document = json!({
        "type": "document",
        "source": {"type": "base64", "media_type": "application/pdf", "data": super::base64(req.pdf)},
        "title": req.title,
        // The prefix up to here (system prompt and document) is read back from the cache by
        // every later question about this document.
        "cache_control": {"type": "ephemeral"},
    });
    // The first question rides with the document; later ones follow the answers.
    let mut prompts = req.prompts();
    let first = prompts.next().unwrap_or_default();
    let mut messages = vec![json!({"role": "user", "content": [document, {"type": "text", "text": first}]})];
    for (turn, prompt) in req.history.iter().zip(prompts) {
        messages.push(json!({"role": "assistant", "content": turn.answer}));
        // What became of the answer's proposals comes first, one result for each (the API
        // refuses a conversation that leaves one out), then the question.
        let mut content: Vec<Value> =
            calls(&turn.answer).iter().map(|c| json!({"type": "tool_result", "tool_use_id": c.id, "content": turn.result(&c.id)})).collect();
        content.push(json!({"type": "text", "text": prompt}));
        messages.push(json!({"role": "user", "content": content}));
    }
    // The conversation so far is cached too, so the next question reads it back instead of
    // paying for it again (the marker on the newest question; earlier ones stay read points).
    if let Some(block) = messages.last_mut().and_then(|m| m["content"].as_array_mut()).and_then(|c| c.last_mut()).and_then(Value::as_object_mut)
        && block.get("type").and_then(Value::as_str) == Some("text")
    {
        block.insert("cache_control".into(), json!({"type": "ephemeral"}));
    }
    let mut body = json!({
        "model": req.model.trim(),
        "max_tokens": MAX_TOKENS,
        "output_config": {"effort": "medium"},
        "system": req.system(),
        "messages": messages,
    });
    if let Some(map) = body.as_object_mut() {
        if req.tool() {
            map.insert(
                "tools".into(),
                json!([{"name": super::EDIT_TOOL, "description": super::EDIT_DESCRIPTION, "input_schema": super::edit_schema(true)}]),
            );
        }
        if fallbacks(req) {
            map.insert("fallbacks".into(), json!("default"));
        }
    }
    body
}

/// The request, with its key and version headers.
#[cfg(not(target_arch = "wasm32"))]
pub(super) fn post(agent: &ureq::Agent, req: &Request) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
    let request = agent.post(URL).header("x-api-key", req.key.trim()).header("anthropic-version", API_VERSION);
    if fallbacks(req) { request.header("anthropic-beta", FALLBACK_BETA) } else { request }
}

/// The `tool_use` blocks of an answer.
pub(super) fn calls(answer: &Value) -> Vec<Call> {
    let Some(blocks) = answer.as_array() else { return Vec::new() };
    blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .filter_map(|b| {
            Some(Call { id: b["id"].as_str()?.to_owned(), name: b["name"].as_str().unwrap_or_default().to_owned(), args: b["input"].clone() })
        })
        .collect()
}

pub(super) fn parse(status: u16, body: &str) -> Result<Reply, AiError> {
    if !(200..300).contains(&status) {
        // An error page from a proxy or a captive portal isn't JSON: the status says enough.
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let message = super::error_message(&v);
        return Err(match v["error"]["type"].as_str().unwrap_or_default() {
            "authentication_error" => AiError::InvalidKey,
            "permission_error" | "billing_error" => AiError::NotAllowed(message),
            "rate_limit_error" => AiError::RateLimited,
            "overloaded_error" => AiError::Overloaded,
            _ => super::error_for(status, message),
        });
    }
    let v: Value = serde_json::from_str(body).map_err(|e| AiError::Parse(e.to_string()))?;
    // Check the stop reason before the content: a declined request has nothing to read.
    let stop = v["stop_reason"].as_str().unwrap_or_default();
    if stop == "refusal" {
        let category = v["stop_details"]["category"].as_str().unwrap_or("policy").chars().take(super::MAX_CATEGORY).collect();
        return Err(AiError::Refused(category));
    }
    let content = v.get("content").filter(|c| c.is_array()).cloned().ok_or_else(|| AiError::Parse("no content".into()))?;
    let text = content
        .as_array()
        .map(|blocks| blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n\n"))
        .unwrap_or_default();
    let n = |field: &str| v["usage"][field].as_u64().unwrap_or(0);
    Ok(Parsed {
        text,
        calls: calls(&content),
        content,
        usage: Usage {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cache_write: n("cache_creation_input_tokens"),
            cache_read: n("cache_read_input_tokens"),
        },
        truncated: matches!(stop, "max_tokens" | "model_context_window_exceeded"),
        model: v["model"].as_str().unwrap_or_default().to_owned(),
        // The served-by signal: a fallback model's attempt in the per-attempt usage.
        fell_back: v["usage"]["iterations"].as_array().is_some_and(|i| i.iter().any(|e| e["type"] == "fallback_message")),
    }
    .reply(Provider::Anthropic))
}
