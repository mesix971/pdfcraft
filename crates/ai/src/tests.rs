use super::*;

fn request<'a>(pdf: &'a [u8], history: &'a [Turn], task: Task) -> Request<'a> {
    Request {
        provider: Provider::Anthropic,
        key: "sk-ant-test",
        model: "claude-opus-5-5",
        base_url: "",
        title: "report.pdf",
        pdf,
        text: None,
        pages: 3,
        max_pages: 100,
        language: "French",
        history,
        task,
        edits: false,
    }
}

/// `req` asked of `provider`'s `model`.
fn of<'a>(provider: Provider, model: &'a str, req: Request<'a>) -> Request<'a> {
    Request { provider, model, ..req }
}

fn claude(status: u16, body: &str) -> Result<Reply, AiError> {
    parse_response(Provider::Anthropic, "claude-opus-5-5", status, body)
}

// ---------------------------------------------------------------------------------------------
// Anthropic

#[test]
fn the_first_question_sends_the_document_cached() {
    let body = request_body(&request(b"%PDF-1.7 tiny", &[], Task::Summarize)).unwrap();
    assert_eq!(body["model"], "claude-opus-5-5");
    assert_eq!(body["fallbacks"], "default", "a declined request falls back server-side");
    assert_eq!(body["output_config"]["effort"], "medium");
    assert!(body["system"].as_str().unwrap().ends_with("Reply in French."));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 1);
    let content = messages[0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "document");
    assert_eq!(content[0]["source"]["media_type"], "application/pdf");
    assert_eq!(content[0]["source"]["data"], "JVBERi0xLjcgdGlueQ==");
    assert_eq!(content[0]["title"], "report.pdf");
    assert_eq!(content[0]["cache_control"]["type"], "ephemeral");
    assert!(content[1]["text"].as_str().unwrap().starts_with("Summarize this document"));
    // Short, for a phone (a long summary also takes long to write).
    assert!(content[1]["text"].as_str().unwrap().contains("under 120 words"));
    // Thinking is left to the model (it can't be turned off on Opus 5.5), and nothing is prefilled.
    assert!(body.get("thinking").is_none());
}

#[test]
fn an_effort_level_goes_only_to_a_model_that_takes_one() {
    let effort =
        |model| request_body(&of(Provider::Anthropic, model, request(b"%PDF", &[], Task::Summarize))).unwrap()["output_config"]["effort"].clone();
    assert_eq!(effort("claude-opus-4-6"), "medium");
    assert_eq!(effort("claude-haiku-4-5"), Value::Null, "Haiku 4.5 refuses one");
    assert_eq!(effort("claude-somebody-new"), Value::Null, "a model the user names may too");
}

#[test]
fn follow_up_questions_keep_the_conversation_and_its_cached_start() {
    let first = request_body(&request(b"%PDF", &[], Task::Ask("Who signs it?".into()))).unwrap();
    let answer = json!([{"type": "thinking", "thinking": "", "signature": "sig"}, {"type": "text", "text": "Ms Durand (p. 2)."}]);
    let history = [Turn { prompt: "Who signs it?".into(), answer: answer.clone(), results: vec![] }];
    let next = request_body(&request(b"%PDF", &history, Task::Ask("When?".into()))).unwrap();
    let messages = next["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["content"][0], first["messages"][0]["content"][0], "the cached document is byte for byte the same");
    assert_eq!(messages[0]["content"][1]["text"], first["messages"][0]["content"][1]["text"]);
    assert_eq!(next["system"], first["system"]);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], answer, "the answer goes back exactly as it came, thinking included");
    assert_eq!(messages[2]["content"][0]["text"], "When?");
    // The conversation is cached at its newest question, and only there.
    assert_eq!(first["messages"][0]["content"][1]["cache_control"]["type"], "ephemeral");
    assert_eq!(messages[2]["content"][0]["cache_control"]["type"], "ephemeral");
    assert!(messages[0]["content"][1].get("cache_control").is_none());
}

#[test]
fn changes_are_offered_only_for_a_document_that_can_change() {
    let plain = request_body(&request(b"%PDF", &[], Task::Summarize)).unwrap();
    assert!(plain.get("tools").is_none());
    assert!(plain["system"].as_str().unwrap().contains("can't be changed here"));
    let mut req = request(b"%PDF", &[], Task::Ask("Fix the typos".into()));
    req.edits = true;
    let body = request_body(&req).unwrap();
    assert_eq!(body["tools"][0]["name"], "edit_text");
    assert_eq!(body["tools"][0]["input_schema"]["required"], json!(["page", "find", "replace"]));
    assert_eq!(body["tools"][0]["input_schema"]["properties"]["page"]["minimum"], 1);
    assert!(body["system"].as_str().unwrap().contains("edit_text tool"));
    assert!(body["system"].as_str().unwrap().ends_with("Reply in French."));
}

