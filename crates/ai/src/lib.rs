//! The optional AI assistant: summarize a document, answer questions about it, translate a page.
//!
//! Acrobat's AI Assistant is a cloud service tied to Adobe accounts; PdfCraft's is the user's own:
//! it talks to Anthropic's Messages API with the user's own API key, and only when the user asks.
//! Nothing is sent until a key is set and a question is asked, and each request carries the open
//! document and the question, nothing else.
//!
//! The document goes as a PDF `document` block, so Claude reads its text and sees its pages
//! (scans included). The block is marked for prompt caching: a follow-up question about the same
//! document reads it back from the cache for a fraction of the first request's price.
//!
//! Every call blocks until the answer arrives (seconds to a few minutes): run it on a worker thread.
//! The web build has no assistant: [`send`] says so (a browser page can't call the API directly).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use base64::Engine as _;
use serde_json::{Value, json};

#[cfg(not(target_arch = "wasm32"))]
const API_URL: &str = "https://api.anthropic.com/v1/messages";
#[cfg(not(target_arch = "wasm32"))]
const API_VERSION: &str = "2023-06-01";
/// Server-side refusal fallbacks in their `"default"` form: a declined request is re-run on the
/// model Anthropic recommends for that kind of refusal, in the same call.
#[cfg(not(target_arch = "wasm32"))]
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";
/// The Messages API takes requests up to 32 MB, and base64 makes a PDF a third larger.
pub const MAX_PDF_BYTES: usize = 22 * 1024 * 1024;
/// The most pages the API reads from one PDF on these models.
pub const API_MAX_PAGES: usize = 600;
/// The most earlier exchanges a question carries. Each one is read again by every later question,
/// so a longer conversation costs more and more: past this, the caller starts a new one.
pub const MAX_TURNS: usize = 20;
/// Room for the answer. A request this size stays well inside the HTTP timeout without streaming.
const MAX_TOKENS: u32 = 16_000;
/// The longest answer read back, in bytes.
#[cfg(not(target_arch = "wasm32"))]
const MAX_RESPONSE: u64 = 8 << 20;
/// How long one request may take: thinking on a long document can take minutes.
#[cfg(not(target_arch = "wasm32"))]
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
/// The longest API key accepted (Anthropic's keys are about a hundred characters).
const MAX_KEY: usize = 512;
/// The longest refusal category kept (the API's are one word).
const MAX_CATEGORY: usize = 40;

/// The Claude model that answers. Opus is the default; Sonnet and Haiku cost less per question.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Model {
    #[default]
    Opus,
    Sonnet,
    Haiku,
}

/// List prices in US dollars per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Prices {
    input: f64,
    output: f64,
    cache_write: f64,
    cache_read: f64,
}

impl Model {
    pub const ALL: [Self; 3] = [Self::Opus, Self::Sonnet, Self::Haiku];

    /// The model id the API takes.
    pub fn id(self) -> &'static str {
        match self {
            Self::Opus => "claude-opus-5-5",
            Self::Sonnet => "claude-sonnet-5-5",
            Self::Haiku => "claude-haiku-5-5",
        }
    }

    /// The model a saved [`id`](Self::id) names, or `None`.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.id() == id.trim())
    }

    /// The model's name, as Anthropic writes it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Opus => "Claude Opus 5.5",
            Self::Sonnet => "Claude Sonnet 5.5",
            Self::Haiku => "Claude Haiku 5.5",
        }
    }

    /// Anthropic's list prices (October 2026) for a request whose prompt is `prompt` tokens. A
    /// five-minute cache write costs 1.25 times the input price; a read is far cheaper. Haiku has
    /// two rate cards, the dearer one for prompts over 100K tokens.
    fn prices(self, prompt: u64) -> Prices {
        match self {
            Self::Opus => Prices { input: 4.0, output: 20.0, cache_write: 5.0, cache_read: 0.20 },
            Self::Sonnet => Prices { input: 2.0, output: 10.0, cache_write: 2.5, cache_read: 0.20 },
            Self::Haiku if prompt > 100_000 => Prices { input: 0.50, output: 2.50, cache_write: 0.625, cache_read: 0.05 },
            Self::Haiku => Prices { input: 0.10, output: 0.50, cache_write: 0.125, cache_read: 0.01 },
        }
    }

    /// The input and output prices per million tokens, in US dollars (Haiku's for prompts up to
    /// 100K tokens).
    pub fn list_prices(self) -> (f64, f64) {
        let p = self.prices(0);
        (p.input, p.output)
    }

    /// Whether the server re-runs a declined request on another model. Haiku has no fallback.
    fn fallbacks(self) -> bool {
        matches!(self, Self::Opus | Self::Sonnet)
    }
}

