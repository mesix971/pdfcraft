//! Google's Gemini, through its Interactions API (ai.google.dev/api/interactions-api), which
//! Google recommends for new projects.
//!
//! The PDF goes as a `document` (Gemini reads its text and sees its pages). Nothing is kept on
//! Google's side between questions (`store: false`): each one carries the conversation, every
//! answer's steps exactly as they came (its thoughts and their signatures included), a
//! `function_result` for each change it proposed, then the question. Gemini caches a repeated
//! prompt by itself.

use serde_json::{Value, json};

use super::{AiError, Call, Parsed, Provider, Reply, Request, Usage};

#[cfg(not(target_arch = "wasm32"))]
pub(super) const URL: &str = "https://generativelanguage.googleapis.com/v1beta/interactions";
/// Room for the answer and the thinking before it.
const MAX_OUTPUT: u32 = 25_000;
/// The steps that are the model's own: the answer kept for the next question. The user's input
/// and the function results are the conversation's, sent again from it.
const NOT_THE_MODELS: [&str; 2] = ["user_input", "function_result"];

pub(super) fn body(req: &Request) -> Value {
    let mut prompts = req.prompts();
    let first = prompts.next().unwrap_or_default();
    let document = json!({"type": "document", "data": super::base64(req.pdf), "mime_type": "application/pdf"});
    let mut input = vec![json!({"type": "user_input", "content": [document, {"type": "text", "text": first}]})];
    for (turn, prompt) in req.history.iter().zip(prompts) {
        if let Some(steps) = turn.answer.as_array() {
            input.extend(steps.iter().cloned());
        }
        input.extend(
            calls(&turn.answer).iter().map(
                |c| json!({"type": "function_result", "name": c.name, "call_id": c.id, "result": [{"type": "text", "text": turn.result(&c.id)}]}),
            ),
        );
        input.push(json!({"type": "user_input", "content": [{"type": "text", "text": prompt}]}));
    }
    let mut body = json!({
        "model": req.model.trim(),
        "system_instruction": req.system(),
        "input": input,
        "store": false,
        "generation_config": {"max_output_tokens": MAX_OUTPUT},
    });
    if req.tool()
        && let Some(map) = body.as_object_mut()
    {
        map.insert(
            "tools".into(),
            json!([{"type": "function", "name": super::EDIT_TOOL, "description": super::EDIT_DESCRIPTION, "parameters": super::edit_schema(false)}]),
        );
    }
    body
}

/// The `function_call` steps of an answer (its arguments are an object).
pub(super) fn calls(answer: &Value) -> Vec<Call> {
    let Some(steps) = answer.as_array() else { return Vec::new() };
    steps
        .iter()
        .filter(|s| s["type"] == "function_call")
        .filter_map(|s| {
            Some(Call {
                id: s["id"].as_str()?.to_owned(),
                name: s["name"].as_str().unwrap_or_default().to_owned(),
                args: super::arguments(&s["arguments"]),
            })
        })
        .collect()
}

pub(super) fn parse(status: u16, body: &str) -> Result<Reply, AiError> {
    if !(200..300).contains(&status) {
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        return Err(super::error_for(status, super::error_message(&v)));
    }
    let v: Value = serde_json::from_str(body).map_err(|e| AiError::Parse(e.to_string()))?;
    let state = v["status"].as_str().unwrap_or_default();
    if matches!(state, "failed" | "cancelled") {
        let message = super::error_message(&v);
        return Err(AiError::Api(if message.is_empty() { state.to_owned() } else { message }));
    }
    let steps: Vec<Value> =
        v["steps"].as_array().into_iter().flatten().filter(|s| !NOT_THE_MODELS.iter().any(|t| s["type"] == *t)).cloned().collect();
    let text = steps
        .iter()
        .filter(|s| s["type"] == "model_output")
        .flat_map(|s| s["content"].as_array().into_iter().flatten())
        .filter(|c| c["type"] == "text")
        .filter_map(|c| c["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    let content = Value::Array(steps);
    // Counted as Gemini's generateContent documents them: the prompt's tokens include the cached
    // ones, and the thoughts are counted apart from the answer (both are billed as output).
    let n = |field: &str| v["usage"][field].as_u64().unwrap_or(0);
    let read = n("total_cached_tokens");
    Ok(Parsed {
        text,
        calls: calls(&content),
        content,
        usage: Usage {
            input: n("total_input_tokens").saturating_sub(read),
            output: n("total_output_tokens").saturating_add(n("total_thought_tokens")),
            cache_write: 0,
            cache_read: read,
        },
        // Gemini's "completed, but incomplete" (the length limit).
        truncated: state == "incomplete",
        model: v["model"].as_str().map(|m| m.strip_prefix("models/").unwrap_or(m)).unwrap_or_default().to_owned(),
        fell_back: false,
    }
    .reply(Provider::Gemini))
}