#[test]
fn proposed_changes_are_read_and_their_fate_goes_back_with_the_next_question() {
    let answer = r#"{"model":"claude-opus-5-5","stop_reason":"tool_use","content":[
        {"type":"text","text":"Two typos."},
        {"type":"tool_use","id":"toolu_1","name":"edit_text","input":{"page":2,"find":"teh cat","replace":"the cat"}},
        {"type":"tool_use","id":"toolu_2","name":"edit_text","input":{"page":0,"find":"bad page","replace":"x"}},
        {"type":"tool_use","id":"toolu_3","name":"edit_text","input":{"page":1,"find":"recieve","replace":"receive"}}],
        "usage":{"input_tokens":1,"output_tokens":1}}"#;
    let r = claude(200, answer).unwrap();
    assert_eq!(
        r.edits,
        vec![
            ProposedEdit { id: "toolu_1".into(), page: 2, find: "teh cat".into(), replace: "the cat".into() },
            ProposedEdit { id: "toolu_3".into(), page: 1, find: "recieve".into(), replace: "receive".into() },
        ],
        "page 0 doesn't exist: left out"
    );
    // Only proposals, no words: still an answer.
    let only = answer.replace(r#"{"type":"text","text":"Two typos."},"#, "");
    assert_eq!(claude(200, &only).unwrap().edits.len(), 2);
    // Cut off at the length limit: the last proposal may stop mid-way, so it isn't offered.
    let cut = answer.replace("\"tool_use\",\"content\"", "\"max_tokens\",\"content\"");
    let r_cut = claude(200, &cut).unwrap();
    assert!(r_cut.truncated);
    assert_eq!(r_cut.edits.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["toolu_1"]);
    // The next question reports each proposal, in order; the malformed one as not applied.
    let history = [Turn { prompt: "Fix the typos".into(), answer: r.content.clone(), results: vec![("toolu_3".into(), "Applied.".into())] }];
    let mut req = request(b"%PDF", &history, Task::Ask("Thanks".into()));
    req.edits = true;
    let body = request_body(&req).unwrap();
    let next = body["messages"][2]["content"].as_array().unwrap();
    assert_eq!(next.len(), 4);
    assert_eq!(next[0], json!({"type": "tool_result", "tool_use_id": "toolu_1", "content": NOT_APPLIED}));
    assert_eq!(next[1]["tool_use_id"], "toolu_2");
    assert_eq!(next[2], json!({"type": "tool_result", "tool_use_id": "toolu_3", "content": "Applied."}));
    assert_eq!(next[3]["text"], "Thanks");
    assert_eq!(next[3]["cache_control"]["type"], "ephemeral");
    // The tool stays defined for a conversation that used it.
    req.edits = false;
    assert_eq!(request_body(&req).unwrap()["tools"][0]["name"], "edit_text");
}

#[test]
fn an_empty_question_is_never_sent() {
    assert_eq!(request_body(&request(b"%PDF", &[], Task::Ask(" \n ".into()))), Err(AiError::NoQuestion));
    let history = [Turn { prompt: String::new(), answer: json!([]), results: vec![] }];
    assert_eq!(request_body(&request(b"%PDF", &history, Task::Summarize)), Err(AiError::NoQuestion));
}

#[test]
fn haiku_has_no_fallback_and_translation_names_the_page() {
    let mut req = request(b"%PDF", &[], Task::Translate { page: 2, language: "German".into() });
    req.model = "claude-haiku-5-5";
    let body = request_body(&req).unwrap();
    assert!(body.get("fallbacks").is_none());
    assert!(body["messages"][0]["content"][1]["text"].as_str().unwrap().starts_with("Translate page 2 of this document into German."));
}

#[test]
fn requests_are_refused_before_anything_is_sent() {
    let mut req = request(b"%PDF", &[], Task::Summarize);
    req.key = "  ";
    assert_eq!(request_body(&req), Err(AiError::NoKey));
    req.key = "sk-ant-\nInjected: header";
    assert_eq!(request_body(&req), Err(AiError::BadKey));
    req.key = "sk-ant-ok";
    req.model = " ";
    assert_eq!(request_body(&req), Err(AiError::NoModel));
    req.model = "claude\nInjected";
    assert_eq!(request_body(&req), Err(AiError::NoModel));
    req.model = "claude-opus-5-5";
    req.pages = 101;
    assert_eq!(request_body(&req), Err(AiError::TooManyPages { pages: 101, max: 100 }));
    req.max_pages = 10_000;
    req.pages = 601;
    assert_eq!(request_body(&req), Err(AiError::TooManyPages { pages: 601, max: API_MAX_PAGES }), "never past the API's own limit");
    req.pages = 1;
    req.pdf = &[];
    assert_eq!(request_body(&req), Err(AiError::Empty));
    let big = vec![0u8; MAX_PDF_BYTES + 1];
    req.pdf = &big;
    assert!(matches!(request_body(&req), Err(AiError::TooLarge { .. })));
    // Only a compatible service gets the text: the others always get the PDF.
    req.text = Some("[Page 1]\nHello");
    assert!(matches!(request_body(&req), Err(AiError::TooLarge { .. })));
}

#[test]
fn answers_are_read_after_their_stop_reason() {
    let ok = r#"{"model":"claude-opus-5-5","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"","signature":"s"},{"type":"text","text":"Two points."}],"usage":{"input_tokens":12,"output_tokens":40,"cache_creation_input_tokens":3000,"cache_read_input_tokens":0}}"#;
    let r = claude(200, ok).unwrap();
    assert_eq!(r.text, "Two points.");
    assert_eq!(r.content.as_array().map(Vec::len), Some(2), "kept whole for the next turn");
    assert_eq!(r.usage, Usage { input: 12, output: 40, cache_write: 3000, cache_read: 0 });
    assert!(!r.truncated);
    assert!(!r.fell_back);
    let cut = ok.replace("end_turn", "max_tokens");
    assert!(claude(200, &cut).unwrap().truncated);
    let full = ok.replace("end_turn", "model_context_window_exceeded");
    assert!(claude(200, &full).unwrap().truncated);
    let declined = r#"{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"},"content":[]}"#;
    assert_eq!(claude(200, declined), Err(AiError::Refused("cyber".into())));
    let long = declined.replace("cyber", &"x".repeat(5000));
    assert!(matches!(claude(200, &long), Err(AiError::Refused(c)) if c.len() == MAX_CATEGORY));
}