/// What one request used, as the API counts it (thinking is part of `output`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Usage {
    /// Roughly what the request cost on `model`, in US dollars, at list prices.
    pub fn cost_usd(&self, model: Model) -> f64 {
        self.cost_at(model.prices(self.prompt()))
    }

    fn cost_at(&self, p: Prices) -> f64 {
        let per = |tokens: u64, price: f64| tokens as f64 * price / 1_000_000.0;
        per(self.input, p.input) + per(self.output, p.output) + per(self.cache_write, p.cache_write) + per(self.cache_read, p.cache_read)
    }

    /// The prompt's tokens, cached or not.
    pub fn prompt(&self) -> u64 {
        self.input.saturating_add(self.cache_write).saturating_add(self.cache_read)
    }

    /// All the tokens read and written.
    pub fn total(&self) -> u64 {
        self.prompt().saturating_add(self.output)
    }
}

/// The list prices of the model the API names `id`: the assistant's own models, and the ones a
/// declined request may fall back to (Claude Opus 4.8 and Claude Opus 5, $5 / $25, reads at a
/// tenth of the input price). `None` for any other.
fn prices_of(id: &str, prompt: u64) -> Option<Prices> {
    // A dated id ("…-20261001") names the same model.
    let id = id.trim();
    let id = match id.rsplit_once('-') {
        Some((base, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => base,
        _ => id,
    };
    if let Some(model) = Model::from_id(id) {
        return Some(model.prices(prompt));
    }
    match id {
        "claude-opus-4-8" | "claude-opus-5" => Some(Prices { input: 5.0, output: 25.0, cache_write: 6.25, cache_read: 0.50 }),
        // Sonnet 5.5's fallback, at Sonnet 5.5's prices.
        "claude-sonnet-5" => Some(Model::Sonnet.prices(prompt)),
        _ => None,
    }
}

/// What the assistant is asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Task {
    /// Summarize the whole document.
    Summarize,
    /// Answer a question about the document. Follow-up questions keep the conversation.
    Ask(String),
    /// Translate one page (1-based) into a language, named in English ("French").
    Translate { page: usize, language: String },
}

impl Task {
    /// The instruction the model gets (kept with the answer for the next turn).
    pub fn prompt(&self) -> String {
        match self {
            Self::Summarize => {
                "Summarize this document: what it is, its main points, and anything it asks of the reader (dates, amounts, obligations).".into()
            }
            Self::Ask(question) => question.trim().to_owned(),
            Self::Translate { page, language } => {
                format!("Translate page {page} of this document into {language}. Reply with the translation only, keeping its paragraphs and lists.")
            }
        }
    }
}

/// One earlier question and its answer, as the API returned it (kept whole so the conversation
/// can continue exactly as the model left it).
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub prompt: String,
    pub answer: Value,
    /// What became of the changes the answer proposed: (the proposal's id, what happened), sent
    /// back with the next question. A proposal left out is reported as not applied.
    pub results: Vec<(String, String)>,
}

/// The most changes one answer may propose; past this they are reported as not applied.
pub const MAX_EDITS: usize = 200;
/// The longest passage, or replacement, one change may have (in characters).
const MAX_EDIT_TEXT: usize = 20_000;
/// What a proposal the user hasn't applied is reported as.
const NOT_APPLIED: &str = "Not applied: the user didn't apply this change.";

