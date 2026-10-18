//! The optional AI assistant: summarize a document, answer questions about it, translate a page,
//! propose changes to its text.
//!
//! Acrobat's AI Assistant is a cloud service tied to Adobe accounts; PdfCraft's is the user's own:
//! it talks to the service the user chooses, with the user's own key, and only when the user asks.
//! Anthropic's Claude ([`anthropic`]), OpenAI's ChatGPT models ([`openai`]) and Google's Gemini
//! ([`gemini`]) read the PDF itself, its text and its pages (scans included). Any other service
//! that speaks OpenAI's Chat Completions ([`compatible`]: OpenRouter, Mistral, DeepSeek, Groq, a
//! local Ollama…) gets the document's text, page by page, unless it reads PDF files too. Nothing
//! is sent until the service is set up and a question is asked, and each request carries the open
//! document and the conversation about it, nothing else.
//!
//! Each service is asked the same way: one system prompt, the document with the first question,
//! then every earlier answer exactly as the service gave it (its thinking included, which the
//! services check), what became of the changes it proposed, and the next question.
//!
//! Every call blocks until the answer arrives (seconds to a few minutes): run it on a worker thread.
//! The web build has no assistant: [`send`] says so (a browser page can't call the APIs directly).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde_json::{Value, json};

mod anthropic;
mod compatible;
mod gemini;
mod openai;

/// The largest PDF sent. Anthropic's Messages API takes requests up to 32 MB, and base64 makes a
/// PDF a third larger; OpenAI and Gemini take PDFs up to 50 MB.
pub const MAX_PDF_BYTES: usize = 22 * 1024 * 1024;
/// The most pages a request carries: what Anthropic reads from one PDF (Gemini reads 1000).
pub const API_MAX_PAGES: usize = 600;
/// The most earlier exchanges a question carries. Each one is read again by every later question,
/// so a longer conversation costs more and more: past this, the caller starts a new one.
pub const MAX_TURNS: usize = 20;
/// The most of a document's text sent to a service that doesn't read PDF files, in characters
/// (about 400,000 tokens).
pub const MAX_TEXT_CHARS: usize = 1_600_000;
/// The longest answer read back, in bytes.
#[cfg(not(target_arch = "wasm32"))]
const MAX_RESPONSE: u64 = 8 << 20;
/// How long one request may take: thinking on a long document can take minutes.
#[cfg(not(target_arch = "wasm32"))]
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);
/// The longest API key accepted (the services' keys are about a hundred characters).
const MAX_KEY: usize = 512;
/// The longest model id accepted.
const MAX_MODEL: usize = 200;
/// The longest address of a compatible service.
const MAX_URL: usize = 2048;
/// The largest request sent (the document and the conversation, as JSON): what the services take,
/// and what a phone can hold twice.
#[cfg(not(target_arch = "wasm32"))]
const MAX_BODY: usize = 48 << 20;
/// The longest reason for a refusal kept.
const MAX_CATEGORY: usize = 40;

/// The service that answers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Provider {
    /// Anthropic's Claude.
    #[default]
    Anthropic,
    /// OpenAI's models (ChatGPT's).
    OpenAi,
    /// Google's Gemini.
    Gemini,
    /// Any service that speaks OpenAI's Chat Completions, at the address the user gives.
    Compatible,
}

impl Provider {
    pub const ALL: [Self; 4] = [Self::Anthropic, Self::OpenAi, Self::Gemini, Self::Compatible];

    /// Its name in the settings.
    pub fn id(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
            Self::Gemini => "gemini",
            Self::Compatible => "compatible",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.id() == id.trim())
    }

    /// Its place in [`Self::ALL`].
    pub fn index(self) -> usize {
        match self {
            Self::Anthropic => 0,
            Self::OpenAi => 1,
            Self::Gemini => 2,
            Self::Compatible => 3,
        }
    }

    /// Its name, as its maker writes it; the compatible services have none of their own (the
    /// interface names them).
    pub fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "Claude (Anthropic)",
            Self::OpenAi => "ChatGPT (OpenAI)",
            Self::Gemini => "Gemini (Google)",
            Self::Compatible => "OpenAI-compatible",
        }
    }

    /// The models of its the assistant offers, best first (none for a compatible service: the
    /// user names one).
    pub fn models(self) -> impl Iterator<Item = &'static Known> {
        KNOWN.iter().filter(move |k| k.provider == self && k.listed)
    }

    /// The model asked until the user picks one.
    pub fn default_model(self) -> &'static str {
        match self {
            Self::Anthropic => "claude-opus-5-5",
            // OpenAI's own default.
            Self::OpenAi => "gpt-6.1-sol",
            // Google's recommendation for new projects, with a free tier.
            Self::Gemini => "gemini-3.8-flash",
            Self::Compatible => "",
        }
    }

    /// A request needs a key (a compatible service on the device or the local network may not).
    pub fn needs_key(self) -> bool {
        self != Self::Compatible
    }
}