#[test]
fn an_answer_without_text_is_no_answer() {
    let thought_only = r#"{"model":"claude-opus-5-5","stop_reason":"max_tokens","content":[{"type":"thinking","thinking":"","signature":"s"}],"usage":{"input_tokens":1,"output_tokens":16000}}"#;
    assert_eq!(claude(200, thought_only), Err(AiError::NoAnswer));
    let blank = r#"{"model":"claude-opus-5-5","stop_reason":"end_turn","content":[{"type":"text","text":"  \n "}],"usage":{}}"#;
    assert_eq!(claude(200, blank), Err(AiError::NoAnswer));
}

#[test]
fn a_fallback_answer_is_priced_as_the_model_that_gave_it() {
    let body = r#"{"model":"claude-opus-4-8","stop_reason":"end_turn","content":[{"type":"fallback","from":{"model":"claude-opus-5-5"},"to":{"model":"claude-opus-4-8"}},{"type":"text","text":"Here."}],"usage":{"input_tokens":1000000,"output_tokens":1000000,"iterations":[{"type":"message"},{"type":"fallback_message"}]}}"#;
    let r = claude(200, body).unwrap();
    assert!(r.fell_back);
    assert_eq!(r.text, "Here.");
    assert!((r.cost_usd().unwrap() - 30.0).abs() < 1e-9, "Opus 4.8's $5 / $25, not Opus 5.5's $4 / $20");
    let unknown = body.replace("claude-opus-4-8\",\"stop", "claude-somethingelse\",\"stop");
    assert_eq!(claude(200, &unknown).unwrap().cost_usd(), None, "no made-up price");
    let dated = body.replace("claude-opus-4-8\",\"stop", "claude-opus-5-5-20261001\",\"stop");
    assert!((claude(200, &dated).unwrap().cost_usd().unwrap() - 24.0).abs() < 1e-9);
}