/// A change to the document the assistant proposes: on `page` (1-based), the passage `find`, as it
/// reads in the document, becomes `replace`. Nothing changes until the user applies it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProposedEdit {
    /// The API's id for the proposal, to report what became of it.
    pub id: String,
    pub page: usize,
    pub find: String,
    pub replace: String,
}

/// The tool the model proposes changes with. Its name and wording stay the same for every
/// question, so the cached prefix stays valid.
fn edit_tool() -> Value {
    json!({
        "name": "edit_text",
        "description": "Propose a change to the text of the open PDF, keeping its layout: on page `page`, the passage `find` becomes `replace`. \
            Quote `find` exactly as it reads in the document, within one paragraph; the paragraph keeps its font, size, colour and box and is rewrapped. \
            Use one call per passage, and keep a replacement about as long as what it replaces so it fits. \
            The user sees every proposal and decides whether to apply them; use this only when asked to change the document itself.",
        "input_schema": {
            "type": "object",
            "properties": {
                "page": {"type": "integer", "minimum": 1, "description": "The page number, from 1."},
                "find": {"type": "string", "description": "The passage to change, exactly as it reads in the document."},
                "replace": {"type": "string", "description": "The new text for that passage."},
            },
            "required": ["page", "find", "replace"],
        },
    })
}

/// A request: the document, the conversation so far, and the new task.
pub struct Request<'a> {
    pub key: &'a str,
    pub model: Model,
    /// The document's file name, shown to the model as its title.
    pub title: &'a str,
    pub pdf: &'a [u8],
    pub pages: usize,
    /// The most pages a request may send (a cost guard the user sets); at most [`API_MAX_PAGES`].
    pub max_pages: usize,
    /// The language answers are written in, named in English ("French").
    pub language: &'a str,
    pub history: &'a [Turn],
    pub task: Task,
    /// The assistant may propose changes to the document (it can be edited). Keep it the same for
    /// a whole conversation.
    pub edits: bool,
}

/// An answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    /// The answer's text.
    pub text: String,
    /// The answer as the API returned it, for the next turn of the conversation.
    pub content: Value,
    pub usage: Usage,
    /// The answer stopped at the length limit.
    pub truncated: bool,
    /// The model that answered (a fallback model, when the first one declined).
    pub model: String,
    /// The model asked declined and another one answered.
    pub fell_back: bool,
    /// The changes to the document the answer proposes (at most [`MAX_EDITS`]).
    pub edits: Vec<ProposedEdit>,
}