/// List prices in US dollars per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Prices {
    input: f64,
    output: f64,
    cache_write: f64,
    cache_read: f64,
}

impl Prices {
    /// The prices of a service that caches by itself: what it writes to its cache is plain input.
    const fn cached_by_itself(input: f64, output: f64, cache_read: f64) -> Self {
        Self { input, output, cache_write: input, cache_read }
    }
}

/// A model the assistant knows: who makes it, the id its API takes, its name and list prices.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Known {
    pub provider: Provider,
    pub id: &'static str,
    pub name: &'static str,
    prices: Prices,
    /// The dearer prices of a request whose prompt is longer than this many tokens, for the whole
    /// request.
    long: Option<(u64, Prices)>,
    /// Offered to choose; the others are only priced (a declined request's fallback).
    listed: bool,
    /// The service has a free tier for it, on which what is sent may be used to improve it.
    pub free_tier: bool,
    /// Its API takes an effort level (`output_config.effort`): Claude's from Opus 4.5 and Sonnet
    /// 4.6 on, not Haiku 4.5, which answers it with an error.
    effort: bool,
}

impl Known {
    fn prices(&self, prompt: u64) -> Prices {
        match self.long {
            Some((over, dearer)) if prompt > over => dearer,
            _ => self.prices,
        }
    }

    /// The input and output prices per million tokens, in US dollars (the cheaper card's).
    pub fn list_prices(&self) -> (f64, f64) {
        (self.prices.input, self.prices.output)
    }

    /// Its API takes an effort level.
    pub fn effort(&self) -> bool {
        self.effort
    }

    /// The server re-runs a declined request on another model (Anthropic's, but Haiku).
    fn fallbacks(&self) -> bool {
        matches!(self.id, "claude-opus-5-5" | "claude-sonnet-5-5")
    }
}