#[test]
fn api_errors_say_what_to_do() {
    let error = |kind: &str, message: &str| format!(r#"{{"type":"error","error":{{"type":"{kind}","message":"{message}"}}}}"#);
    assert_eq!(claude(401, &error("authentication_error", "invalid x-api-key")), Err(AiError::InvalidKey));
    assert_eq!(claude(429, &error("rate_limit_error", "slow down")), Err(AiError::RateLimited));
    assert_eq!(claude(529, &error("overloaded_error", "busy")), Err(AiError::Overloaded));
    assert_eq!(claude(400, &error("invalid_request_error", "prompt is too long")), Err(AiError::Api("prompt is too long".into())));
    assert!(matches!(claude(403, &error("permission_error", "no access")), Err(AiError::NotAllowed(_))));
    assert_eq!(claude(400, &error("billing_error", "credit balance is too low")), Err(AiError::NotAllowed("credit balance is too low".into())));
    assert_eq!(claude(502, "<html>bad gateway</html>"), Err(AiError::Api("HTTP 502".into())), "a proxy's page is not a parse error");
    assert_eq!(claude(407, ""), Err(AiError::Api("HTTP 407".into())));
    assert!(matches!(claude(200, "<html>portal</html>"), Err(AiError::Parse(_))));
}

#[test]
fn costs_follow_the_list_prices() {
    let k = |provider, id| known(provider, id).unwrap();
    let usage = Usage { input: 1_000_000, output: 1_000_000, cache_write: 0, cache_read: 0 };
    assert!((usage.cost_usd(k(Provider::Anthropic, "claude-opus-5-5")) - 24.0).abs() < 1e-9);
    assert!((usage.cost_usd(k(Provider::Anthropic, "claude-sonnet-5-5")) - 12.0).abs() < 1e-9);
    let cached = Usage { input: 0, output: 0, cache_write: 0, cache_read: 1_000_000 };
    assert!((cached.cost_usd(k(Provider::Anthropic, "claude-opus-5-5")) - 0.20).abs() < 1e-9);
    // Opus 5.5 and Sonnet 5.5 read their cache at 0.05 times their input price.
    assert!((cached.cost_usd(k(Provider::Anthropic, "claude-sonnet-5-5")) - 0.10).abs() < 1e-9);
    // Haiku's dearer rate card starts past 100K prompt tokens.
    let haiku = k(Provider::Anthropic, "claude-haiku-5-5");
    let short = Usage { input: 100_000, output: 0, cache_write: 0, cache_read: 0 };
    assert!((short.cost_usd(haiku) - 0.01).abs() < 1e-9);
    let long = Usage { input: 50_000, output: 1_000_000, cache_write: 0, cache_read: 60_000 };
    assert!((long.cost_usd(haiku) - (0.025 + 2.5 + 0.003)).abs() < 1e-9);
    // OpenAI's: twice the input and cache prices, 1.5 times the output's, past 272K prompt tokens.
    let sol = k(Provider::OpenAi, "gpt-6.1-sol");
    assert!((usage.cost_usd(sol) - (4.0 + 15.0)).abs() < 1e-9);
    let within = Usage { input: 200_000, output: 100_000, cache_write: 0, cache_read: 72_000 };
    assert!((within.cost_usd(sol) - (0.4 + 1.0 + 0.0072)).abs() < 1e-9);
    // Gemini's, at the regular prices.
    assert!((usage.cost_usd(k(Provider::Gemini, "gemini-3.8-flash")) - 9.0).abs() < 1e-9);
    assert!((usage.cost_usd(k(Provider::Gemini, "gemini-4-argon")) - 24.0).abs() < 1e-9);
}

#[test]
fn models_are_known_by_their_provider_and_their_dated_ids() {
    for p in Provider::ALL {
        assert_eq!(Provider::from_id(p.id()), Some(p));
        assert_eq!(Provider::ALL.get(p.index()), Some(&p));
        if p == Provider::Compatible {
            assert_eq!(p.models().count(), 0, "a compatible service's model is named by the user");
            continue;
        }
        assert_eq!(p.models().next().map(|m| m.provider), Some(p));
        assert!(p.models().any(|m| m.id == p.default_model()), "{p:?}'s default is offered");
        for m in p.models() {
            assert_eq!(known(p, m.id).map(|k| k.id), Some(m.id));
        }
    }
    assert_eq!(Provider::from_id("mystery"), None);
    assert_eq!(known(Provider::Anthropic, "claude-opus-5-5-20261001").map(|k| k.id), Some("claude-opus-5-5"), "not Opus 5");
    assert_eq!(known(Provider::OpenAi, "gpt-6.1-sol-2026-05-01").map(|k| k.id), Some("gpt-6.1-sol"));
    assert_eq!(known(Provider::OpenAi, "gpt-6.1-sol-mini"), None);
    assert_eq!(known(Provider::OpenAi, "claude-opus-5-5"), None, "another provider's");
    assert_eq!(known(Provider::Compatible, "gpt-6.1-sol"), None, "a compatible service's are never priced");
    assert!(Provider::Anthropic.models().any(|m| m.id == "claude-opus-4-6"), "older Claude models are offered");
    assert!(known(Provider::Anthropic, "claude-opus-4-6").is_some_and(|k| k.effort()));
    assert!(known(Provider::Anthropic, "claude-haiku-4-5-20251001").is_some_and(|k| !k.effort()), "Haiku 4.5 refuses one");
    assert!(known(Provider::Gemini, "gemini-3.8-flash").is_some_and(|k| k.free_tier));
    assert!(known(Provider::Gemini, "gemini-4-argon").is_some_and(|k| !k.free_tier));
}

// ---------------------------------------------------------------------------------------------
// OpenAI

fn openai_reply(status: u16, body: &str) -> Result<Reply, AiError> {
    parse_response(Provider::OpenAi, "gpt-6.1-sol", status, body)
}

#[test]
fn openai_gets_the_pdf_as_a_file_and_nothing_is_stored() {
    let mut req = of(Provider::OpenAi, "gpt-6.1-sol", request(b"%PDF-1.7 tiny", &[], Task::Summarize));
    req.key = "sk-proj-test";
    let body = request_body(&req).unwrap();
    assert_eq!(body["model"], "gpt-6.1-sol");
    assert_eq!(body["store"], false);
    assert_eq!(body["reasoning"]["effort"], "medium");
    assert_eq!(body["max_output_tokens"], 25_000);
    assert!(body["instructions"].as_str().unwrap().ends_with("Reply in French."));
    assert!(body["instructions"].as_str().unwrap().contains("attached PDF"));
    let content = &body["input"][0]["content"];
    assert_eq!(body["input"][0]["role"], "user");
    assert_eq!(content[0]["type"], "input_file");
    assert_eq!(content[0]["filename"], "report.pdf");
    assert_eq!(content[0]["file_data"], "data:application/pdf;base64,JVBERi0xLjcgdGlueQ==");
    assert_eq!(content[1]["type"], "input_text");
    assert!(content[1]["text"].as_str().unwrap().starts_with("Summarize this document"));
    assert!(body.get("tools").is_none());
    req.edits = true;
    let tools = request_body(&req).unwrap()["tools"].clone();
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "edit_text");
    assert_eq!(tools[0]["strict"], false);
    assert_eq!(tools[0]["parameters"]["required"], json!(["page", "find", "replace"]));
    // A title without the extension still names a PDF.
    req.title = "Lease";
    assert_eq!(request_body(&req).unwrap()["input"][0]["content"][0]["filename"], "Lease.pdf");
}