impl Reply {
    /// Roughly what the answer cost, in US dollars, at the list prices of the model that gave it;
    /// `None` when that model's prices aren't known here. When another model answered, the
    /// attempt that declined isn't counted.
    pub fn cost_usd(&self) -> Option<f64> {
        prices_of(&self.model, self.usage.prompt()).map(|p| self.usage.cost_at(p))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AiError {
    #[error("no API key: add your Anthropic API key first")]
    NoKey,
    #[error("the API key isn't usable: it must be one line of plain characters")]
    BadKey,
    #[error("the document has no pages to send")]
    Empty,
    #[error("the question is empty")]
    NoQuestion,
    #[error("the model gave no answer")]
    NoAnswer,
    #[error("the document has {pages} pages, more than the {max} the assistant may read")]
    TooManyPages { pages: usize, max: usize },
    #[error("the document is {mb} MB, more than the {max_mb} MB the assistant may read")]
    TooLarge { mb: usize, max_mb: usize },
    #[error("the API key was refused")]
    InvalidKey,
    #[error("the account can't use this model, or has no credit left: {0}")]
    NotAllowed(String),
    #[error("too many requests right now; try again in a moment")]
    RateLimited,
    #[error("the service is overloaded right now; try again in a moment")]
    Overloaded,
    #[error("the request was declined ({0})")]
    Refused(String),
    #[error("the service answered: {0}")]
    Api(String),
    #[error("couldn't reach the service ({0})")]
    Network(String),
    #[error("the answer couldn't be read ({0})")]
    Parse(String),
}

/// The request body for `req`, after its size checks.
pub fn request_body(req: &Request) -> Result<Value, AiError> {
    let key = req.key.trim();
    if key.is_empty() {
        return Err(AiError::NoKey);
    }
    if key.len() > MAX_KEY || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(AiError::BadKey);
    }
    if req.pdf.is_empty() || req.pages == 0 {
        return Err(AiError::Empty);
    }
    if req.task.prompt().is_empty() || req.history.iter().any(|t| t.prompt.trim().is_empty()) {
        return Err(AiError::NoQuestion);
    }
    let max = req.max_pages.clamp(1, API_MAX_PAGES);
    if req.pages > max {
        return Err(AiError::TooManyPages { pages: req.pages, max });
    }
    if req.pdf.len() > MAX_PDF_BYTES {
        return Err(AiError::TooLarge { mb: req.pdf.len().div_ceil(1 << 20), max_mb: MAX_PDF_BYTES >> 20 });
    }
    let document = json!({
        "type": "document",
        "source": {"type": "base64", "media_type": "application/pdf", "data": base64::engine::general_purpose::STANDARD.encode(req.pdf)},
        "title": req.title,
        // The prefix up to here (system prompt and document) is read back from the cache by
        // every later question about this document.
        "cache_control": {"type": "ephemeral"},
    });
    // The first question rides with the document; later ones follow the answers.
    let mut prompts = req.history.iter().map(|t| t.prompt.clone()).chain(std::iter::once(req.task.prompt()));
    let first = prompts.next().unwrap_or_default();
    let mut messages = vec![json!({"role": "user", "content": [document, {"type": "text", "text": first}]})];
    for (turn, prompt) in req.history.iter().zip(prompts) {
        messages.push(json!({"role": "assistant", "content": turn.answer}));
        // What became of the answer's proposals comes first, one result for each, then the question.
        let mut content = tool_results(turn);
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
        "model": req.model.id(),
        "max_tokens": MAX_TOKENS,
        "output_config": {"effort": "medium"},
        "system": system_prompt(req.language, req.edits),
        "messages": messages,
    });
    // A conversation that has used the tool keeps it defined, so every proposal in its history
    // has its tool beside it (and the prefix stays the same).
    let tool_used = req.history.iter().any(|t| t.answer.as_array().is_some_and(|b| b.iter().any(|b| b["type"] == "tool_use")));
    if let Some(map) = body.as_object_mut() {
        if req.edits || tool_used {
            map.insert("tools".into(), json!([edit_tool()]));
        }
        if req.model.fallbacks() {
            map.insert("fallbacks".into(), json!("default"));
        }
    }
    Ok(body)
}

/// The results the next question carries for `turn`'s proposals: one for every `tool_use` in its
/// answer (the API refuses a conversation that leaves one out), in the answer's order.
fn tool_results(turn: &Turn) -> Vec<Value> {
    let Some(blocks) = turn.answer.as_array() else { return Vec::new() };
    blocks
        .iter()
        .filter(|b| b["type"] == "tool_use")
        .filter_map(|b| b["id"].as_str())
        .map(|id| {
            let result = turn.results.iter().find(|(r, _)| r == id).map_or(NOT_APPLIED, |(_, text)| text.as_str());
            json!({"type": "tool_result", "tool_use_id": id, "content": result})
        })
        .collect()
}