/// The models the assistant knows, with their list prices (October 2026):
///
/// - Anthropic's (platform.claude.com/docs/en/about-claude/pricing). A five-minute cache write
///   costs 1.25 times the input price; a read 0.05 times on Opus 5.5 and Sonnet 5.5, 0.1 times on
///   the others. Haiku 5.5's prompts over 100K tokens cost more. Opus 4.8, Opus 5 and Sonnet 5
///   are also a declined request's fallbacks.
/// - OpenAI's Standard prices (developers.openai.com/api/docs/pricing). A prompt over 272K tokens
///   costs twice the input and cache prices and 1.5 times the output price, for the whole request.
/// - Google's paid tier, Standard (ai.google.dev/gemini-api/docs/pricing), at the regular prices:
///   Gemini 4 Argon costs half until 30 November 2026, Gemini 3.8 Flash until 31 December 2026.
///   Gemini caches by itself; output includes thinking.
const KNOWN: [Known; 16] = [
    claude("claude-opus-5-5", "Claude Opus 5.5", Prices { input: 4.0, output: 20.0, cache_write: 5.0, cache_read: 0.20 }, None, true),
    claude("claude-sonnet-5-5", "Claude Sonnet 5.5", Prices { input: 2.0, output: 10.0, cache_write: 2.5, cache_read: 0.10 }, None, true),
    claude(
        "claude-haiku-5-5",
        "Claude Haiku 5.5",
        Prices { input: 0.10, output: 0.50, cache_write: 0.125, cache_read: 0.01 },
        Some((100_000, Prices { input: 0.50, output: 2.50, cache_write: 0.625, cache_read: 0.05 })),
        true,
    ),
    claude("claude-opus-5", "Claude Opus 5", Prices { input: 5.0, output: 25.0, cache_write: 6.25, cache_read: 0.50 }, None, true),
    claude("claude-sonnet-5", "Claude Sonnet 5", Prices { input: 2.0, output: 10.0, cache_write: 2.5, cache_read: 0.20 }, None, true),
    claude("claude-opus-4-8", "Claude Opus 4.8", Prices { input: 5.0, output: 25.0, cache_write: 6.25, cache_read: 0.50 }, None, true),
    claude("claude-opus-4-7", "Claude Opus 4.7", Prices { input: 5.0, output: 25.0, cache_write: 6.25, cache_read: 0.50 }, None, true),
    claude("claude-opus-4-6", "Claude Opus 4.6", Prices { input: 5.0, output: 25.0, cache_write: 6.25, cache_read: 0.50 }, None, true),
    claude("claude-sonnet-4-6", "Claude Sonnet 4.6", Prices { input: 3.0, output: 15.0, cache_write: 3.75, cache_read: 0.30 }, None, true),
    // No effort level (the API refuses one).
    Known {
        effort: false,
        ..claude("claude-haiku-4-5", "Claude Haiku 4.5", Prices { input: 1.0, output: 5.0, cache_write: 1.25, cache_read: 0.10 }, None, true)
    },
    openai(
        "gpt-6-astra",
        "GPT-6 Astra",
        Prices { input: 10.0, output: 50.0, cache_write: 12.5, cache_read: 1.0 },
        Prices { input: 20.0, output: 75.0, cache_write: 25.0, cache_read: 2.0 },
    ),
    openai(
        "gpt-6.1-sol",
        "GPT-6.1 Sol",
        Prices { input: 2.0, output: 10.0, cache_write: 2.5, cache_read: 0.10 },
        Prices { input: 4.0, output: 15.0, cache_write: 5.0, cache_read: 0.20 },
    ),
    openai(
        "gpt-6-luna",
        "GPT-6 Luna",
        Prices { input: 0.10, output: 0.50, cache_write: 0.125, cache_read: 0.01 },
        Prices { input: 0.20, output: 0.75, cache_write: 0.25, cache_read: 0.02 },
    ),
    gemini("gemini-4-argon", "Gemini 4 Argon", Prices::cached_by_itself(4.0, 20.0, 0.20), false),
    gemini("gemini-3.8-flash", "Gemini 3.8 Flash", Prices::cached_by_itself(1.50, 7.50, 0.15), true),
    gemini("gemini-3.5-flash-lite", "Gemini 3.5 Flash-Lite", Prices::cached_by_itself(0.30, 2.50, 0.03), true),
];

const fn claude(id: &'static str, name: &'static str, prices: Prices, long: Option<(u64, Prices)>, listed: bool) -> Known {
    Known { provider: Provider::Anthropic, id, name, prices, long, listed, free_tier: false, effort: true }
}

const fn openai(id: &'static str, name: &'static str, prices: Prices, long: Prices) -> Known {
    Known { provider: Provider::OpenAi, id, name, prices, long: Some((272_000, long)), listed: true, free_tier: false, effort: false }
}

const fn gemini(id: &'static str, name: &'static str, prices: Prices, free_tier: bool) -> Known {
    Known { provider: Provider::Gemini, id, name, prices, long: None, listed: true, free_tier, effort: false }
}

/// The model of `provider` an API names `id`, or `None` (a compatible service's are never
/// known). A dated id ("…-20261001", "…-2026-10-01") names the same model.
pub fn known(provider: Provider, id: &str) -> Option<&'static Known> {
    let id = id.trim();
    KNOWN
        .iter()
        .filter(|k| k.provider == provider && provider != Provider::Compatible)
        .filter(|k| {
            id == k.id
                || id.strip_prefix(k.id).and_then(|rest| rest.strip_prefix('-')).is_some_and(|date| {
                    !date.is_empty() && date.bytes().all(|b| b.is_ascii_digit() || b == b'-') && date.bytes().filter(u8::is_ascii_digit).count() == 8
                })
        })
        // "claude-opus-5-5-…" is Opus 5.5, not Opus 5.
        .max_by_key(|k| k.id.len())
}

