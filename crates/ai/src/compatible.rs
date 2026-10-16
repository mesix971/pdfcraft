//! Any service that speaks OpenAI's Chat Completions (`POST {address}/chat/completions`):
//! OpenRouter, Mistral, DeepSeek, Groq, Ollama on the device or the local network…
//!
//! Most of them don't read PDF files: they get the document's text, page by page, with the first
//! question. One that does (OpenRouter) can get the PDF as a `file` part instead. Each question
//! carries the conversation: every answer as a plain assistant message (its text and tool calls,
//! nothing a service added of its own), a `tool` message for each change it proposed, then the
//! question. The service's own limits (the answer's length, the prompt's) apply.

use serde_json::{Value, json};

use super::{AiError, Call, Parsed, Provider, Reply, Request, Usage};

/// The address requests go to: the service's own (`base`, up to its version; the whole address
/// pasted is taken too) and the Chat Completions path. One line of plain characters, starting
/// with `https://`; or `http://` for a service on this device or the local network, which has no
/// certificate (plain HTTP carries the key and the document as they are: never further).
pub(super) fn endpoint(base: &str) -> Result<String, AiError> {
    const PATH: &str = "/chat/completions";
    let mut base = base.trim().trim_end_matches('/');
    if let Some(cut) = base.len().checked_sub(PATH.len())
        && base.get(cut..).is_some_and(|tail| tail.eq_ignore_ascii_case(PATH))
    {
        base = base.get(..cut).unwrap_or_default().trim_end_matches('/');
    }
    let scheme = ["https://", "http://"].into_iter().find(|s| base.get(..s.len()).is_some_and(|b| b.eq_ignore_ascii_case(s)));
    let rest = scheme.and_then(|s| base.get(s.len()..)).unwrap_or_default();
    let host = host_of(rest);
    if host.is_empty() || base.len() > super::MAX_URL || !base.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(AiError::BadAddress);
    }
    if scheme == Some("http://") && !local(host) {
        return Err(AiError::BadAddress);
    }
    Ok(format!("{base}{PATH}"))
}

/// The host in an address after its scheme: "[::1]:8080/v1" is "::1". Empty when the address
/// carries a user name or a backslash before its path (what URL readers disagree on: the host
/// one of them sees isn't the one another connects to).
pub(super) fn host_of(rest: &str) -> &str {
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.contains(['@', '\\']) {
        return "";
    }
    match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or_default(),
        None => authority.split(':').next().unwrap_or_default(),
    }
}

/// A host on this device or the local network: `localhost`, or a loopback, private or
/// link-local address. Not a name: a short one may be resolved somewhere else, and anyone on
/// the network may answer for a `.local` one.
fn local(host: &str) -> bool {
    use std::net::IpAddr;
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Ok(IpAddr::V6(ip)) => {
            let [first, ..] = ip.segments();
            // Loopback, unique local (fc00::/7), link-local (fe80::/10).
            ip.is_loopback() || first & 0xfe00 == 0xfc00 || first & 0xffc0 == 0xfe80
        }
        Err(_) => host == "localhost",
    }
}

/// Mistral takes no question right after the answers to tool calls: an empty answer goes between
/// (as other clients of it do).
fn needs_a_turn_after_tools(base: &str) -> bool {
    super::service_host(base).is_some_and(|h| h == "mistral.ai" || h.ends_with(".mistral.ai"))
}

pub(super) fn body(req: &Request) -> Value {
    let mut prompts = req.prompts();
    let first = prompts.next().unwrap_or_default();
    let document = match req.text() {
        // The text and the question as one message: some services take nothing else.
        Some(text) => json!(format!("The open document, \"{}\":\n\n{}\n\n---\n\n{first}", req.title.trim(), text.trim())),
        None => json!([
            {"type": "file", "file": {"filename": super::file_name(req.title), "file_data": format!("data:application/pdf;base64,{}", super::base64(req.pdf))}},
            {"type": "text", "text": first},
        ]),
    };
    let mut messages = vec![json!({"role": "system", "content": req.system()}), json!({"role": "user", "content": document})];
    for (turn, prompt) in req.history.iter().zip(prompts) {
        messages.push(turn.answer.clone());
        // Every tool call gets its answer (the API refuses a conversation that leaves one out).
        let answered = calls(&turn.answer);
        messages.extend(answered.iter().map(|c| json!({"role": "tool", "tool_call_id": c.id, "content": turn.result(&c.id)})));
        if !answered.is_empty() && needs_a_turn_after_tools(req.base_url) {
            messages.push(json!({"role": "assistant", "content": ""}));
        }
        messages.push(json!({"role": "user", "content": prompt}));
    }
    let mut body = json!({"model": req.model.trim(), "messages": messages});
    if req.tool()
        && let Some(map) = body.as_object_mut()
    {
        map.insert(
            "tools".into(),
            json!([{"type": "function", "function": {"name": super::EDIT_TOOL, "description": super::EDIT_DESCRIPTION, "parameters": super::edit_schema(false)}}]),
        );
    }
    body
}

/// The tool calls of an answer (a message's `tool_calls`; arguments as a JSON string, or as an
/// object where a service sends one).
pub(super) fn calls(answer: &Value) -> Vec<Call> {
    let Some(tool_calls) = answer["tool_calls"].as_array() else { return Vec::new() };
    tool_calls
        .iter()
        .filter_map(|c| {
            Some(Call {
                id: c["id"].as_str()?.to_owned(),
                name: c["function"]["name"].as_str().unwrap_or_default().to_owned(),
                args: super::arguments(&c["function"]["arguments"]),
            })
        })
        .collect()
}

pub(super) fn parse(status: u16, body: &str) -> Result<Reply, AiError> {
    if !(200..300).contains(&status) {
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        return Err(match super::error_for(status, super::error_message(&v)) {
            // A gateway in front of a busy service.
            AiError::Api(_) if status == 502 => AiError::Overloaded,
            e => e,
        });
    }
    let v: Value = serde_json::from_str(body).map_err(|e| AiError::Parse(e.to_string()))?;
    let choice = &v["choices"][0];
    let message = choice.get("message").filter(|m| m.is_object()).ok_or_else(|| AiError::Parse("no message".into()))?;
    let finish = choice["finish_reason"].as_str().unwrap_or_default();
    if finish == "content_filter" {
        return Err(AiError::Refused("content_filter".into()));
    }
    let text = match &message["content"] {
        Value::String(s) => s.clone(),
        // Some services send parts.
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect::<Vec<_>>().join("\n\n"),
        _ => String::new(),
    };
    let tool_calls: Vec<Value> = message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let arguments = match &c["function"]["arguments"] {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            Some(json!({"id": c["id"].as_str()?, "type": "function", "function": {"name": c["function"]["name"], "arguments": arguments}}))
        })
        .collect();
    // The answer as it goes back: what any service takes, and nothing one added of its own.
    let mut content = json!({"role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) }});
    if !tool_calls.is_empty()
        && let Some(map) = content.as_object_mut()
    {
        map.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    let n = |path: &[&str]| path.iter().fold(&v["usage"], |u, k| &u[*k]).as_u64().unwrap_or(0);
    let read = n(&["prompt_tokens_details", "cached_tokens"]);
    Ok(Parsed {
        text,
        calls: calls(&content),
        content,
        usage: Usage { input: n(&["prompt_tokens"]).saturating_sub(read), output: n(&["completion_tokens"]), cache_write: 0, cache_read: read },
        truncated: finish == "length",
        model: v["model"].as_str().unwrap_or_default().to_owned(),
        fell_back: false,
    }
    .reply(Provider::Compatible))
}