#[test]
fn openai_answers_are_read_and_go_back_whole_with_what_became_of_their_changes() {
    let answer = r#"{"model":"gpt-6.1-sol-2026-05-01","status":"completed","output":[
        {"type":"reasoning","id":"rs_1","encrypted_content":"opaque","summary":[]},
        {"type":"message","id":"msg_1","role":"assistant","content":[{"type":"output_text","text":"One typo.","annotations":[]}]},
        {"type":"function_call","id":"fc_1","call_id":"call_1","name":"edit_text","arguments":"{\"page\":1,\"find\":\"teh\",\"replace\":\"the\"}"},
        {"type":"function_call","id":"fc_2","call_id":"call_2","name":"edit_text","arguments":"not json"}],
        "usage":{"input_tokens":1000,"input_tokens_details":{"cached_tokens":600,"cache_write_tokens":100},"output_tokens":50,"output_tokens_details":{"reasoning_tokens":20}}}"#;
    let r = openai_reply(200, answer).unwrap();
    assert_eq!(r.text, "One typo.");
    assert_eq!(
        r.edits,
        vec![ProposedEdit { id: "call_1".into(), page: 1, find: "teh".into(), replace: "the".into() }],
        "malformed arguments: left out"
    );
    assert_eq!(r.usage, Usage { input: 300, output: 50, cache_write: 100, cache_read: 600 }, "input_tokens counts the cached ones");
    assert_eq!(r.model, "gpt-6.1-sol-2026-05-01");
    let cost = 300.0 * 2.0 / 1e6 + 50.0 * 10.0 / 1e6 + 100.0 * 2.5 / 1e6 + 600.0 * 0.10 / 1e6;
    assert!((r.cost_usd().unwrap() - cost).abs() < 1e-12, "priced as the dated id's model");
    assert!(!r.truncated);
    // The next question: the output items exactly as they came, an output for every call, then the question.
    let history = [Turn { prompt: "Fix the typos".into(), answer: r.content.clone(), results: vec![("call_1".into(), "Applied.".into())] }];
    let req = of(Provider::OpenAi, "gpt-6.1-sol", request(b"%PDF", &history, Task::Ask("Thanks".into())));
    let body = request_body(&req).unwrap();
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.len(), 8);
    assert_eq!(input[1]["encrypted_content"], "opaque", "the reasoning goes back");
    assert_eq!(&input[1..5], r.content.as_array().unwrap().as_slice());
    assert_eq!(input[5], json!({"type": "function_call_output", "call_id": "call_1", "output": "Applied."}));
    assert_eq!(input[6], json!({"type": "function_call_output", "call_id": "call_2", "output": NOT_APPLIED}));
    assert_eq!(input[7], json!({"role": "user", "content": [{"type": "input_text", "text": "Thanks"}]}));
    assert_eq!(body["tools"][0]["name"], "edit_text", "the tool stays defined for a conversation that used it");
    // Cut off at the length limit: the last call isn't offered.
    let cut = answer.replace("\"completed\"", "\"incomplete\",\"incomplete_details\":{\"reason\":\"max_output_tokens\"}");
    let r_cut = openai_reply(200, &cut).unwrap();
    assert!(r_cut.truncated);
    assert_eq!(r_cut.edits.len(), 1, "call_2 was the last one, and malformed anyway");
}

#[test]
fn openai_refusals_failures_and_errors() {
    let refused = r#"{"status":"completed","output":[{"type":"message","content":[{"type":"refusal","refusal":"I can't help with that."}]}]}"#;
    assert_eq!(openai_reply(200, refused), Err(AiError::Refused("I can't help with that.".into())));
    let filtered = r#"{"status":"incomplete","incomplete_details":{"reason":"content_filter"},"output":[]}"#;
    assert_eq!(openai_reply(200, filtered), Err(AiError::Refused("content_filter".into())));
    let failed = r#"{"status":"failed","error":{"code":"server_error","message":"Something broke."},"output":[]}"#;
    assert_eq!(openai_reply(200, failed), Err(AiError::Api("Something broke.".into())));
    let empty = r#"{"status":"completed","output":[{"type":"reasoning","encrypted_content":"x"}]}"#;
    assert_eq!(openai_reply(200, empty), Err(AiError::NoAnswer));
    let error = |code: &str, kind: &str| format!(r#"{{"error":{{"message":"msg","type":"{kind}","code":"{code}"}}}}"#);
    assert_eq!(openai_reply(401, &error("invalid_api_key", "invalid_request_error")), Err(AiError::InvalidKey));
    assert_eq!(openai_reply(429, &error("credit_balance_exhausted", "")), Err(AiError::NotAllowed("msg".into())));
    assert_eq!(openai_reply(429, &error("", "insufficient_quota")), Err(AiError::NotAllowed("msg".into())));
    assert_eq!(openai_reply(429, &error("rate_limit_exceeded", "requests")), Err(AiError::RateLimited));
    assert_eq!(openai_reply(403, &error("unsupported_country_region_territory", "")), Err(AiError::NotAllowed("msg".into())));
    assert_eq!(openai_reply(503, &error("server_is_overloaded", "")), Err(AiError::Overloaded));
    assert_eq!(openai_reply(400, &error("model_not_found", "")), Err(AiError::Api("msg".into())));
}

// ---------------------------------------------------------------------------------------------
// Gemini

fn gemini_reply(status: u16, body: &str) -> Result<Reply, AiError> {
    parse_response(Provider::Gemini, "gemini-3.8-flash", status, body)
}

#[test]
fn gemini_gets_the_pdf_as_a_document_and_nothing_is_stored() {
    let mut req = of(Provider::Gemini, "gemini-3.8-flash", request(b"%PDF-1.7 tiny", &[], Task::Summarize));
    req.key = "AIza-test";
    let body = request_body(&req).unwrap();
    assert_eq!(body["model"], "gemini-3.8-flash");
    assert_eq!(body["store"], false);
    assert_eq!(body["generation_config"]["max_output_tokens"], 25_000);
    assert!(body["system_instruction"].as_str().unwrap().ends_with("Reply in French."));
    let first = &body["input"][0];
    assert_eq!(first["type"], "user_input");
    assert_eq!(first["content"][0], json!({"type": "document", "data": "JVBERi0xLjcgdGlueQ==", "mime_type": "application/pdf"}));
    assert_eq!(first["content"][1]["type"], "text");
    assert!(body.get("tools").is_none());
    req.edits = true;
    let tools = request_body(&req).unwrap()["tools"].clone();
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["name"], "edit_text");
    assert!(tools[0]["parameters"]["properties"]["page"].get("minimum").is_none(), "Gemini reads only part of JSON Schema");
}

