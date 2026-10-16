//! OpenAI's Responses API (developers.openai.com/api/docs): ChatGPT's models.
//!
//! The PDF goes as an `input_file` (OpenAI reads its text and sees its pages). Nothing is kept on
//! OpenAI's side between questions (`store: false`): each one carries the conversation, every
//! answer's output items exactly as they came (its encrypted reasoning included), a
//! `function_call_output` for each change it proposed, then the question. OpenAI caches a
//! repeated prompt by itself.

use serde_json::{Value, json};

use super::{AiError, Call, Parsed, Provider, Reply, Request, Usage};

#[cfg(not(target_arch = "wasm32"))]
pub(super) const URL: &str = "https://api.openai.com/v1/responses";
/// Room for the answer and the reasoning before it (OpenAI suggests leaving at least 25,000
/// tokens for both).
const MAX_OUTPUT: u32 = 25_000;
/// The error codes of a 429 that is about money, not speed.
const BILLING: [&str; 4] =
    ["credit_balance_exhausted", "organization_spend_limit_exceeded", "project_spend_limit_exceeded", "organization_usage_limit_exceeded"];

pub(super) fn body(req: &Request) -> Value {
    let mut prompts = req.prompts();
    let first = prompts.next().unwrap_or_default();
    let file = json!({
        "type": "input_file",
        "filename": super::file_name(req.title),
        "file_data": format!("data:application/pdf;base64,{}", super::base64(req.pdf)),
    });
    let mut input = vec![json!({"role": "user", "content": [file, {"type": "input_text", "text": first}]})];
    for (turn, prompt) in req.history.iter().zip(prompts) {
        if let Some(items) = turn.answer.as_array() {
            input.extend(items.iter().cloned());
        }
        // Every function call gets its output (OpenAI refuses a conversation that leaves one out).
        input.extend(calls(&turn.answer).iter().map(|c| json!({"type": "function_call_output", "call_id": c.id, "output": turn.result(&c.id)})));
        input.push(json!({"role": "user", "content": [{"type": "input_text", "text": prompt}]}));
    }
    let mut body = json!({
        "model": req.model.trim(),
        "instructions": req.system(),
        "input": input,
        "max_output_tokens": MAX_OUTPUT,
        "reasoning": {"effort": "medium"},
        "store": false,
    });
    if req.tool()
        && let Some(map) = body.as_object_mut()
    {
        // Not strict: strict mode takes only part of JSON Schema, and proposals are checked here.
        map.insert(
            "tools".into(),
            json!([{"type": "function", "name": super::EDIT_TOOL, "description": super::EDIT_DESCRIPTION, "parameters": super::edit_schema(false), "strict": false}]),
        );
    }
    body
}

/// The `function_call` items of an answer (its arguments are a JSON string).
pub(super) fn calls(answer: &Value) -> Vec<Call> {
    let Some(items) = answer.as_array() else { return Vec::new() };
    items
        .iter()
        .filter(|i| i["type"] == "function_call")
        .filter_map(|i| {
            Some(Call {
                id: i["call_id"].as_str()?.to_owned(),
                name: i["name"].as_str().unwrap_or_default().to_owned(),
                args: super::arguments(&i["arguments"]),
            })
        })
        .collect()
}

pub(super) fn parse(status: u16, body: &str) -> Result<Reply, AiError> {
    if !(200..300).contains(&status) {
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let message = super::error_message(&v);
        let code = v["error"]["code"].as_str().unwrap_or_default();
        return Err(match status {
            429 if BILLING.contains(&code) || v["error"]["type"] == "insufficient_quota" => AiError::NotAllowed(message),
            _ => super::error_for(status, message),
        });
    }
    let v: Value = serde_json::from_str(body).map_err(|e| AiError::Parse(e.to_string()))?;
    let state = v["status"].as_str().unwrap_or_default();
    let why = v["incomplete_details"]["reason"].as_str().unwrap_or_default();
    if matches!(state, "failed" | "cancelled") {
        let message = super::error_message(&v);
        return Err(AiError::Api(if message.is_empty() { state.to_owned() } else { message }));
    }
    if why == "content_filter" {
        return Err(AiError::Refused("content_filter".into()));
    }
    let output = v.get("output").filter(|o| o.is_array()).cloned().ok_or_else(|| AiError::Parse("no output".into()))?;
    let parts: Vec<&Value> = output
        .as_array()
        .into_iter()
        .flatten()
        .filter(|i| i["type"] == "message")
        .flat_map(|i| i["content"].as_array().into_iter().flatten())
        .collect();
    // A declined request: the message says so instead of answering.
    if let Some(refusal) = parts.iter().find(|p| p["type"] == "refusal") {
        let why = refusal["refusal"].as_str().or_else(|| refusal["text"].as_str()).unwrap_or("policy");
        return Err(AiError::Refused(why.chars().take(200).collect()));
    }
    let text = parts.iter().filter(|p| p["type"] == "output_text").filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n\n");
    // `input_tokens` counts the cached tokens too.
    let n = |path: &[&str]| path.iter().fold(&v["usage"], |u, k| &u[*k]).as_u64().unwrap_or(0);
    let (read, write) = (n(&["input_tokens_details", "cached_tokens"]), n(&["input_tokens_details", "cache_write_tokens"]));
    Ok(Parsed {
        text,
        calls: calls(&output),
        content: output,
        usage: Usage {
            input: n(&["input_tokens"]).saturating_sub(read).saturating_sub(write),
            output: n(&["output_tokens"]),
            cache_write: write,
            cache_read: read,
        },
        truncated: state == "incomplete" && why == "max_output_tokens",
        model: v["model"].as_str().unwrap_or_default().to_owned(),
        fell_back: false,
    }
    .reply(Provider::OpenAi))
}