/// The system prompt. It stays the same for every question about a document, so the cached
/// prefix stays valid.
fn system_prompt(language: &str, edits: bool) -> String {
    let edits = if edits {
        " When the user asks you to change the document itself (correct, reword, shorten or translate its text in place), propose the changes \
         with the edit_text tool, then say in a sentence or two what you changed; the user reviews them and applies them or not. \
         Otherwise don't use the tool."
    } else {
        " The document can't be changed here: if the user asks for changes, say so, and give the new text in your answer instead."
    };
    format!(
        "You are the assistant built into PdfCraft, a PDF application. The user has the attached PDF open and asks about it. \
         Work from what the document says; when it doesn't say something, say so rather than guessing. Refer to pages as (p. N). \
         The answer is read on a phone: write plain text in short paragraphs, with simple \"- \" lists when a list helps, \
         and no tables, headings or other Markdown.{edits} Reply in {language}."
    )
}

/// The web build: no assistant.
#[cfg(target_arch = "wasm32")]
pub fn send(req: &Request) -> Result<Reply, AiError> {
    request_body(req)?;
    Err(AiError::Network("the assistant isn't available in the web version".into()))
}

/// Send `req` and wait for the answer.
#[cfg(not(target_arch = "wasm32"))]
pub fn send(req: &Request) -> Result<Reply, AiError> {
    let body = request_body(req)?.to_string();
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        // Error statuses carry the API's explanation in their body.
        .http_status_as_error(false)
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(roots()?).build())
        .build()
        .new_agent();
    let mut request =
        agent.post(API_URL).header("x-api-key", req.key.trim()).header("anthropic-version", API_VERSION).content_type("application/json");
    if req.model.fallbacks() {
        request = request.header("anthropic-beta", FALLBACK_BETA);
    }
    let mut response = request.send(body.as_bytes()).map_err(|e| AiError::Network(e.to_string()))?;
    let status = response.status().as_u16();
    let text = response.body_mut().with_config().limit(MAX_RESPONSE).read_to_string().map_err(|e| AiError::Network(e.to_string()))?;
    parse_response(status, &text)
}