#[test]
fn gemini_answers_are_read_and_their_steps_go_back_whole() {
    let answer = r#"{"id":"int_1","status":"requires_action","steps":[
        {"type":"user_input","content":[{"type":"text","text":"echo"}]},
        {"type":"thought","signature":"sig-1"},
        {"type":"model_output","content":[{"type":"text","text":"One typo."}]},
        {"type":"function_call","id":"fc_1","name":"edit_text","arguments":{"page":3,"find":"teh","replace":"the"}}],
        "usage":{"total_input_tokens":1000,"total_cached_tokens":400,"total_output_tokens":30,"total_thought_tokens":70,"total_tokens":1100}}"#;
    let r = gemini_reply(200, answer).unwrap();
    assert_eq!(r.text, "One typo.");
    let named = answer.replace("\"id\":\"int_1\"", "\"id\":\"int_1\",\"model\":\"models/gemini-3.8-flash\"");
    assert_eq!(gemini_reply(200, &named).unwrap().model, "gemini-3.8-flash", "named the API's way");
    assert_eq!(r.edits, vec![ProposedEdit { id: "fc_1".into(), page: 3, find: "teh".into(), replace: "the".into() }]);
    assert_eq!(r.usage, Usage { input: 600, output: 100, cache_write: 0, cache_read: 400 }, "thoughts are output");
    assert_eq!(r.model, "gemini-3.8-flash", "not named in the answer: the model asked");
    assert!(r.cost_usd().is_some());
    assert_eq!(r.content.as_array().map(Vec::len), Some(3), "the model's steps only");
    let history = [Turn { prompt: "Fix it".into(), answer: r.content.clone(), results: vec![] }];
    let req = of(Provider::Gemini, "gemini-3.8-flash", request(b"%PDF", &history, Task::Ask("Thanks".into())));
    let input = request_body(&req).unwrap()["input"].as_array().unwrap().clone();
    assert_eq!(input.len(), 6);
    assert_eq!(input[1], json!({"type": "thought", "signature": "sig-1"}), "the thought and its signature go back");
    assert_eq!(
        input[4],
        json!({"type": "function_result", "name": "edit_text", "call_id": "fc_1", "result": [{"type": "text", "text": NOT_APPLIED}]})
    );
    assert_eq!(input[5], json!({"type": "user_input", "content": [{"type": "text", "text": "Thanks"}]}));
    let cut = answer.replace("requires_action", "incomplete");
    assert!(gemini_reply(200, &cut).unwrap().truncated);
    let failed = r#"{"status":"failed","steps":[]}"#;
    assert_eq!(gemini_reply(200, failed), Err(AiError::Api("failed".into())));
    let error = |code: &str| format!(r#"{{"error":{{"code":"{code}","message":"msg"}}}}"#);
    assert_eq!(gemini_reply(401, &error("authentication")), Err(AiError::InvalidKey));
    assert_eq!(gemini_reply(402, &error("payment_required")), Err(AiError::NotAllowed("msg".into())));
    assert_eq!(gemini_reply(429, &error("quota_exceeded")), Err(AiError::RateLimited));
    assert_eq!(gemini_reply(503, &error("service_unavailable")), Err(AiError::Overloaded));
}

// ---------------------------------------------------------------------------------------------
// Compatible services

fn compatible_reply(status: u16, body: &str) -> Result<Reply, AiError> {
    parse_response(Provider::Compatible, "llama3.2", status, body)
}

#[test]
fn a_compatible_services_address_is_checked() {
    assert_eq!(compatible::endpoint(" https://openrouter.ai/api/v1/ "), Ok("https://openrouter.ai/api/v1/chat/completions".into()));
    assert_eq!(
        compatible::endpoint("https://api.mistral.ai/v1/chat/completions"),
        Ok("https://api.mistral.ai/v1/chat/completions".into()),
        "the whole address pasted"
    );
    // Plain HTTP only on this device or the local network: the key and the document go as they are.
    for local in [
        "http://localhost:11434/v1",
        "http://127.0.0.1:1234/v1",
        "http://192.168.1.20:11434/v1",
        "http://10.0.0.5/v1",
        "http://[::1]:8080/v1",
        "HTTP://172.16.0.2/v1",
    ] {
        assert!(compatible::endpoint(local).is_ok(), "{local}");
    }
    for bad in [
        "",
        "openrouter.ai/api/v1",
        "ftp://host/v1",
        "https://",
        "https://a b",
        "https://host/\nv1",
        "http://openrouter.ai/api/v1",
        "http://8.8.8.8/v1",
        "http://[2001:db8::1]/v1",
        "http://evil.example@localhost.evil.com/v1",
        // Names: a short one may be resolved elsewhere, a .local one answered by anyone.
        "http://mypc:11434/v1",
        "http://mac.local/v1",
        // What URL readers disagree on: the host one sees isn't the one another connects to.
        "http://evil.com\\@localhost/v1",
        "https://user@openrouter.ai/v1",
    ] {
        assert_eq!(compatible::endpoint(bad), Err(AiError::BadAddress), "{bad:?}");
    }
    assert_eq!(compatible::endpoint(&format!("https://{}", "a".repeat(MAX_URL))), Err(AiError::BadAddress));
    assert_eq!(service_host("https://API.Mistral.ai/v1").as_deref(), Some("api.mistral.ai"));
    assert_eq!(service_host("http://[::1]:8080/v1").as_deref(), Some("::1"));
    assert_eq!(service_host("http://openrouter.ai/v1"), None, "not an address requests go to");
}