/// The host of a compatible service's address, in lower case ("api.mistral.ai"); `None` for an
/// address that isn't one.
pub fn service_host(base_url: &str) -> Option<String> {
    compatible::endpoint(base_url).ok()?;
    let rest = base_url.trim().split_once("://").map(|(_, rest)| rest)?;
    Some(compatible::host_of(rest).to_ascii_lowercase()).filter(|h| !h.is_empty())
}

/// What one request used, as the service counts it (thinking is part of `output`). `input` is the
/// prompt's tokens that were neither read from nor written to a cache.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Usage {
    fn cost_at(&self, p: Prices) -> f64 {
        let per = |tokens: u64, price: f64| tokens as f64 * price / 1_000_000.0;
        per(self.input, p.input) + per(self.output, p.output) + per(self.cache_write, p.cache_write) + per(self.cache_read, p.cache_read)
    }

    /// Roughly what the request cost on `model`, in US dollars, at list prices.
    pub fn cost_usd(&self, model: &Known) -> f64 {
        self.cost_at(model.prices(self.prompt()))
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
            // Short, for a phone, and quicker to write: the user can ask for more.
            Self::Summarize => concat!(
                "Summarize this document briefly: one sentence on what it is, then at most five short \"- \" points with its main points ",
                "and anything it asks of the reader (dates, amounts, obligations). Stay under 120 words."
            )
            .into(),
            Self::Ask(question) => question.trim().to_owned(),
            Self::Translate { page, language } => {
                format!("Translate page {page} of this document into {language}. Reply with the translation only, keeping its paragraphs and lists.")
            }
        }
    }
}

/// One earlier question and its answer, as the service returned it (kept whole so the
/// conversation can continue exactly as the model left it). An answer only goes back to the
/// service that gave it: a conversation starts again with another service or model.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub prompt: String,
    pub answer: Value,
    /// What became of the changes the answer proposed: (the proposal's id, what happened), sent
    /// back with the next question. A proposal left out is reported as not applied.
    pub results: Vec<(String, String)>,
}

impl Turn {
    /// What became of proposal `id`, as the next question tells it.
    fn result(&self, id: &str) -> &str {
        self.results.iter().find(|(r, _)| r == id).map_or(NOT_APPLIED, |(_, text)| text.as_str())
    }
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
    /// The service's id for the proposal, to report what became of it.
    pub id: String,
    pub page: usize,
    pub find: String,
    pub replace: String,
}

/// The tool the model proposes changes with. Its name and wording stay the same for every
/// question, so a cached prefix stays valid.
const EDIT_TOOL: &str = "edit_text";
const EDIT_DESCRIPTION: &str = "Propose a change to the text of the open PDF, keeping its layout: on page `page`, the passage `find` becomes `replace`. \
    Quote `find` exactly as it reads in the document, within one paragraph; the paragraph keeps its font, size, colour and box and is rewrapped. \
    In a table, each cell is a separate paragraph: match one cell at a time (e.g. find \"152,00 €\" in one call, not a whole row). \
    Use one call per passage, and keep a replacement about as long as what it replaces so it fits. \
    The user sees every proposal and decides whether to apply them; use this only when asked to change the document itself.";

/// The tool's parameters, a JSON schema. `minimum` is left out where a service reads only part of
/// JSON Schema (Gemini); proposals are checked here anyway.
fn edit_schema(minimum: bool) -> Value {
    let mut page = json!({"type": "integer", "description": "The page number, from 1."});
    if minimum && let Some(map) = page.as_object_mut() {
        map.insert("minimum".into(), json!(1));
    }
    json!({
        "type": "object",
        "properties": {
            "page": page,
            "find": {"type": "string", "description": "The passage to change, exactly as it reads in the document."},
            "replace": {"type": "string", "description": "The new text for that passage."},
        },
        "required": ["page", "find", "replace"],
    })
}

/// A request: the service, the document, the conversation so far, and the new task.
pub struct Request<'a> {
    pub provider: Provider,
    pub key: &'a str,
    /// The model's id, as the service names it.
    pub model: &'a str,
    /// A compatible service's address, up to its version ("https://openrouter.ai/api/v1"); the
    /// others have their own.
    pub base_url: &'a str,
    /// The document's file name, shown to the model as its title.
    pub title: &'a str,
    pub pdf: &'a [u8],
    /// For a compatible service that doesn't read PDF files: the document's text, page by page,
    /// sent instead of the PDF. `None` sends the PDF; the other services always get the PDF.
    pub text: Option<&'a str>,
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