/// Read the API's answer (`status` and body).
pub fn parse_response(status: u16, body: &str) -> Result<Reply, AiError> {
    if !(200..300).contains(&status) {
        // An error page from a proxy or a captive portal isn't JSON: the status says enough.
        let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
        let message = v["error"]["message"].as_str().unwrap_or_default().chars().take(400).collect::<String>();
        return Err(match (status, v["error"]["type"].as_str().unwrap_or_default()) {
            (401, _) | (_, "authentication_error") => AiError::InvalidKey,
            (403, _) | (_, "permission_error") | (402, _) | (_, "billing_error") => AiError::NotAllowed(message),
            (429, _) | (_, "rate_limit_error") => AiError::RateLimited,
            (529, _) | (_, "overloaded_error") => AiError::Overloaded,
            _ if message.is_empty() => AiError::Api(format!("HTTP {status}")),
            _ => AiError::Api(message),
        });
    }
    let v: Value = serde_json::from_str(body).map_err(|e| AiError::Parse(e.to_string()))?;
    // Check the stop reason before the content: a declined request has nothing to read.
    let stop = v["stop_reason"].as_str().unwrap_or_default();
    if stop == "refusal" {
        let category = v["stop_details"]["category"].as_str().unwrap_or("policy").chars().take(MAX_CATEGORY).collect();
        return Err(AiError::Refused(category));
    }
    let content = v.get("content").filter(|c| c.is_array()).cloned().ok_or_else(|| AiError::Parse("no content".into()))?;
    let text = content
        .as_array()
        .map(|blocks| blocks.iter().filter(|b| b["type"] == "text").filter_map(|b| b["text"].as_str()).collect::<Vec<_>>().join("\n\n"))
        .unwrap_or_default();
    let truncated = matches!(stop, "max_tokens" | "model_context_window_exceeded");
    let edits = proposed_edits(&content, truncated);
    // An answer with no text and nothing proposed (all of it spent thinking) can't be shown, and
    // mustn't become part of the conversation.
    let text = text.trim();
    if text.is_empty() && edits.is_empty() {
        return Err(AiError::NoAnswer);
    }
    let n = |field: &str| v["usage"][field].as_u64().unwrap_or(0);
    let usage = Usage {
        input: n("input_tokens"),
        output: n("output_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        cache_read: n("cache_read_input_tokens"),
    };
    // The served-by signal: a fallback model's attempt in the per-attempt usage.
    let fell_back = v["usage"]["iterations"].as_array().is_some_and(|i| i.iter().any(|e| e["type"] == "fallback_message"));
    Ok(Reply {
        text: text.to_owned(),
        content,
        usage,
        truncated,
        model: v["model"].as_str().unwrap_or_default().chars().take(MAX_CATEGORY * 2).collect(),
        fell_back,
        edits,
    })
}

/// The well-formed `edit_text` proposals in an answer's content, at most [`MAX_EDITS`]. One that
/// is malformed is left out (its result then says it wasn't applied), and so is the last one of
/// an answer cut off at its length limit: its replacement may stop mid-way.
fn proposed_edits(content: &Value, truncated: bool) -> Vec<ProposedEdit> {
    let Some(blocks) = content.as_array() else { return Vec::new() };
    let cut = if truncated { blocks.iter().rposition(|b| b["type"] == "tool_use") } else { None };
    blocks
        .iter()
        .enumerate()
        .filter(|(i, _)| Some(*i) != cut)
        .map(|(_, b)| b)
        .filter(|b| b["type"] == "tool_use" && b["name"] == "edit_text")
        .filter_map(|b| {
            let input = &b["input"];
            let page = usize::try_from(input["page"].as_u64()?).ok().filter(|p| *p >= 1)?;
            let find = input["find"].as_str().filter(|f| !f.trim().is_empty() && f.chars().count() <= MAX_EDIT_TEXT)?;
            let replace = input["replace"].as_str().filter(|r| r.chars().count() <= MAX_EDIT_TEXT)?;
            Some(ProposedEdit { id: b["id"].as_str()?.to_owned(), page, find: find.to_owned(), replace: replace.to_owned() })
        })
        .take(MAX_EDITS)
        .collect()
}

/// The certificate authorities the device trusts.
#[cfg(not(target_arch = "wasm32"))]
fn roots() -> Result<ureq::tls::RootCerts, AiError> {
    let certs = system_roots();
    if certs.is_empty() {
        return Err(AiError::Network("no trusted certificates found on this device".into()));
    }
    Ok(ureq::tls::RootCerts::new_with_certs(&certs))
}

#[cfg(not(any(target_os = "android", target_arch = "wasm32")))]
fn system_roots() -> Vec<ureq::tls::Certificate<'static>> {
    rustls_native_certs::load_native_certs().certs.iter().map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned()).collect()
}

/// Android keeps the authorities it trusts as one PEM file each, in the system's `cacerts`.
#[cfg(target_os = "android")]
fn system_roots() -> Vec<ureq::tls::Certificate<'static>> {
    const DIR: &str = "/system/etc/security/cacerts";
    // A certificate file is a few kilobytes; the folder holds a few hundred.
    const MAX_FILE: u64 = 64 * 1024;
    const MAX_FILES: usize = 2000;
    let Ok(entries) = std::fs::read_dir(DIR) else { return Vec::new() };
    entries
        .flatten()
        .take(MAX_FILES)
        .map(|e| e.path())
        // Follows links (an entry's own metadata wouldn't): some builds link the files in.
        .filter(|path| std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE))
        .filter_map(|path| std::fs::read(path).ok())
        .flat_map(|bytes| pem_certificates(&bytes))
        .collect()
}