#[test]
fn the_key_never_shows_in_an_error() {
    let key = "sk-or-secret-123456";
    assert_eq!(hide_key(AiError::Api(format!("bad key {key} given")), key), AiError::Api("bad key … given".into()));
    assert_eq!(hide_key(AiError::NotAllowed(key.into()), key), AiError::NotAllowed("…".into()));
    assert_eq!(hide_key(AiError::Api("x".into()), ""), AiError::Api("x".into()), "no key: nothing to hide");
    assert_eq!(hide_key(AiError::InvalidKey, key), AiError::InvalidKey);
}

#[test]
fn mistral_gets_an_answer_between_the_tool_answers_and_the_question() {
    let answer = json!({"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "edit_text", "arguments": "{}"}}]});
    let history = [Turn { prompt: "Fix it".into(), answer, results: vec![] }];
    let mut req = of(Provider::Compatible, "mistral-small-latest", request(b"%PDF", &history, Task::Ask("Thanks".into())));
    req.base_url = "https://api.mistral.ai/v1";
    let roles = |req: &Request| {
        request_body(req).unwrap()["messages"].as_array().unwrap().iter().map(|m| m["role"].as_str().unwrap().to_owned()).collect::<Vec<_>>()
    };
    assert_eq!(roles(&req), ["system", "user", "assistant", "tool", "assistant", "user"]);
    req.base_url = "https://openrouter.ai/api/v1";
    assert_eq!(roles(&req), ["system", "user", "assistant", "tool", "user"], "the others as OpenAI's");
}

#[test]
fn a_compatible_service_gets_the_text_or_the_pdf_and_may_need_no_key() {
    let text = "[Page 1]\nRent: 900 EUR.\n\n[Page 2]\nSigned: Ms Durand.";
    let mut req = of(Provider::Compatible, "llama3.2", request(b"%PDF", &[], Task::Ask("Who signs?".into())));
    req.key = "";
    req.base_url = "http://localhost:11434/v1";
    req.text = Some(text);
    let body = request_body(&req).unwrap();
    assert_eq!(body["model"], "llama3.2");
    assert!(body.get("max_tokens").is_none(), "the service's own limit");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(messages[0]["role"], "system");
    assert!(messages[0]["content"].as_str().unwrap().contains("[Page N]"), "told how the text comes");
    let first = messages[1]["content"].as_str().unwrap();
    assert!(first.contains("report.pdf") && first.contains(text) && first.ends_with("Who signs?"), "{first}");
    // Without text: the PDF itself, as a file.
    req.text = None;
    let body = request_body(&req).unwrap();
    assert_eq!(body["messages"][1]["content"][0]["file"]["file_data"], "data:application/pdf;base64,JVBERg==");
    assert_eq!(body["messages"][1]["content"][1]["text"], "Who signs?");
    // No text to send, or too much.
    req.text = Some(" \u{c}\n ");
    assert_eq!(request_body(&req), Err(AiError::NoText));
    let long = "x".repeat(MAX_TEXT_CHARS + 1);
    req.text = Some(&long);
    assert_eq!(request_body(&req), Err(AiError::TooMuchText { chars: MAX_TEXT_CHARS + 1, max: MAX_TEXT_CHARS }));
    req.text = Some(text);
    req.base_url = "localhost:11434";
    assert_eq!(request_body(&req), Err(AiError::BadAddress));
    req.base_url = "https://openrouter.ai/api/v1";
    req.key = "sk-or-\nInjected";
    assert_eq!(request_body(&req), Err(AiError::BadKey));
}

#[test]
fn a_compatible_answer_goes_back_as_a_plain_message_with_its_tool_answers() {
    let answer = r#"{"model":"llama3.2","choices":[{"finish_reason":"tool_calls","message":{"role":"assistant","content":"One typo.","reasoning_content":"thoughts",
        "tool_calls":[{"id":"call_1","type":"function","function":{"name":"edit_text","arguments":"{\"page\":2,\"find\":\"teh\",\"replace\":\"the\"}"}},
                      {"id":"call_2","type":"function","function":{"name":"edit_text","arguments":{"page":1,"find":"recieve","replace":"receive"}}}]}}],
        "usage":{"prompt_tokens":900,"completion_tokens":40,"prompt_tokens_details":{"cached_tokens":500}}}"#;
    let r = compatible_reply(200, answer).unwrap();
    assert_eq!(r.text, "One typo.");
    assert_eq!(
        r.edits.iter().map(|e| (e.id.as_str(), e.page)).collect::<Vec<_>>(),
        [("call_1", 2), ("call_2", 1)],
        "arguments as a string or an object"
    );
    assert_eq!(r.usage, Usage { input: 400, output: 40, cache_write: 0, cache_read: 500 });
    assert_eq!(r.cost_usd(), None, "a compatible service's prices aren't known");
    assert!(r.content.get("reasoning_content").is_none(), "nothing a service added of its own goes back");
    assert_eq!(
        r.content["tool_calls"][1]["function"]["arguments"], r#"{"find":"recieve","page":1,"replace":"receive"}"#,
        "arguments go back as a string"
    );
    let history = [Turn { prompt: "Fix it".into(), answer: r.content.clone(), results: vec![("call_2".into(), "Applied.".into())] }];
    let mut req = of(Provider::Compatible, "llama3.2", request(b"%PDF", &history, Task::Ask("Thanks".into())));
    req.base_url = "https://openrouter.ai/api/v1";
    let messages = request_body(&req).unwrap()["messages"].as_array().unwrap().clone();
    assert_eq!(messages.len(), 6);
    assert_eq!(messages[2], r.content);
    assert_eq!(messages[3], json!({"role": "tool", "tool_call_id": "call_1", "content": NOT_APPLIED}));
    assert_eq!(messages[4], json!({"role": "tool", "tool_call_id": "call_2", "content": "Applied."}));
    assert_eq!(messages[5], json!({"role": "user", "content": "Thanks"}));
    let cut = answer.replace("\"tool_calls\",\"message\"", "\"length\",\"message\"");
    assert!(compatible_reply(200, &cut).unwrap().truncated);
    let filtered = answer.replace("\"tool_calls\",\"message\"", "\"content_filter\",\"message\"");
    assert_eq!(compatible_reply(200, &filtered), Err(AiError::Refused("content_filter".into())));
    assert_eq!(compatible_reply(401, r#"{"error":{"message":"No auth","code":401}}"#), Err(AiError::InvalidKey));
    assert_eq!(
        compatible_reply(402, r#"{"error":{"message":"Insufficient credits","code":402}}"#),
        Err(AiError::NotAllowed("Insufficient credits".into()))
    );
    assert_eq!(compatible_reply(404, r#"{"error":"model \"llama9\" not found"}"#), Err(AiError::Api("model \"llama9\" not found".into())));
    assert_eq!(compatible_reply(502, "<html>"), Err(AiError::Overloaded));
    assert!(matches!(compatible_reply(200, r#"{"choices":[]}"#), Err(AiError::Parse(_))));
}

// ---------------------------------------------------------------------------------------------
// Live checks

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn pem_files_give_their_certificates_and_nothing_else() {
    assert!(pem_certificates(b"not a certificate").is_empty());
    let pem = b"Certificate:\n    Data: text Android adds\n-----BEGIN CERTIFICATE-----\nMAA=\n-----END CERTIFICATE-----\n";
    assert_eq!(pem_certificates(pem).len(), 1);
}

/// A one-page PDF for the live checks.
const ONE_PAGE: &[u8] = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF";

/// One real question to `provider`'s `model` with the key in `env`, then a follow-up (the
/// conversation going back as the service gave it).
#[cfg(not(target_arch = "wasm32"))]
fn live(provider: Provider, model: &str, env: &str) {
    let key = std::env::var(env).unwrap_or_default();
    let mut req = of(provider, model, request(ONE_PAGE, &[], Task::Ask("How many pages does this document have? Answer with a number.".into())));
    req.key = &key;
    req.pages = 1;
    let reply = send(&req).unwrap();
    assert!(reply.text.contains('1'), "{reply:?}");
    let history = [Turn { prompt: req.task.prompt(), answer: reply.content.clone(), results: vec![] }];
    let mut next = of(provider, model, request(ONE_PAGE, &history, Task::Ask("And how many words are on it? Answer with a number.".into())));
    next.key = &key;
    next.pages = 1;
    let again = send(&next).unwrap();
    assert!(again.text.contains('0'), "{again:?}");
}

/// Live: `ANTHROPIC_API_KEY=… cargo test -p pdfcraft-ai -- --ignored anthropic`. A fraction of a cent.
#[test]
#[cfg(not(target_arch = "wasm32"))]
#[ignore = "needs network access and an API key"]
fn live_anthropic() {
    live(Provider::Anthropic, "claude-haiku-5-5", "ANTHROPIC_API_KEY");
}

/// Live: `OPENAI_API_KEY=… cargo test -p pdfcraft-ai -- --ignored openai`. A fraction of a cent.
#[test]
#[cfg(not(target_arch = "wasm32"))]
#[ignore = "needs network access and an API key"]
fn live_openai() {
    live(Provider::OpenAi, "gpt-6-luna", "OPENAI_API_KEY");
}

/// Live: `GEMINI_API_KEY=… cargo test -p pdfcraft-ai -- --ignored gemini`. Free on Google's free
/// tier.
#[test]
#[cfg(not(target_arch = "wasm32"))]
#[ignore = "needs network access and an API key"]
fn live_gemini() {
    live(Provider::Gemini, "gemini-3.5-flash-lite", "GEMINI_API_KEY");
}