impl Request<'_> {
    /// The text sent instead of the PDF (a compatible service's, when it has one).
    fn text(&self) -> Option<&str> {
        self.text.filter(|_| self.provider == Provider::Compatible)
    }

    /// The prompts in order: the earlier ones, then the task's.
    fn prompts(&self) -> impl Iterator<Item = String> + '_ {
        self.history.iter().map(|t| t.prompt.clone()).chain(std::iter::once(self.task.prompt()))
    }

    /// The tool is defined: changes may be proposed, or the conversation already used it (every
    /// proposal in its history then has its tool beside it, and a cached prefix stays the same).
    fn tool(&self) -> bool {
        self.edits || self.history.iter().any(|t| !calls(self.provider, &t.answer).is_empty())
    }

    /// The system prompt. It stays the same for every question about a document, so a cached
    /// prefix stays valid.
    fn system(&self) -> String {
        system_prompt(self.language, self.edits, self.text().is_some())
    }
}

/// An answer.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub provider: Provider,
    /// The answer's text.
    pub text: String,
    /// The answer as the service returned it, for the next turn of the conversation.
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
    /// `None` when that model's prices aren't known here (a compatible service's never are). When
    /// another model answered, the attempt that declined isn't counted.
    pub fn cost_usd(&self) -> Option<f64> {
        known(self.provider, &self.model).map(|k| self.usage.cost_usd(k))
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AiError {
    #[error("no API key: add your key for the service first")]
    NoKey,
    #[error("the API key isn't usable: it must be one line of plain characters")]
    BadKey,
    #[error("no model: name the model to ask first")]
    NoModel,
    #[error("the service's address isn't usable: it starts with https:// or http://")]
    BadAddress,
    #[error("the document has no pages to send")]
    Empty,
    #[error("the document has no text to send")]
    NoText,
    #[error("the question is empty")]
    NoQuestion,
    #[error("the model gave no answer")]
    NoAnswer,
    #[error("the document has {pages} pages, more than the {max} the assistant may read")]
    TooManyPages { pages: usize, max: usize },
    #[error("the document is {mb} MB, more than the {max_mb} MB the assistant may read")]
    TooLarge { mb: usize, max_mb: usize },
    #[error("the document's text is {chars} characters, more than the {max} the assistant may send")]
    TooMuchText { chars: usize, max: usize },
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

/// `req`'s checks, made before anything is sent.
fn check(req: &Request) -> Result<(), AiError> {
    let key = req.key.trim();
    if key.is_empty() && req.provider.needs_key() {
        return Err(AiError::NoKey);
    }
    if key.len() > MAX_KEY || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(AiError::BadKey);
    }
    let model = req.model.trim();
    if model.is_empty() || model.len() > MAX_MODEL || !model.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(AiError::NoModel);
    }
    if req.provider == Provider::Compatible {
        compatible::endpoint(req.base_url)?;
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
    match req.text() {
        Some(text) if text.trim().is_empty() => return Err(AiError::NoText),
        Some(text) => {
            let chars = text.chars().count();
            if chars > MAX_TEXT_CHARS {
                return Err(AiError::TooMuchText { chars, max: MAX_TEXT_CHARS });
            }
        }
        None if req.pdf.len() > MAX_PDF_BYTES => {
            return Err(AiError::TooLarge { mb: req.pdf.len().div_ceil(1 << 20), max_mb: MAX_PDF_BYTES >> 20 });
        }
        None => {}
    }
    Ok(())
}

/// The request body for `req`, after its checks.
pub fn request_body(req: &Request) -> Result<Value, AiError> {
    check(req)?;
    Ok(match req.provider {
        Provider::Anthropic => anthropic::body(req),
        Provider::OpenAi => openai::body(req),
        Provider::Gemini => gemini::body(req),
        Provider::Compatible => compatible::body(req),
    })
}

/// The system prompt. `text_only`: the document comes as its text, not as the PDF.
fn system_prompt(language: &str, edits: bool, text_only: bool) -> String {
    let edits = if edits {
        " When the user asks you to change the document itself (correct, reword, shorten or translate its text in place), propose the changes \
         with the edit_text tool, then say in a sentence or two what you changed; the user reviews them and applies them or not. \
         Otherwise don't use the tool."
    } else {
        " The document can't be changed here: if the user asks for changes, say so, and give the new text in your answer instead."
    };
    let document = if text_only {
        "The user has a PDF open and asks about it; its text comes with the first question, page by page, each page after a \"[Page N]\" line \
         (pictures, scans and layout aren't included)."
    } else {
        "The user has the attached PDF open and asks about it."
    };
    format!(
        "You are the assistant built into PdfCraft, a PDF application. {document} \
         Work from what the document says; when it doesn't say something, say so rather than guessing. Refer to pages as (p. N). \
         The answer is read on a phone: write plain text in short paragraphs, with simple \"- \" lists when a list helps, \
         and no tables, headings or other Markdown.{edits} Reply in {language}."
    )
}

/// One call of a tool in an answer.
#[derive(Clone, Debug, PartialEq)]
struct Call {
    id: String,
    name: String,
    args: Value,
}

/// The tool calls in an answer `provider` gave, in order.
fn calls(provider: Provider, answer: &Value) -> Vec<Call> {
    match provider {
        Provider::Anthropic => anthropic::calls(answer),
        Provider::OpenAi => openai::calls(answer),
        Provider::Gemini => gemini::calls(answer),
        Provider::Compatible => compatible::calls(answer),
    }
}

/// The well-formed `edit_text` proposals among `calls`, at most [`MAX_EDITS`]. One that is
/// malformed is left out (its result then says it wasn't applied), and so is the last call of an
/// answer cut off at its length limit: its replacement may stop mid-way.
fn proposals(mut calls: Vec<Call>, truncated: bool) -> Vec<ProposedEdit> {
    if truncated {
        calls.pop();
    }
    calls
        .into_iter()
        .filter(|c| c.name == EDIT_TOOL && !c.id.is_empty())
        .filter_map(|c| {
            let page = usize::try_from(c.args["page"].as_u64()?).ok().filter(|p| *p >= 1)?;
            let find = c.args["find"].as_str().filter(|f| !f.trim().is_empty() && f.chars().count() <= MAX_EDIT_TEXT)?;
            let replace = c.args["replace"].as_str().filter(|r| r.chars().count() <= MAX_EDIT_TEXT)?;
            Some(ProposedEdit { id: c.id, page, find: find.to_owned(), replace: replace.to_owned() })
        })
        .take(MAX_EDITS)
        .collect()
}

/// Arguments a service gave as a JSON string (OpenAI's) or as an object; anything else is none.
fn arguments(v: &Value) -> Value {
    match v {
        Value::String(s) => serde_json::from_str(s).unwrap_or(Value::Null),
        Value::Object(_) => v.clone(),
        _ => Value::Null,
    }
}

/// An error answer's message (a proxy's page isn't JSON: then nothing), at most 400 characters.
fn error_message(v: &Value) -> String {
    let message = v["error"]["message"].as_str().or_else(|| v["error"].as_str()).or_else(|| v["message"].as_str()).unwrap_or_default();
    message.chars().take(400).collect()
}

/// An error status's meaning, whatever the service: what its body adds is read by its own module.
fn error_for(status: u16, message: String) -> AiError {
    match status {
        401 => AiError::InvalidKey,
        402 | 403 => AiError::NotAllowed(message),
        429 => AiError::RateLimited,
        503 | 529 => AiError::Overloaded,
        _ if message.is_empty() => AiError::Api(format!("HTTP {status}")),
        _ => AiError::Api(message),
    }
}

/// `pdf` as base64.
fn base64(pdf: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(pdf)
}

/// A file name for the PDF, from the document's title.
fn file_name(title: &str) -> String {
    let title = title.trim();
    let title = if title.is_empty() { "document" } else { title };
    if title.to_ascii_lowercase().ends_with(".pdf") { title.to_owned() } else { format!("{title}.pdf") }
}

/// The web build: no assistant.
#[cfg(target_arch = "wasm32")]
pub fn send(req: &Request) -> Result<Reply, AiError> {
    request_body(req)?;
    Err(AiError::Network("the assistant isn't available in the web version".into()))
}

/// Send `req` and wait for the answer. A compatible service whose model takes no tools is asked
/// again without the one that proposes changes (Ollama's smaller models): the answer is then
/// words only. The key never shows in an error.
#[cfg(not(target_arch = "wasm32"))]
pub fn send(req: &Request) -> Result<Reply, AiError> {
    let first = send_once(req);
    let without_tools = Request { edits: false, task: req.task.clone(), ..*req };
    let reply = match first {
        Err(AiError::Api(m))
            if req.provider == Provider::Compatible && req.tool() && !without_tools.tool() && m.to_ascii_lowercase().contains("tool") =>
        {
            send_once(&without_tools)
        }
        other => other,
    };
    reply.map_err(|e| hide_key(e, req.key.trim()))
}

/// `e` with `key` taken out of its message (a service may quote what it was sent).
#[cfg(any(test, not(target_arch = "wasm32")))]
fn hide_key(e: AiError, key: &str) -> AiError {
    if key.len() < 8 {
        return e;
    }
    let hide = |m: String| m.replace(key, "…");
    match e {
        AiError::Api(m) => AiError::Api(hide(m)),
        AiError::NotAllowed(m) => AiError::NotAllowed(hide(m)),
        AiError::Refused(m) => AiError::Refused(hide(m)),
        AiError::Network(m) => AiError::Network(hide(m)),
        AiError::Parse(m) => AiError::Parse(hide(m)),
        other => other,
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn send_once(req: &Request) -> Result<Reply, AiError> {
    let body = request_body(req)?.to_string();
    if body.len() > MAX_BODY {
        return Err(AiError::TooLarge { mb: body.len().div_ceil(1 << 20), max_mb: MAX_BODY >> 20 });
    }
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        // Error statuses carry the service's explanation in their body.
        .http_status_as_error(false)
        // Never somewhere else: a redirect would take the key along (or drop the request's body).
        .max_redirects(0)
        .tls_config(ureq::tls::TlsConfig::builder().root_certs(roots()?).build())
        .build()
        .new_agent();
    let key = req.key.trim();
    let request = match req.provider {
        Provider::Anthropic => anthropic::post(&agent, req),
        Provider::OpenAi => agent.post(openai::URL).header("authorization", format!("Bearer {key}")),
        Provider::Gemini => agent.post(gemini::URL).header("x-goog-api-key", key),
        Provider::Compatible => {
            let post = agent.post(compatible::endpoint(req.base_url)?);
            // A service on the device or the local network may take no key.
            if key.is_empty() { post } else { post.header("authorization", format!("Bearer {key}")) }
        }
    };
    let mut response = request.content_type("application/json").send(body.as_bytes()).map_err(|e| AiError::Network(e.to_string()))?;
    let status = response.status().as_u16();
    let text = response.body_mut().with_config().limit(MAX_RESPONSE).read_to_string().map_err(|e| AiError::Network(e.to_string()))?;
    parse_response(req.provider, req.model, status, &text)
}

/// Read `provider`'s answer (`status` and body) to a question to `model`.
pub fn parse_response(provider: Provider, model: &str, status: u16, body: &str) -> Result<Reply, AiError> {
    let reply = match provider {
        Provider::Anthropic => anthropic::parse(status, body),
        Provider::OpenAi => openai::parse(status, body),
        Provider::Gemini => gemini::parse(status, body),
        Provider::Compatible => compatible::parse(status, body),
    }?;
    // An answer with no text and nothing proposed (all of it spent thinking) can't be shown, and
    // mustn't become part of the conversation.
    if reply.text.is_empty() && reply.edits.is_empty() {
        return Err(AiError::NoAnswer);
    }
    // A service that doesn't say which model answered: the one asked.
    let model = if reply.model.is_empty() { model.trim().chars().take(MAX_MODEL).collect() } else { reply.model };
    Ok(Reply { provider, model, ..reply })
}

/// The answer's parts every module reads the same way.
struct Parsed {
    text: String,
    content: Value,
    usage: Usage,
    truncated: bool,
    model: String,
    fell_back: bool,
    calls: Vec<Call>,
}

impl Parsed {
    fn reply(self, provider: Provider) -> Reply {
        let edits = proposals(self.calls, self.truncated);
        Reply {
            provider,
            text: self.text.trim().to_owned(),
            content: self.content,
            usage: self.usage,
            truncated: self.truncated,
            model: self.model.chars().take(MAX_MODEL).collect(),
            fell_back: self.fell_back,
            edits,
        }
    }
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
mod tests;