/// The certificates in a PEM file (other text in it is skipped).
#[cfg(all(not(target_arch = "wasm32"), any(target_os = "android", test)))]
fn pem_certificates(bytes: &[u8]) -> Vec<ureq::tls::Certificate<'static>> {
    use rustls_pki_types::pem::PemObject as _;
    rustls_pki_types::CertificateDer::pem_slice_iter(bytes)
        .filter_map(Result::ok)
        .map(|c| ureq::tls::Certificate::from_der(c.as_ref()).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(pdf: &'a [u8], history: &'a [Turn], task: Task) -> Request<'a> {
        Request {
            key: "sk-ant-test",
            model: Model::Opus,
            title: "report.pdf",
            pdf,
            pages: 3,
            max_pages: 100,
            language: "French",
            history,
            task,
            edits: false,
        }
    }

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
        // Thinking is left to the model (it can't be turned off on Opus 5.5), and nothing is prefilled.
        assert!(body.get("thinking").is_none());
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
        let r = parse_response(200, answer).unwrap();
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
        assert_eq!(parse_response(200, &only).unwrap().edits.len(), 2);
        // Cut off at the length limit: the last proposal may stop mid-way, so it isn't offered.
        let cut = answer.replace("\"tool_use\",\"content\"", "\"max_tokens\",\"content\"");
        let r_cut = parse_response(200, &cut).unwrap();
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
        req.model = Model::Haiku;
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
    }

    #[test]
    fn answers_are_read_after_their_stop_reason() {
        let ok = r#"{"model":"claude-opus-5-5","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"","signature":"s"},{"type":"text","text":"Two points."}],"usage":{"input_tokens":12,"output_tokens":40,"cache_creation_input_tokens":3000,"cache_read_input_tokens":0}}"#;
        let r = parse_response(200, ok).unwrap();
        assert_eq!(r.text, "Two points.");
        assert_eq!(r.content.as_array().map(Vec::len), Some(2), "kept whole for the next turn");
        assert_eq!(r.usage, Usage { input: 12, output: 40, cache_write: 3000, cache_read: 0 });
        assert!(!r.truncated);
        assert!(!r.fell_back);
        let cut = ok.replace("end_turn", "max_tokens");
        assert!(parse_response(200, &cut).unwrap().truncated);
        let full = ok.replace("end_turn", "model_context_window_exceeded");
        assert!(parse_response(200, &full).unwrap().truncated);
        let declined = r#"{"stop_reason":"refusal","stop_details":{"type":"refusal","category":"cyber"},"content":[]}"#;
        assert_eq!(parse_response(200, declined), Err(AiError::Refused("cyber".into())));
        let long = declined.replace("cyber", &"x".repeat(5000));
        assert!(matches!(parse_response(200, &long), Err(AiError::Refused(c)) if c.len() == MAX_CATEGORY));
    }

    #[test]
    fn an_answer_without_text_is_no_answer() {
        let thought_only = r#"{"model":"claude-opus-5-5","stop_reason":"max_tokens","content":[{"type":"thinking","thinking":"","signature":"s"}],"usage":{"input_tokens":1,"output_tokens":16000}}"#;
        assert_eq!(parse_response(200, thought_only), Err(AiError::NoAnswer));
        let blank = r#"{"model":"claude-opus-5-5","stop_reason":"end_turn","content":[{"type":"text","text":"  \n "}],"usage":{}}"#;
        assert_eq!(parse_response(200, blank), Err(AiError::NoAnswer));
    }

    #[test]
    fn a_fallback_answer_is_priced_as_the_model_that_gave_it() {
        let body = r#"{"model":"claude-opus-4-8","stop_reason":"end_turn","content":[{"type":"fallback","from":{"model":"claude-opus-5-5"},"to":{"model":"claude-opus-4-8"}},{"type":"text","text":"Here."}],"usage":{"input_tokens":1000000,"output_tokens":1000000,"iterations":[{"type":"message"},{"type":"fallback_message"}]}}"#;
        let r = parse_response(200, body).unwrap();
        assert!(r.fell_back);
        assert_eq!(r.text, "Here.");
        assert!((r.cost_usd().unwrap() - 30.0).abs() < 1e-9, "Opus 4.8's $5 / $25, not Opus 5.5's $4 / $20");
        let unknown = body.replace("claude-opus-4-8\",\"stop", "claude-somethingelse\",\"stop");
        assert_eq!(parse_response(200, &unknown).unwrap().cost_usd(), None, "no made-up price");
        let dated = body.replace("claude-opus-4-8\",\"stop", "claude-opus-5-5-20261001\",\"stop");
        assert!((parse_response(200, &dated).unwrap().cost_usd().unwrap() - 24.0).abs() < 1e-9);
    }

    #[test]
    fn api_errors_say_what_to_do() {
        let error = |kind: &str, message: &str| format!(r#"{{"type":"error","error":{{"type":"{kind}","message":"{message}"}}}}"#);
        assert_eq!(parse_response(401, &error("authentication_error", "invalid x-api-key")), Err(AiError::InvalidKey));
        assert_eq!(parse_response(429, &error("rate_limit_error", "slow down")), Err(AiError::RateLimited));
        assert_eq!(parse_response(529, &error("overloaded_error", "busy")), Err(AiError::Overloaded));
        assert_eq!(parse_response(400, &error("invalid_request_error", "prompt is too long")), Err(AiError::Api("prompt is too long".into())));
        assert!(matches!(parse_response(403, &error("permission_error", "no access")), Err(AiError::NotAllowed(_))));
        assert_eq!(
            parse_response(400, &error("billing_error", "credit balance is too low")),
            Err(AiError::NotAllowed("credit balance is too low".into()))
        );
        assert_eq!(parse_response(502, "<html>bad gateway</html>"), Err(AiError::Api("HTTP 502".into())), "a proxy's page is not a parse error");
        assert_eq!(parse_response(407, ""), Err(AiError::Api("HTTP 407".into())));
        assert!(matches!(parse_response(200, "<html>portal</html>"), Err(AiError::Parse(_))));
    }

    #[test]
    fn costs_follow_the_list_prices() {
        let usage = Usage { input: 1_000_000, output: 1_000_000, cache_write: 0, cache_read: 0 };
        assert!((usage.cost_usd(Model::Opus) - 24.0).abs() < 1e-9);
        assert!((usage.cost_usd(Model::Sonnet) - 12.0).abs() < 1e-9);
        let cached = Usage { input: 0, output: 0, cache_write: 0, cache_read: 1_000_000 };
        assert!((cached.cost_usd(Model::Opus) - 0.20).abs() < 1e-9);
        // Haiku's dearer rate card starts past 100K prompt tokens.
        let short = Usage { input: 100_000, output: 0, cache_write: 0, cache_read: 0 };
        assert!((short.cost_usd(Model::Haiku) - 0.01).abs() < 1e-9);
        let long = Usage { input: 50_000, output: 1_000_000, cache_write: 0, cache_read: 60_000 };
        assert!((long.cost_usd(Model::Haiku) - (0.025 + 2.5 + 0.003)).abs() < 1e-9);
        for m in Model::ALL {
            assert_eq!(Model::from_id(m.id()), Some(m));
        }
        assert_eq!(Model::from_id("claude-unknown"), None);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn pem_files_give_their_certificates_and_nothing_else() {
        assert!(pem_certificates(b"not a certificate").is_empty());
        let pem = b"Certificate:\n    Data: text Android adds\n-----BEGIN CERTIFICATE-----\nMAA=\n-----END CERTIFICATE-----\n";
        assert_eq!(pem_certificates(pem).len(), 1);
    }

    /// Live: one real question (`ANTHROPIC_API_KEY=… cargo test -p pdfcraft-ai -- --ignored`).
    /// It costs a fraction of a cent.
    #[test]
    #[ignore = "needs network access and an API key"]
    fn a_real_question_gets_an_answer() {
        let key = std::env::var("ANTHROPIC_API_KEY").unwrap_or_default();
        let pdf = b"%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj\ntrailer << /Root 1 0 R >>\n%%EOF";
        let mut req = request(pdf, &[], Task::Ask("How many pages does this document have? Answer with a number.".into()));
        req.key = &key;
        req.model = Model::Haiku;
        req.pages = 1;
        let reply = send(&req).unwrap();
        assert!(reply.text.contains('1'), "{reply:?}");
    }
}
