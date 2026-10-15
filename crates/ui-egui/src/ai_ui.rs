//! The AI assistant's screen: summarize the open document, ask about it, translate a page, with
//! the service the user chooses and the user's own key (`pdfcraft-ai`): Claude, ChatGPT, Gemini,
//! or any OpenAI-compatible service. Off until the service is set up; each request runs on a
//! worker thread and its answer arrives in [`PdfCraftApp::poll_ai`].

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, vec2};
use pdfcraft_ai::{AiError, ProposedEdit, Provider, Reply, Task, Turn};
use pdfcraft_engine::{DocId, Edit};

use crate::theme::{self, Tokens};
use crate::{PdfCraftApp, icons, widgets};

/// The most pages a question sends unless the user changes it: a cost guard.
pub const DEFAULT_MAX_PAGES: usize = 100;
/// The longest paragraph a proposed change is looked for loosely in (bytes).
const MAX_LOOSE: usize = 1 << 20;

/// What became of a change the assistant proposed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Fate {
    /// Waiting for the user.
    Proposed,
    Applied,
    /// The user discarded it.
    Discarded,
    /// The document has no such page (1-based).
    NoPage(usize),
    /// The passage isn't on that page (1-based).
    NotFound(usize),
    /// The passage reads more than once on that page (1-based): which one is meant isn't known.
    Ambiguous(usize),
    /// It overlaps another change of the same answer.
    Overlaps,
    /// The document refused the change (the edit failed; the reason was shown).
    Refused,
}

impl Fate {
    /// What the model is told about it with the next question (in English: it is for the model).
    fn result(self) -> Option<String> {
        Some(match self {
            Self::Proposed => return None,
            Self::Applied => "Applied.".into(),
            Self::Discarded => "Not applied: the user discarded this change.".into(),
            Self::NoPage(p) => format!("Not applied: the document has no page {p}."),
            Self::NotFound(p) => format!("Not applied: the passage wasn't found on page {p}. Quote it exactly as it reads, within one paragraph."),
            Self::Ambiguous(p) => format!("Not applied: the passage reads more than once on page {p}. Quote enough of it to be unique."),
            Self::Overlaps => "Not applied: it overlaps another change proposed in the same answer.".into(),
            Self::Refused => "Not applied: the document refused the change.".into(),
        })
    }

    /// What the user is shown about it, once decided.
    fn shown(self) -> Option<String> {
        use crate::i18n::fmt;
        Some(match self {
            Self::Proposed => return None,
            Self::Applied => tl!("Applied").to_owned(),
            Self::Discarded => tl!("Discarded").to_owned(),
            Self::NoPage(p) => fmt(tl!("The document has no page {page}."), &[("page", &p.to_string())]),
            Self::NotFound(p) => fmt(tl!("The passage wasn't found on page {page}."), &[("page", &p.to_string())]),
            Self::Ambiguous(p) => fmt(tl!("The passage appears more than once on page {page}."), &[("page", &p.to_string())]),
            Self::Overlaps => tl!("It overlaps another proposed change.").to_owned(),
            Self::Refused => tl!("The document refused the change.").to_owned(),
        })
    }
}

/// One exchange of a conversation.
enum Entry {
    /// A question (as shown) and its answer, with the changes it proposed.
    Answered { shown: String, prompt: String, reply: Reply, model: String, edits: Vec<(ProposedEdit, Fate)> },
    /// A question that got no answer, and why; with what to ask again when asking again may
    /// get one (not when the document refused to be sent).
    Failed { shown: String, error: String, retry: Option<Task> },
    /// A new conversation starts here: the document or the model changed, or the last one grew
    /// long. Earlier answers stay on screen but aren't sent again.
    NewConversation,
}

/// What a conversation was had about: everything that goes into what the model has already
/// read (the service and model, the document, its title, the system prompt's language and tool).
/// The conversation as the model saw it can't change after the fact, so when any of this does —
/// another service or model, an edit, an Undo, Save As, another interface language — a new one
/// starts.
#[derive(Clone, Debug, PartialEq, Eq)]
struct About {
    /// The document's edit generation: it moves with every edit, Undo, Redo and save.
    generation: u64,
    provider: Provider,
    model: String,
    /// A compatible service's address, and whether it gets the PDF or the text.
    base_url: String,
    reads_pdfs: bool,
    title: String,
    language: String,
    edits: bool,
}

/// What was said about one document.
#[derive(Default)]
struct Chat {
    entries: Vec<Entry>,
    /// Where the conversation the next question continues starts in `entries`.
    start: usize,
    about: Option<About>,
}

impl Chat {
    /// The exchanges the next question carries.
    fn history(&self) -> Vec<Turn> {
        self.entries
            .get(self.start..)
            .unwrap_or_default()
            .iter()
            .filter_map(|e| match e {
                Entry::Answered { prompt, reply, edits, .. } => Some(Turn {
                    prompt: prompt.clone(),
                    answer: reply.content.clone(),
                    results: edits.iter().filter_map(|(e, fate)| Some((e.id.clone(), fate.result()?))).collect(),
                }),
                _ => None,
            })
            .collect()
    }

    /// Get ready to ask about `about`: start a new conversation when it changed, or when this
    /// one is long. The proposals still waiting in the conversation the question continues are
    /// discarded: the question tells the model they weren't applied, and what it was told can't
    /// change afterwards.
    fn prepare(&mut self, about: About) {
        if self.about.as_ref() != Some(&about) || self.history().len() >= pdfcraft_ai::MAX_TURNS {
            if !self.history().is_empty() {
                self.entries.push(Entry::NewConversation);
            }
            self.start = self.entries.len();
        }
        self.about = Some(about);
        for entry in self.entries.get_mut(self.start..).unwrap_or_default() {
            if let Entry::Answered { edits, .. } = entry {
                for (_, fate) in edits.iter_mut().filter(|(_, f)| *f == Fate::Proposed) {
                    *fate = Fate::Discarded;
                }
            }
        }
    }
}

/// A question on its way.
struct Pending {
    doc: DocId,
    shown: String,
    /// What was asked, to ask again if no answer comes.
    task: Task,
    prompt: String,
    /// The service and the model asked.
    provider: Provider,
    model: String,
    rx: Receiver<Result<Reply, AiError>>,
}

/// The assistant's settings and conversations.
pub struct AiState {
    /// The service asked.
    pub provider: Provider,
    /// Each service's API key, by [`Provider::index`]. Empty: that service is off and nothing is
    /// ever sent to it (a compatible service may take none).
    pub keys: [String; 4],
    /// Each service's model, by [`Provider::index`].
    pub models: [String; 4],
    /// The compatible service's address, up to its version ("https://openrouter.ai/api/v1").
    pub base_url: String,
    /// The compatible service reads PDF files: it gets the PDF rather than the document's text.
    pub reads_pdfs: bool,
    /// The host the compatible service's key was saved for: it goes nowhere else (another
    /// address filled in later gets no key until one is saved for it).
    pub key_host: String,
    /// The most pages a question may send.
    pub max_pages: usize,
    chats: HashMap<DocId, Chat>,
    pending: Option<Pending>,
    input: String,
    /// The keys being typed in the settings, by [`Provider::index`]: saved with Save.
    key_drafts: [String; 4],
    /// The service chosen in the settings: asked once saved.
    provider_draft: Provider,
    settings_open: bool,
    /// The language pages are translated into (a language's own name, "Français").
    translate_to: String,
}

impl Default for AiState {
    fn default() -> Self {
        Self {
            provider: Provider::default(),
            keys: Default::default(),
            models: Provider::ALL.map(|p| p.default_model().to_owned()),
            base_url: String::new(),
            reads_pdfs: false,
            key_host: String::new(),
            max_pages: DEFAULT_MAX_PAGES,
            chats: HashMap::new(),
            pending: None,
            input: String::new(),
            key_drafts: Default::default(),
            provider_draft: Provider::default(),
            settings_open: false,
            translate_to: String::new(),
        }
    }
}

impl AiState {
    /// The key of the service asked.
    pub fn key(&self) -> &str {
        self.keys.get(self.provider.index()).map_or("", String::as_str)
    }

    /// Set the key of the service asked (one line: a pasted key's spaces and breaks go).
    pub fn set_key(&mut self, key: &str) {
        if let Some(k) = self.keys.get_mut(self.provider.index()) {
            *k = key.split_whitespace().collect();
        }
    }

    /// The model of the service asked.
    pub fn model(&self) -> &str {
        self.models.get(self.provider.index()).map_or("", String::as_str)
    }

    /// The service asked is set up: its key (a compatible service: its address and model), and a
    /// model. Until then the assistant shows its settings, and nothing is sent.
    pub fn ready(&self) -> bool {
        let set_up = match self.provider {
            Provider::Compatible => !self.base_url.trim().is_empty(),
            _ => !self.key().trim().is_empty(),
        };
        set_up && !self.model().trim().is_empty()
    }

    /// The key sent to the service asked: a compatible service's only to the host it was saved
    /// for.
    fn key_to_send(&self) -> &str {
        let host = (self.provider == Provider::Compatible).then(|| pdfcraft_ai::service_host(&self.base_url));
        match host {
            Some(host) if host.as_deref() != Some(self.key_host.as_str()) => "",
            _ => self.key(),
        }
    }

    /// Open the settings: the drafts start from what is saved.
    fn open_settings(&mut self) {
        self.settings_open = true;
        self.key_drafts = self.keys.clone();
        self.provider_draft = self.provider;
    }

    /// Save the settings: the service chosen, and the keys typed (a key pasted from a mail or a
    /// PDF may carry line breaks or spaces: a key has none). A compatible service's key is for
    /// the host it was typed with.
    fn save_settings(&mut self) {
        for (key, draft) in self.keys.iter_mut().zip(&self.key_drafts) {
            *key = draft.split_whitespace().collect();
        }
        self.provider = self.provider_draft;
        let compatible = self.keys.get(Provider::Compatible.index()).is_some_and(|k| !k.is_empty());
        self.key_host = if compatible { pdfcraft_ai::service_host(&self.base_url).unwrap_or_default() } else { String::new() };
        self.settings_open = false;
    }

    /// Whether the settings show (rather than the conversation) because they were opened.
    pub(crate) fn settings_open(&self) -> bool {
        self.settings_open
    }

    /// Close the settings without saving the key being typed (Back).
    pub(crate) fn close_settings(&mut self) {
        self.settings_open = false;
    }

    /// Whether a question is on its way (tests and automation).
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// The answers about `doc` so far, oldest first (tests and automation).
    pub fn answers(&self, doc: DocId) -> Vec<String> {
        self.chats
            .get(&doc)
            .map(|c| c.entries.iter().filter_map(|e| if let Entry::Answered { reply, .. } = e { Some(reply.text.clone()) } else { None }).collect())
            .unwrap_or_default()
    }

    fn fail(&mut self, doc: DocId, shown: String, error: String, retry: Option<Task>) {
        self.chats.entry(doc).or_default().entries.push(Entry::Failed { shown, error, retry });
    }
}

/// The interface language's own name, which answers are written in.
fn reply_language() -> String {
    crate::i18n::current().name().to_owned()
}

/// A service's name, as the interface shows it: its maker's, or a compatible service's host.
fn service_name(provider: Provider, base_url: &str) -> String {
    if provider != Provider::Compatible {
        return provider.name().to_owned();
    }
    match pdfcraft_ai::service_host(base_url) {
        Some(host) => host.chars().take(80).collect(),
        None => tl!("Another service (OpenAI-compatible)").to_owned(),
    }
}

/// What to tell the user when a question to `service` got no answer.
fn error_text(e: &AiError, service: &str) -> String {
    use crate::i18n::fmt;
    match e {
        AiError::NoKey => fmt(tl!("Add your API key for {service} in the assistant's settings."), &[("service", service)]),
        AiError::NoModel => tl!("Choose the model to ask in the assistant's settings.").to_owned(),
        AiError::BadAddress => {
            tl!("The service's address isn't valid: it starts with https:// (http:// only for this device or the local network).").to_owned()
        }
        AiError::NoText => {
            tl!("This document has no text to send (is it a scan?). Choose a service that reads PDF files, or recognize its text first.").to_owned()
        }
        AiError::TooMuchText { chars, max } => fmt(
            tl!("This document's text is too long for the assistant ({chars} characters, at most {max})."),
            &[("chars", &chars.to_string()), ("max", &max.to_string())],
        ),
        AiError::BadKey => tl!("The API key isn't valid: paste it again in the assistant's settings.").to_owned(),
        AiError::InvalidKey => tl!("The API key was refused. Check it in the assistant's settings.").to_owned(),
        AiError::TooManyPages { pages, max } => fmt(
            tl!("This document has {pages} pages; the assistant reads at most {max}. Raise the limit in the assistant's settings."),
            &[("pages", &pages.to_string()), ("max", &max.to_string())],
        ),
        AiError::TooLarge { mb, max_mb } => fmt(
            tl!("This document is too large for the assistant ({mb} MB, at most {max} MB)."),
            &[("mb", &mb.to_string()), ("max", &max_mb.to_string())],
        ),
        AiError::RateLimited | AiError::Overloaded => tl!("Too many requests right now. Try again in a moment.").to_owned(),
        AiError::Refused(reason) => fmt(tl!("The request was declined ({reason})."), &[("reason", reason)]),
        AiError::Empty => tl!("This document has no pages.").to_owned(),
        AiError::NoQuestion => tl!("Ask about this document").to_owned(),
        AiError::NoAnswer => tl!("The assistant gave no answer. Try asking differently.").to_owned(),
        AiError::NotAllowed(m) => {
            fmt(tl!("Your {service} account can't use this model, or has no credit left: {error}"), &[("service", service), ("error", m)])
        }
        AiError::Api(m) | AiError::Parse(m) => fmt(tl!("The AI service answered: {error}"), &[("error", m)]),
        AiError::Network(m) => fmt(tl!("Couldn't reach the AI service: {error}"), &[("error", m)]),
    }
}

impl PdfCraftApp {
    /// Ask the assistant about the active document. The answer arrives in [`Self::poll_ai`].
    pub fn ask_ai(&mut self, task: Task, shown: String) {
        let Some((_, id)) = self.active_ids() else { return };
        if self.ai.pending.is_some() {
            return;
        }
        let Some(doc) = self.session.get(id) else { return };
        // Sending a document to a service copies its content: not one that forbids copying.
        if !doc.allows_copying() {
            let error = tl!("This document doesn't allow copying its content, so it isn't sent to the assistant.").to_owned();
            self.ai.fail(id, shown, error, None);
            return;
        }
        // The working file as it is now, edits included. It stays the same object until the
        // document changes, so later questions send the very same bytes and read them back
        // from the cache; nothing is written for it.
        let pdf = doc.bytes.clone();
        let (provider, model, base_url, reads_pdfs) = (self.ai.provider, self.ai.model().to_owned(), self.ai.base_url.clone(), self.ai.reads_pdfs);
        let (key, max_pages, pages, title, language) =
            (self.ai.key_to_send().to_owned(), self.ai.max_pages, doc.info.pages.len(), doc.display_name(), reply_language());
        // A compatible service that doesn't read PDF files gets the document's text, made on the
        // worker thread (and only for a document within the page limit: the request refuses the
        // others before anything is sent).
        let source =
            (provider == Provider::Compatible && !reads_pdfs && pages <= max_pages.clamp(1, pdfcraft_ai::API_MAX_PAGES)).then(|| doc.export_source());
        // Changes are offered only where the user could make them by hand.
        let edits = doc.allows_modification();
        let chat = self.ai.chats.entry(id).or_default();
        chat.prepare(About {
            generation: doc.edit_generation(),
            provider,
            model: model.clone(),
            base_url: base_url.clone(),
            reads_pdfs,
            title: title.clone(),
            language: language.clone(),
            edits,
        });
        let history = chat.history();
        let prompt = task.prompt();
        let again = task.clone();
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        let asked = model.clone();
        let spawned = std::thread::Builder::new().name("pdfcraft-ai".into()).spawn(move || {
            let text = source.map(|s| document_text(s, pages));
            let request = pdfcraft_ai::Request {
                provider,
                key: &key,
                model: &model,
                base_url: &base_url,
                title: &title,
                pdf: &pdf,
                text: text.as_deref(),
                pages,
                max_pages,
                language: &language,
                history: &history,
                task,
                edits,
            };
            // Nobody may be waiting any more (the app closed): nothing to do about it.
            let _ = tx.send(pdfcraft_ai::send(&request));
            if let Some(ctx) = ctx {
                ctx.request_repaint();
            }
        });
        match spawned {
            Ok(_) => self.ai.pending = Some(Pending { doc: id, shown, task: again, prompt, provider, model: asked, rx }),
            Err(e) => self.ai.fail(id, shown, e.to_string(), Some(again)),
        }
    }

    /// Stop waiting for the question on its way: its answer, if one still comes, is dropped. The
    /// request itself can't be called back, so the service may still finish it.
    pub fn cancel_ai(&mut self) {
        if let Some(Pending { doc, shown, task, .. }) = self.ai.pending.take() {
            self.ai.fail(doc, shown, tl!("Cancelled").to_owned(), Some(task));
        }
    }

    /// Ask again the question of failed answer `entry` of `doc`'s conversation (which it replaces).
    fn retry_ai(&mut self, doc: DocId, entry: usize) {
        if self.ai.pending.is_some() || self.active_ids().map(|(_, id)| id) != Some(doc) {
            return;
        }
        let Some(chat) = self.ai.chats.get_mut(&doc) else { return };
        let Some(Entry::Failed { retry: Some(_), .. }) = chat.entries.get(entry) else { return };
        let Entry::Failed { shown, retry: Some(task), .. } = chat.entries.remove(entry) else { return };
        self.ask_ai(task, shown);
    }

    /// Take an answer that arrived (called every frame).
    pub(crate) fn poll_ai(&mut self) {
        // A conversation goes with its document.
        if !self.ai.chats.is_empty() {
            let session = &self.session;
            self.ai.chats.retain(|id, _| session.get(*id).is_some());
        }
        let Some(pending) = self.ai.pending.as_ref() else { return };
        let result = match pending.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            // The worker stopped without an answer (it can't panic, but a lost answer is still an answer).
            Err(TryRecvError::Disconnected) => Err(AiError::Network("no answer".into())),
        };
        let Some(Pending { doc, shown, task, prompt, provider, model, .. }) = self.ai.pending.take() else { return };
        if self.session.get(doc).is_none() {
            // Closed while the question was on its way.
            return;
        }
        let entry = match result {
            Ok(reply) => {
                let edits = reply.edits.iter().map(|e| (e.clone(), Fate::Proposed)).collect();
                Entry::Answered { shown, prompt, reply, model, edits }
            }
            Err(e) => Entry::Failed { shown, error: error_text(&e, &service_name(provider, &self.ai.base_url)), retry: Some(task) },
        };
        self.ai.chats.entry(doc).or_default().entries.push(entry);
    }

    /// Apply the changes answer `entry` of `doc`'s conversation proposed (those still proposed),
    /// as one step that Undo takes back whole. Each one that can't be applied says why.
    fn apply_ai_edits(&mut self, doc: DocId, entry: usize) {
        // Edits go to the active document: the one the assistant's screen is about.
        if self.active_ids().map(|(_, id)| id) != Some(doc) {
            return;
        }
        let Some(Entry::Answered { edits, .. }) = self.ai.chats.get(&doc).and_then(|c| c.entries.get(entry)) else { return };
        let proposed: Vec<(usize, ProposedEdit)> =
            edits.iter().enumerate().filter(|(_, (_, fate))| *fate == Fate::Proposed).map(|(i, (e, _))| (i, e.clone())).collect();
        let Some(document) = self.session.get(doc) else { return };
        let (batch, fates) = text_changes(document, &proposed);
        let applied = !batch.is_empty() && self.apply_edit(Edit::Batch { label: "Edit text".into(), edits: batch });
        let Some(Entry::Answered { edits, .. }) = self.ai.chats.get_mut(&doc).and_then(|c| c.entries.get_mut(entry)) else { return };
        for (i, fate) in fates {
            let fate = match fate {
                Fate::Applied if !applied => Fate::Refused,
                fate => fate,
            };
            if let Some(slot) = edits.get_mut(i) {
                slot.1 = fate;
            }
        }
    }

    /// Discard the changes answer `entry` of `doc`'s conversation still proposes.
    fn discard_ai_edits(&mut self, doc: DocId, entry: usize) {
        if let Some(Entry::Answered { edits, .. }) = self.ai.chats.get_mut(&doc).and_then(|c| c.entries.get_mut(entry)) {
            for (_, fate) in edits.iter_mut().filter(|(_, f)| *f == Fate::Proposed) {
                *fate = Fate::Discarded;
            }
        }
    }
}

/// The edits that make `proposed` (each with its place in the answer's list) in `doc`, and what
/// becomes of each. A passage must read exactly once on its page, and not overlap another change;
/// every passage found becomes part of its paragraph's rewrite (in the paragraph's own font, size
/// and colour, rewrapped in its box), one rewrite per paragraph.
/// The document's text for a service that doesn't read PDF files: its first `pages` pages, each
/// after a "[Page N]" line, as the system prompt says. Empty when no page has any text (a scan),
/// which the request refuses with the way out; it stops once past what a request may carry.
fn document_text(source: pdfcraft_engine::export::ExportSource, pages: usize) -> String {
    let mut exporter = pdfcraft_engine::export::Exporter::from_source(source);
    let (mut out, mut any) = (String::new(), false);
    for page in 0..pages {
        let text = exporter.text(page).unwrap_or_default();
        let text = text.trim();
        any |= !text.is_empty();
        out.push_str(&format!("[Page {}]\n{text}\n\n", page.saturating_add(1)));
        // Bytes are at least characters: past this, the request refuses it anyway.
        if out.len() > pdfcraft_ai::MAX_TEXT_CHARS.saturating_mul(4) {
            break;
        }
    }
    if any { out } else { String::new() }
}

fn text_changes(doc: &pdfcraft_engine::Document, proposed: &[(usize, ProposedEdit)]) -> (Vec<Edit>, Vec<(usize, Fate)>) {
    use std::collections::BTreeMap;
    let pages = doc.info.pages.len();
    let mut blocks: HashMap<usize, Vec<pdfcraft_engine::TextBlock>> = HashMap::new();
    // The passages found in a paragraph's text (byte ranges), and their replacements.
    type Places = Vec<(std::ops::Range<usize>, String)>;
    // (page, paragraph) → its places.
    let mut found: BTreeMap<(usize, usize), Places> = BTreeMap::new();
    let mut fates = Vec::new();
    for (i, e) in proposed {
        let Some(page) = e.page.checked_sub(1).filter(|p| *p < pages) else {
            fates.push((*i, Fate::NoPage(e.page)));
            continue;
        };
        let on_page = blocks.entry(page).or_insert_with(|| doc.text_blocks(page));
        // Where the passage reads on the page: two places are enough to know it's ambiguous.
        let mut places =
            on_page.iter().enumerate().flat_map(|(b, block)| find_loosely(&block.editor_text, &e.find, 2).into_iter().map(move |r| (b, r)));
        let fate = match (places.next(), places.next()) {
            (None, _) => Fate::NotFound(e.page),
            (Some(_), Some(_)) => Fate::Ambiguous(e.page),
            (Some((b, range)), None) => {
                let taken = found.entry((page, b)).or_default();
                if taken.iter().any(|(r, _)| r.start < range.end && range.start < r.end) {
                    Fate::Overlaps
                } else {
                    taken.push((range, e.replace.clone()));
                    Fate::Applied
                }
            }
        };
        fates.push((*i, fate));
    }
    // Later paragraphs first on each page: should a rewrite ever change where its paragraph
    // falls in the page's order, the paragraphs before it still keep their numbers.
    let edits = found
        .into_iter()
        .rev()
        .filter_map(|((page, block), mut places)| {
            let mut text = blocks.get(&page)?.get(block)?.editor_text.clone();
            // From the end, so each range still points where it did in the original text.
            places.sort_by_key(|(r, _)| std::cmp::Reverse(r.start));
            for (range, replace) in places {
                // `find_loosely` gives character boundaries; checked anyway, as `replace_range` would panic.
                if range.start <= range.end && text.is_char_boundary(range.start) && text.is_char_boundary(range.end) {
                    text.replace_range(range, &replace);
                }
            }
            Some(Edit::EditTextBlock { page, block, text, style: pdfcraft_engine::BlockStyle::default() })
        })
        .collect();
    (edits, fates)
}

/// Where `needle` reads in `hay` (at most `limit` places, none overlapping), as byte ranges of
/// `hay`: whitespace runs count as one space (a paragraph's line breaks read as spaces), and
/// curly quotes and dashes as their plain forms.
fn find_loosely(hay: &str, needle: &str, limit: usize) -> Vec<std::ops::Range<usize>> {
    if needle.trim().is_empty() || limit == 0 {
        return Vec::new();
    }
    // The loose search keeps a position for every byte: only for a paragraph's worth of text.
    if hay.len() > MAX_LOOSE {
        return hay.match_indices(needle).take(limit).filter_map(|(at, m)| Some(at..at.checked_add(m.len())?)).collect();
    }
    let fold = |c: char| match c {
        '\u{2018}' | '\u{2019}' | '\u{02BC}' => '\'',
        '\u{201C}' | '\u{201D}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2013}' | '\u{2014}' => '-',
        c => c,
    };
    // The folded text, and for each of its bytes where its character starts in `hay`.
    let mut folded = String::with_capacity(hay.len());
    let mut origin: Vec<usize> = Vec::with_capacity(hay.len() + 1);
    let mut space = false;
    for (at, c) in hay.char_indices() {
        let c = if c.is_whitespace() { ' ' } else { fold(c) };
        if c == ' ' && space {
            continue;
        }
        space = c == ' ';
        let before = folded.len();
        folded.push(c);
        origin.extend(std::iter::repeat_n(at, folded.len() - before));
    }
    origin.push(hay.len());
    let mut wanted = String::with_capacity(needle.len());
    let mut space = false;
    for c in needle.trim().chars() {
        let c = if c.is_whitespace() { ' ' } else { fold(c) };
        if c == ' ' && space {
            continue;
        }
        space = c == ' ';
        wanted.push(c);
    }
    folded.match_indices(wanted.as_str()).take(limit).filter_map(|(at, m)| Some(*origin.get(at)?..*origin.get(at.checked_add(m.len())?)?)).collect()
}
/// The assistant's screen in the phone layout, over the reader: `screen` is the content area.
/// Returns whether its Back was tapped.
pub(crate) fn page(app: &mut PdfCraftApp, ui: &mut egui::Ui, screen: Rect) -> bool {
    let t = Tokens::get(ui.ctx());
    let Some((_, doc)) = app.active_ids() else { return true };
    // Not set up: the settings, which stay until Save or Back (a compatible service's address
    // and model apply as they are typed, and the form mustn't close under the finger).
    if !app.ai.ready() && !app.ai.settings_open {
        app.ai.open_settings();
    }
    let settings = app.ai.settings_open;
    let mut back = false;
    let mut toggle_settings = false;
    let mut clear = false;
    let top = Rect::from_min_size(screen.min, vec2(screen.width(), crate::phone::TOP_BAR));
    // Nothing to clear while a question about this document is on its way.
    let clearable = app.ai.chats.get(&doc).is_some_and(|c| !c.entries.is_empty()) && app.ai.pending.as_ref().is_none_or(|p| p.doc != doc);
    crate::phone::bar(ui, "phone-ai-bar", top, &t, true, |ui| {
        back = icons::button(ui, "chevron-left", crate::phone::TARGET, false, tl!("Back")).clicked();
        let buttons = if clearable && !settings { 2.0 } else { 1.0 };
        let width = ui.available_width() - buttons * crate::phone::TARGET;
        crate::phone::title_text(ui, tl!("AI assistant"), width, &t);
        if clearable && !settings {
            clear = icons::button(ui, "trash-2", crate::phone::TARGET, false, tl!("Clear")).clicked();
        }
        toggle_settings = icons::button(ui, "settings", crate::phone::TARGET, settings, tl!("Settings")).clicked();
    });
    if clear {
        app.ai.chats.remove(&doc);
    }
    if toggle_settings {
        if app.ai.settings_open {
            app.ai.settings_open = false;
        } else {
            app.ai.open_settings();
        }
    }
    let rest = Rect::from_min_max(egui::pos2(screen.left(), top.bottom()), screen.max);
    ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| {
        ui.set_clip_rect(rest);
        egui::Frame::NONE.fill(t.pasteboard).show(ui, |ui| {
            ui.set_min_size(rest.size());
            if settings {
                settings_form(app, ui, &t);
            } else {
                chat(app, doc, ui, &t);
            }
        });
    });
    back
}

/// The assistant's settings as a screen of their own (from Home: no document needed). Returns
/// whether it should close: Back, or Save.
pub(crate) fn settings_page(app: &mut PdfCraftApp, ui: &mut egui::Ui, screen: Rect) -> bool {
    let t = Tokens::get(ui.ctx());
    let back = crate::phone::top_bar_titled(ui, screen, tl!("AI assistant"));
    let rest = Rect::from_min_max(egui::pos2(screen.left(), screen.top() + crate::phone::TOP_BAR), screen.max);
    if !app.ai.settings_open {
        app.ai.open_settings();
    }
    ui.scope_builder(egui::UiBuilder::new().max_rect(rest), |ui| {
        ui.set_clip_rect(rest);
        egui::Frame::NONE.fill(t.pasteboard).show(ui, |ui| {
            ui.set_min_size(rest.size());
            settings_form(app, ui, &t);
        });
    });
    // Save closes the settings.
    let saved = !app.ai.settings_open;
    if back || saved {
        app.ai.settings_open = false;
    }
    back || saved
}

fn settings_form(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    let heading = |ui: &mut egui::Ui, text: &str| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(text).font(theme::semibold(14.0)));
    };
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(
                egui::RichText::new(tl!(
                    "Your question and the open document are sent to the service you choose, with your own API key, only when you ask. Requests are billed to your account with that service."
                ))
                .color(t.text_muted),
            );
            heading(ui, tl!("Service"));
            for p in Provider::ALL {
                let name = if p == Provider::Compatible { tl!("Another service (OpenAI-compatible)") } else { p.name() };
                widgets::radio_value(ui, &mut app.ai.provider_draft, p, name);
            }
            let provider = app.ai.provider_draft;
            let index = provider.index();
            if provider == Provider::Compatible {
                compatible_settings(app, ui, t);
            } else {
                heading(ui, tl!("API key"));
                let hint = match provider {
                    Provider::OpenAi => "sk-…",
                    Provider::Gemini => "AIza…",
                    _ => "sk-ant-…",
                };
                if let Some(draft) = app.ai.key_drafts.get_mut(index) {
                    ui.add(widgets::line(draft).password(true).hint_text(hint).desired_width(f32::INFINITY).char_limit(512));
                }
                heading(ui, tl!("Model"));
                ui.label(egui::RichText::new(tl!("Price per million tokens read / written")).small().color(t.text_muted));
                for m in provider.models() {
                    if let Some(model) = app.ai.models.get_mut(index)
                        && ui.add(widgets::radio(model.as_str() == m.id, format!("{}  ·  {}", m.name, price_line(m)))).clicked()
                    {
                        *model = m.id.to_owned();
                    }
                }
                if provider == Provider::Gemini {
                    ui.label(
                        egui::RichText::new(tl!("On Google's free tier, what you send may be used to improve Google's products.")).small().color(t.text_muted),
                    );
                }
            }
            heading(ui, tl!("Most pages sent per question"));
            ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(80.0);
            ui.add(egui::Slider::new(&mut app.ai.max_pages, 1..=pdfcraft_ai::API_MAX_PAGES).logarithmic(true));
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if pill(ui, tl!("Save"), true, t).clicked() {
                    app.ai.save_settings();
                }
                let saved = app.ai.keys.get(index).is_some_and(|k| !k.is_empty());
                if saved && pill(ui, tl!("Remove"), false, t).clicked() {
                    for keys in [&mut app.ai.keys, &mut app.ai.key_drafts] {
                        if let Some(k) = keys.get_mut(index) {
                            k.clear();
                        }
                    }
                }
            });
        });
    });
}

/// A compatible service's settings: a service to fill in for, its address, the model it names,
/// its key if it takes one, and whether it reads PDF files.
fn compatible_settings(app: &mut PdfCraftApp, ui: &mut egui::Ui, t: &Tokens) {
    // (name, address, a model it names, or none to name)
    const SERVICES: [(&str, &str, &str); 5] = [
        ("OpenRouter", "https://openrouter.ai/api/v1", ""),
        ("Mistral", "https://api.mistral.ai/v1", "mistral-small-latest"),
        ("DeepSeek", "https://api.deepseek.com", "deepseek-flash"),
        ("Groq", "https://api.groq.com/openai/v1", ""),
        ("Ollama", "http://localhost:11434/v1", ""),
    ];
    let index = Provider::Compatible.index();
    ui.add_space(4.0);
    ui.label(egui::RichText::new(tl!("Fill in for")).small().color(t.text_muted));
    ui.horizontal_wrapped(|ui| {
        for (name, address, model) in SERVICES {
            if pill(ui, name, false, t).clicked() {
                app.ai.base_url = address.to_owned();
                if let Some(m) = app.ai.models.get_mut(index) {
                    *m = model.to_owned();
                }
                // Another service: the key typed for the last one isn't its.
                if let Some(draft) = app.ai.key_drafts.get_mut(index) {
                    draft.clear();
                }
            }
        }
    });
    let field = |ui: &mut egui::Ui, title: &str, value: &mut String, hint: &str, password: bool, limit: usize| {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(title).font(theme::semibold(14.0)));
        ui.add(widgets::line(value).password(password).hint_text(hint).desired_width(f32::INFINITY).char_limit(limit));
    };
    field(ui, tl!("Service address"), &mut app.ai.base_url, "https://…/v1", false, 2048);
    if let Some(model) = app.ai.models.get_mut(index) {
        field(ui, tl!("Model"), model, "", false, 200);
    }
    if let Some(draft) = app.ai.key_drafts.get_mut(index) {
        field(ui, tl!("API key (if the service asks for one)"), draft, "", true, 512);
    }
    ui.add_space(4.0);
    ui.add(widgets::checkbox(&mut app.ai.reads_pdfs, tl!("The service reads PDF files")));
    ui.label(
        egui::RichText::new(tl!("Otherwise the document's text is sent, page by page (not its pictures or scans).")).small().color(t.text_muted),
    );
}

/// "4 $ / 20 $" for a million tokens read / written, as the radio buttons show it.
fn price_line(m: &pdfcraft_ai::Known) -> String {
    let (input, output) = m.list_prices();
    format!("{} $ / {} $", trim_price(input), trim_price(output))
}

fn trim_price(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// A rounded button a finger tall.
fn pill(ui: &mut egui::Ui, label: &str, primary: bool, t: &Tokens) -> egui::Response {
    let font = theme::semibold(14.5);
    let w = ui.fonts_mut(|f| f.layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE).size().x) + 36.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, crate::phone::TARGET), Sense::click());
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label));
    let press = theme::Press::track(ui, &resp);
    let body = Rect::from_center_size(rect.center(), vec2(w, 40.0)).translate(press.offset());
    let (fill, stroke, text) = if primary {
        (press.fill(t.accent, t.accent.gamma_multiply(0.92), t.accent.gamma_multiply(0.82)), Stroke::NONE, Color32::WHITE)
    } else {
        (press.fill(t.card, t.hover, t.pressed), Stroke::new(1.0, t.border), t.text)
    };
    ui.painter().rect(body, CornerRadius::same(20), fill, stroke, egui::StrokeKind::Inside);
    ui.painter().text(body.center(), Align2::CENTER_CENTER, label, font, text);
    theme::hand(resp)
}

fn chat(app: &mut PdfCraftApp, doc: DocId, ui: &mut egui::Ui, t: &Tokens) {
    let busy = app.ai.pending.is_some();
    let page = app.active.and_then(|i| app.views.get(i)).map_or(1, |v| v.current.saturating_add(1));
    if app.ai.translate_to.is_empty() {
        app.ai.translate_to = reply_language();
    }
    let mut ask: Option<(Task, String)> = None;
    // An answer's proposed changes applied (true) or discarded (false).
    let mut decide: Option<(usize, bool)> = None;
    // A failed question asked again, or the one on its way cancelled.
    let mut retry: Option<usize> = None;
    let mut cancel = false;
    // The question box at the bottom, the conversation above it.
    let input_h = 64.0;
    let full = ui.max_rect();
    let list = Rect::from_min_max(full.min, egui::pos2(full.right(), full.bottom() - input_h));
    ui.scope_builder(egui::UiBuilder::new().max_rect(list), |ui| {
        egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
            egui::Frame::NONE.inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 10.0;
                // Quick actions.
                ui.horizontal_wrapped(|ui| {
                    ui.add_enabled_ui(!busy, |ui| {
                        if pill(ui, tl!("Summarize"), false, t).clicked() {
                            ask = Some((Task::Summarize, tl!("Summarize").to_owned()));
                        }
                        let label = crate::i18n::fmt(tl!("Translate page {n}"), &[("n", &page.to_string())]);
                        if pill(ui, &label, false, t).clicked() {
                            let shown = format!("{label} → {}", app.ai.translate_to);
                            ask = Some((Task::Translate { page, language: app.ai.translate_to.clone() }, shown));
                        }
                    });
                    crate::widgets::combo("ai-translate-to").selected_text(app.ai.translate_to.clone()).show_ui(ui, |ui| {
                        for lang in crate::i18n::Lang::all() {
                            let name = lang.name().to_owned();
                            if ui.selectable_label(app.ai.translate_to == name, &name).clicked() {
                                app.ai.translate_to = name;
                                ui.close();
                            }
                        }
                    });
                });
                if let Some(chat) = app.ai.chats.get(&doc) {
                    for (index, entry) in chat.entries.iter().enumerate() {
                        match entry {
                            Entry::Answered { shown, reply, model, edits, .. } => {
                                bubble(ui, shown, t);
                                answer(ui, &reply.text, t, Some((reply, model.as_str())));
                                if let Some(apply) = proposals(ui, edits, t) {
                                    decide = Some((index, apply));
                                }
                            }
                            Entry::Failed { shown, error, retry: again } => {
                                bubble(ui, shown, t);
                                answer(ui, error, t, None);
                                if again.is_some() && ui.add_enabled_ui(!busy, |ui| pill(ui, tl!("Try again"), false, t)).inner.clicked() {
                                    retry = Some(index);
                                }
                            }
                            Entry::NewConversation => {
                                ui.vertical_centered(|ui| ui.label(egui::RichText::new(tl!("New conversation")).small().color(t.text_faint)));
                            }
                        }
                    }
                }
                if let Some(p) = app.ai.pending.as_ref().filter(|p| p.doc == doc) {
                    bubble(ui, &p.shown, t);
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(18.0));
                        ui.label(egui::RichText::new(tl!("Thinking…")).color(t.text_muted));
                        ui.add_space(8.0);
                        cancel = pill(ui, tl!("Cancel"), false, t).clicked();
                    });
                }
            });
        });
    });
    // The question box.
    let bar = Rect::from_min_max(egui::pos2(full.left(), full.bottom() - input_h), full.max);
    ui.painter().rect_filled(bar, CornerRadius::ZERO, t.chrome);
    ui.painter().hline(bar.x_range(), bar.top(), Stroke::new(1.0, t.divider));
    ui.scope_builder(egui::UiBuilder::new().max_rect(bar.shrink2(vec2(10.0, 8.0))).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
        let send_w = crate::phone::TARGET;
        let edit = egui::TextEdit::singleline(&mut app.ai.input)
            .id(egui::Id::new("ai-question"))
            .hint_text(tl!("Ask about this document"))
            .font(theme::regular(15.5))
            .desired_width(ui.available_width() - send_w - 8.0)
            .char_limit(4000);
        let r = widgets::named_edit(ui, edit, tl!("Ask about this document"));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(8.0);
        let ready = !busy && !app.ai.input.trim().is_empty();
        // Lit when there is a question to send; a button, not a switch, to a screen reader.
        let send = ui.add_enabled_ui(ready, |ui| icons::button(ui, "send", send_w, ready, tl!("Send"))).inner;
        ui.ctx().accesskit_node_builder(send.id, |node| node.clear_toggled());
        let send = send.clicked();
        if (send || enter) && ready {
            let question = app.ai.input.trim().to_owned();
            app.ai.input.clear();
            ask = Some((Task::Ask(question.clone()), question));
        }
    });
    match decide {
        Some((entry, true)) => app.apply_ai_edits(doc, entry),
        Some((entry, false)) => app.discard_ai_edits(doc, entry),
        None => {}
    }
    if cancel {
        app.cancel_ai();
    }
    if let Some(entry) = retry {
        app.retry_ai(doc, entry);
    }
    if let Some((task, shown)) = ask {
        app.ask_ai(task, shown);
    }
}

/// The changes an answer proposes: each passage struck through above what replaces it, and once
/// decided, what became of it. While any waits, Apply and Discard: returns `Some(true)` for
/// Apply, `Some(false)` for Discard.
fn proposals(ui: &mut egui::Ui, edits: &[(ProposedEdit, Fate)], t: &Tokens) -> Option<bool> {
    /// The most of a passage shown (the whole of it is applied).
    const SHOWN: usize = 240;
    let short = |s: &str| {
        let mut out: String = s.trim().chars().take(SHOWN).collect();
        if s.trim().chars().nth(SHOWN).is_some() {
            out.push('…');
        }
        out
    };
    if edits.is_empty() {
        return None;
    }
    let mut decision = None;
    egui::Frame::NONE.fill(t.accent_soft).corner_radius(CornerRadius::same(14)).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0;
        let title = format!("{} ({})", tl!("Proposed changes"), edits.len());
        ui.label(egui::RichText::new(title).font(theme::semibold(14.5)).color(t.accent_text));
        for (e, fate) in edits {
            ui.add_space(4.0);
            let page = crate::i18n::fmt(tl!("Page {label}"), &[("label", &e.page.to_string())]);
            ui.label(egui::RichText::new(page).small().color(t.text_muted));
            ui.add(egui::Label::new(egui::RichText::new(short(&e.find)).strikethrough().color(t.text_muted)).wrap());
            ui.add(egui::Label::new(egui::RichText::new(short(&e.replace)).color(t.text)).wrap());
            if let Some(shown) = fate.shown() {
                let color = if *fate == Fate::Applied { t.accent_text } else { t.text_faint };
                ui.label(egui::RichText::new(shown).small().color(color));
            }
        }
        if edits.iter().any(|(_, f)| *f == Fate::Proposed) {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if pill(ui, tl!("Apply"), true, t).clicked() {
                    decision = Some(true);
                }
                if pill(ui, tl!("Discard"), false, t).clicked() {
                    decision = Some(false);
                }
            });
            ui.label(egui::RichText::new(tl!("Applied as one step: Undo takes them all back.")).small().color(t.text_muted));
        }
    });
    decision
}

/// The user's question, on the right.
fn bubble(ui: &mut egui::Ui, text: &str, t: &Tokens) {
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        ui.set_max_width(ui.available_width() * 0.85);
        egui::Frame::NONE.fill(t.accent_soft).corner_radius(CornerRadius::same(14)).inner_margin(egui::Margin::symmetric(12, 8)).show(ui, |ui| {
            ui.add(egui::Label::new(egui::RichText::new(text).color(t.text)).wrap());
        });
    });
}

/// "≈ 0.012 $ · 3456 tokens", priced as the model that answered, and named when it isn't the
/// one asked (a declined request answered by another model).
fn cost_line(reply: &Reply, asked: &str) -> String {
    use crate::i18n::fmt;
    let tokens = reply.usage.total().to_string();
    let mut line = match reply.cost_usd() {
        Some(cost) => fmt(tl!("≈ {cost} $ · {tokens} tokens"), &[("cost", &format!("{cost:.3}")), ("tokens", &tokens)]),
        None => fmt(tl!("{tokens} tokens"), &[("tokens", &tokens)]),
    };
    // A dated id ("…-20261001") is still the model asked; a compatible service may name its
    // model its own way.
    let other = reply.provider != Provider::Compatible && !reply.model.starts_with(asked.trim());
    if (reply.fell_back || other) && !reply.model.is_empty() {
        line.push_str(" · ");
        line.push_str(pdfcraft_ai::known(reply.provider, &reply.model).map_or(reply.model.as_str(), |m| m.name));
    }
    line
}

/// An answer (or why there is none), with what it cost and a Copy button.
fn answer(ui: &mut egui::Ui, text: &str, t: &Tokens, reply: Option<(&Reply, &str)>) {
    egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.divider))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Why there is no answer, in the red that keeps its contrast on either theme.
            let color = if reply.is_some() { t.text } else { t.error };
            // An answer may be nothing but proposed changes, shown below it.
            if !text.is_empty() {
                ui.add(egui::Label::new(egui::RichText::new(text).font(theme::regular(15.0)).color(color)).wrap().selectable(true));
            }
            let Some((reply, model)) = reply else { return };
            if reply.truncated {
                ui.label(egui::RichText::new(tl!("The answer stopped at its length limit.")).small().color(t.text_muted));
            }
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(cost_line(reply, model)).small().color(t.text_faint));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if icons::button(ui, "copy", if widgets::phone_dialogs(ui.ctx()) { crate::phone::TARGET } else { 36.0 }, false, tl!("Copy"))
                        .clicked()
                    {
                        ui.ctx().copy_text(reply.text.clone());
                    }
                });
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passages_are_found_however_their_lines_break() {
        let first = |hay: &str, needle: &str| find_loosely(hay, needle, 1).first().cloned();
        assert_eq!(first("The quick\nbrown fox", "quick brown"), Some(4..15));
        assert_eq!(first("The quick brown fox", "quick brown"), Some(4..15));
        // A curly apostrophe (3 bytes) and a double space, asked for plainly.
        assert_eq!(first("It\u{2019}s  here.", "It's here"), Some(0..12));
        assert_eq!(first("abc", ""), None);
        assert_eq!(first("abc", " \n "), None);
        assert_eq!(first("abc", "abd"), None);
        assert_eq!(first("a\u{e9}b c", "\u{e9}b  c"), Some(1..6), "byte ranges on character boundaries");
        assert_eq!(find_loosely("a cat, a cat", "cat", 5), vec![2..5, 9..12]);
        assert_eq!(find_loosely("a cat, a cat", "cat", 1), vec![2..5], "no more than asked for");
    }

    /// One page with three paragraphs, far enough apart to be three.
    fn three_paragraphs() -> Vec<u8> {
        let content = "BT /F1 12 Tf 50 700 Td (First paragraph here.) Tj ET\n\
                       BT /F1 12 Tf 50 600 Td (Second paragraph, teh typo.) Tj ET\n\
                       BT /F1 12 Tf 50 500 Td (Third one, recieve it.) Tj ET\n";
        format!(
            "%PDF-1.7\n1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
             2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj\n\
             3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >> endobj\n\
             4 0 obj << /Length {} >> stream\n{content}endstream endobj\n\
             5 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj\n\
             trailer << /Root 1 0 R >>\n%%EOF",
            content.len()
        )
        .into_bytes()
    }

    #[test]
    fn proposed_changes_rewrite_their_paragraphs_in_one_step() {
        let mut session = pdfcraft_engine::Session::new();
        let id = session.open("three.pdf", None, std::sync::Arc::new(three_paragraphs()), None).unwrap();
        let doc = session.get(id).unwrap();
        assert_eq!(doc.text_blocks(0).len(), 3, "three paragraphs");
        let edit =
            |id: &str, page: usize, find: &str, replace: &str| ProposedEdit { id: id.into(), page, find: find.into(), replace: replace.into() };
        let proposed = vec![
            (0, edit("a", 1, "teh typo", "the typo")),
            (1, edit("b", 1, "recieve", "receive")),
            (2, edit("c", 1, "not in the document", "x")),
            (3, edit("d", 2, "First", "1st")),
            (4, edit("e", 1, "First paragraph", "Opening paragraph")),
            // "paragraph" is in two paragraphs: which one is meant isn't known.
            (5, edit("f", 1, "paragraph", "section")),
            // Inside a passage another change already takes.
            (6, edit("g", 1, "teh", "the")),
        ];
        let (batch, fates) = text_changes(doc, &proposed);
        assert_eq!(
            fates,
            vec![
                (0, Fate::Applied),
                (1, Fate::Applied),
                (2, Fate::NotFound(1)),
                (3, Fate::NoPage(2)),
                (4, Fate::Applied),
                (5, Fate::Ambiguous(1)),
                (6, Fate::Overlaps)
            ]
        );
        assert_eq!(batch.len(), 3, "one rewrite per paragraph");
        session.apply(id, Edit::Batch { label: "Edit text".into(), edits: batch }).unwrap();
        let texts: Vec<String> = session.get(id).unwrap().text_blocks(0).into_iter().map(|b| b.text).collect();
        for want in ["Opening paragraph here.", "Second paragraph, the typo.", "Third one, receive it."] {
            assert!(texts.iter().any(|t| t == want), "{want:?} in {texts:?}");
        }
        assert_eq!(texts.len(), 3, "and nothing else: {texts:?}");
        // The page's text is the same three paragraphs, so one Undo takes them all back.
        session.undo(id).unwrap();
        let back: Vec<String> = session.get(id).unwrap().text_blocks(0).into_iter().map(|b| b.text).collect();
        assert!(back.iter().any(|t| t == "Second paragraph, teh typo."), "{back:?}");
    }

    /// An answer proposing two changes, one of them for a passage that isn't there.
    fn answer_with_changes() -> Entry {
        let edits = vec![
            ProposedEdit { id: "t1".into(), page: 1, find: "teh typo".into(), replace: "the typo".into() },
            ProposedEdit { id: "t2".into(), page: 1, find: "nowhere".into(), replace: "x".into() },
        ];
        let reply = Reply {
            provider: Provider::Anthropic,
            text: "One typo.".into(),
            content: serde_json::json!([]),
            usage: pdfcraft_ai::Usage::default(),
            truncated: false,
            model: "claude-opus-5-5".into(),
            fell_back: false,
            edits: edits.clone(),
        };
        Entry::Answered {
            shown: "Fix it".into(),
            prompt: "Fix it".into(),
            reply,
            model: "claude-opus-5-5".into(),
            edits: edits.into_iter().map(|e| (e, Fate::Proposed)).collect(),
        }
    }

    fn fates(app: &PdfCraftApp, doc: DocId) -> Vec<Fate> {
        match app.ai.chats.get(&doc).and_then(|c| c.entries.first()) {
            Some(Entry::Answered { edits, .. }) => edits.iter().map(|(_, f)| *f).collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn apply_changes_the_document_once_and_the_next_question_says_what_happened() {
        let mut app = PdfCraftApp::new();
        app.open_bytes("three.pdf", None, three_paragraphs()).unwrap();
        let (_, doc) = app.active_ids().unwrap();
        app.ai.chats.entry(doc).or_default().entries.push(answer_with_changes());
        app.apply_ai_edits(doc, 0);
        assert_eq!(fates(&app, doc), vec![Fate::Applied, Fate::NotFound(1)]);
        let texts: Vec<String> = app.session.get(doc).unwrap().text_blocks(0).into_iter().map(|b| b.text).collect();
        assert!(texts.iter().any(|t| t == "Second paragraph, the typo."), "{texts:?}");
        // Applying again changes nothing more: nothing is still proposed.
        let generation = app.session.get(doc).unwrap().edit_generation();
        app.apply_ai_edits(doc, 0);
        assert_eq!(app.session.get(doc).unwrap().edit_generation(), generation);
        // The model hears what became of each proposal.
        let history = app.ai.chats.get(&doc).unwrap().history();
        assert_eq!(history[0].results, vec![("t1".into(), "Applied.".into()), ("t2".into(), Fate::NotFound(1).result().unwrap())]);
    }

    /// A question on its way can be cancelled (an answer arriving after is dropped), and one that
    /// got no answer is asked again in its place; one the document refused isn't. No key is set up,
    /// so nothing reaches the network.
    #[test]
    fn a_question_can_be_cancelled_and_asked_again() {
        let mut app = PdfCraftApp::new();
        app.open_bytes("three.pdf", None, three_paragraphs()).unwrap();
        let (_, doc) = app.active_ids().unwrap();
        let (tx, rx) = channel();
        let (provider, model) = (Provider::Anthropic, "claude-opus-5-5".to_owned());
        app.ai.pending = Some(Pending { doc, shown: "Sum up".into(), task: Task::Summarize, prompt: Task::Summarize.prompt(), provider, model, rx });
        app.cancel_ai();
        assert!(!app.ai.busy());
        let _ = tx.send(Err(AiError::Network("late".into())));
        app.poll_ai();
        let failures = |app: &PdfCraftApp| -> Vec<(String, String, Option<Task>)> {
            let entries = app.ai.chats.get(&doc).map_or(&[][..], |c| c.entries.as_slice());
            entries
                .iter()
                .filter_map(|e| match e {
                    Entry::Failed { shown, error, retry } => Some((shown.clone(), error.clone(), retry.clone())),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(failures(&app), vec![("Sum up".into(), "Cancelled".into(), Some(Task::Summarize))], "the late answer is nobody's");
        app.retry_ai(doc, 0);
        assert!(app.ai.busy(), "asked again");
        for _ in 0..1000 {
            app.poll_ai();
            if !app.ai.busy() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let again = failures(&app);
        assert_eq!(again.len(), 1, "in the cancelled one's place: {:?}", again);
        assert_eq!((again[0].0.as_str(), again[0].2.clone()), ("Sum up", Some(Task::Summarize)));
        assert_ne!(again[0].1, "Cancelled", "this time the service's answer: no key");
        // What the document refused isn't offered again.
        app.ai.chats.clear();
        app.ai.fail(doc, "Sum up".into(), "refused".into(), None);
        app.retry_ai(doc, 0);
        assert!(!app.ai.busy());
        assert_eq!(failures(&app).len(), 1);
    }

    /// What the model was told about a proposal never changes once told, and a conversation
    /// about something else starts afresh.
    #[test]
    fn asking_again_settles_waiting_proposals_and_a_changed_document_starts_over() {
        let about = |generation| About {
            generation,
            provider: Provider::Anthropic,
            model: "claude-opus-5-5".into(),
            base_url: String::new(),
            reads_pdfs: false,
            title: "a.pdf".into(),
            language: "English".into(),
            edits: true,
        };
        let mut chat = Chat::default();
        chat.prepare(about(1));
        chat.entries.push(answer_with_changes());
        chat.prepare(about(1));
        let history = chat.history();
        assert_eq!(history.len(), 1, "the same conversation goes on");
        let told = history[0].results.clone();
        assert_eq!(told.len(), 2, "both proposals are reported: {told:?}");
        assert!(told.iter().all(|(_, r)| r.contains("discarded")), "{told:?}");
        // Nothing left to decide, so nothing can change what the model was told.
        assert!(matches!(chat.entries.first(), Some(Entry::Answered { edits, .. }) if edits.iter().all(|(_, f)| *f == Fate::Discarded)));
        chat.prepare(about(1));
        assert_eq!(chat.history()[0].results, told);
        // The document changed (an edit or an Undo): a new conversation.
        chat.prepare(about(2));
        assert!(chat.history().is_empty());
        assert!(matches!(chat.entries.last(), Some(Entry::NewConversation)));
        // Another language or title too.
        chat.entries.push(answer_with_changes());
        let mut other = about(2);
        other.language = "French".into();
        chat.prepare(other);
        assert!(chat.history().is_empty());
        // Another service, or another model: an answer only goes back to the one that gave it.
        let changes: [fn(&mut About); 4] = [
            |a: &mut About| a.provider = Provider::Gemini,
            |a: &mut About| a.model = "claude-haiku-5-5".into(),
            |a: &mut About| a.base_url = "https://openrouter.ai/api/v1".into(),
            |a: &mut About| a.reads_pdfs = true,
        ];
        for change in changes {
            chat.prepare(about(2));
            chat.entries.push(answer_with_changes());
            assert_eq!(chat.history().len(), 1);
            let mut other = about(2);
            change(&mut other);
            chat.prepare(other);
            assert!(chat.history().is_empty(), "{:?}", chat.about);
        }
    }

    /// The service chosen in the settings is asked once saved (Back leaves the one that works),
    /// and a compatible service's key goes only to the host it was saved for.
    #[test]
    fn a_service_is_asked_once_saved_and_its_key_goes_nowhere_else() {
        let mut ai = AiState::default();
        ai.set_key("sk-ant-a");
        ai.open_settings();
        ai.provider_draft = Provider::Gemini;
        ai.settings_open = false;
        assert_eq!(ai.provider, Provider::Anthropic, "Back: Claude, still set up");
        ai.open_settings();
        ai.provider_draft = Provider::Compatible;
        ai.base_url = "https://openrouter.ai/api/v1".into();
        ai.models[Provider::Compatible.index()] = "openai/gpt-6.1-sol".into();
        ai.key_drafts[Provider::Compatible.index()] = "sk-or-b".into();
        ai.save_settings();
        assert_eq!((ai.provider, ai.key_host.as_str(), ai.key_to_send()), (Provider::Compatible, "openrouter.ai", "sk-or-b"));
        // Another address, the key unsaved for it: none goes.
        ai.base_url = "https://api.mistral.ai/v1".into();
        assert_eq!(ai.key_to_send(), "");
        ai.base_url = "https://openrouter.ai/api/v2".into();
        assert_eq!(ai.key_to_send(), "sk-or-b", "the same host");
        ai.provider = Provider::Anthropic;
        assert_eq!(ai.key_to_send(), "sk-ant-a");
    }

    #[test]
    fn the_assistant_is_ready_once_its_service_is_set_up() {
        let mut ai = AiState::default();
        assert!(!ai.ready(), "no key");
        ai.set_key(
            " sk-ant-
abc ",
        );
        assert_eq!(ai.key(), "sk-ant-abc", "one line");
        assert!(ai.ready());
        // Each service has its own key and model.
        ai.provider = Provider::Gemini;
        assert_eq!((ai.key(), ai.model()), ("", "gemini-3.8-flash"));
        assert!(!ai.ready());
        // A compatible service: an address and a model, a key only if it takes one.
        ai.provider = Provider::Compatible;
        assert!(!ai.ready());
        ai.models[Provider::Compatible.index()] = "llama3.2".into();
        assert!(!ai.ready(), "a model, but no address");
        ai.base_url = "http://localhost:11434/v1".into();
        assert!(ai.ready());
        ai.models[Provider::Compatible.index()] = " ".into();
        assert!(!ai.ready(), "an address, but no model named");
        ai.models[Provider::Compatible.index()] = "llama3.2".into();
        assert_eq!(service_name(Provider::Compatible, &ai.base_url), "localhost");
        assert_eq!(service_name(Provider::Compatible, ""), "Another service (OpenAI-compatible)");
        assert_eq!(service_name(Provider::OpenAi, "https://ignored"), "ChatGPT (OpenAI)");
    }

    /// A service that doesn't read PDF files gets each page's text after its "[Page N]" line;
    /// a document with no text at all (a scan) gets nothing, which the request refuses.
    #[test]
    fn the_text_a_compatible_service_gets() {
        let mut session = pdfcraft_engine::Session::new();
        let id = session.open("three.pdf", None, std::sync::Arc::new(three_paragraphs()), None).unwrap();
        let text = document_text(session.get(id).unwrap().export_source(), 1);
        assert!(
            text.starts_with(
                "[Page 1]
"
            ),
            "{text:?}"
        );
        assert!(text.contains("Second paragraph, teh typo."), "{text:?}");
        let blank = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R] /Count 1 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] >> endobj
trailer << /Root 1 0 R >>
%%EOF";
        let id = session.open("blank.pdf", None, std::sync::Arc::new(blank.to_vec()), None).unwrap();
        assert_eq!(document_text(session.get(id).unwrap().export_source(), 1), "");
    }

    /// Every service's key and model are kept; a key saved before there were several services is
    /// Claude's; what isn't plain, or isn't offered, is dropped.
    #[test]
    fn the_assistants_settings_are_kept_and_read_back_carefully() {
        let mut app = PdfCraftApp::new();
        app.ai.provider = Provider::Compatible;
        app.ai.keys = ["sk-ant-a".into(), "sk-b".into(), "AIza-c".into(), String::new()];
        app.ai.models[Provider::OpenAi.index()] = "gpt-6-luna".into();
        app.ai.models[Provider::Compatible.index()] = "llama3.2".into();
        app.ai.base_url = "http://localhost:11434/v1".into();
        app.ai.reads_pdfs = true;
        let mut back = PdfCraftApp::new();
        back.restore(&app.persist());
        assert_eq!(back.ai.provider, Provider::Compatible);
        assert_eq!(back.ai.keys, app.ai.keys);
        assert_eq!(back.ai.models, app.ai.models);
        assert_eq!((back.ai.base_url.as_str(), back.ai.reads_pdfs), ("http://localhost:11434/v1", true));
        app.ai.key_host = "localhost".into();
        back.restore(&app.persist());
        assert_eq!(back.ai.key_host, "localhost");
        // From before: one key and model, Anthropic's.
        let mut old = PdfCraftApp::new();
        old.restore(r#"{"ai_key":"sk-ant-old","ai_model":"claude-haiku-5-5"}"#);
        assert_eq!((old.ai.provider, old.ai.key(), old.ai.model()), (Provider::Anthropic, "sk-ant-old", "claude-haiku-5-5"));
        // Untrusted: an unknown service, a key on two lines, a model the service doesn't offer.
        let mut bad = PdfCraftApp::new();
        bad.restore(
            r#"{"ai_provider":"skynet","ai_keys":{"openai":"sk-\nx"},"ai_models":{"openai":"gpt-9","gemini":"gemini-3.5-flash-lite"},"ai_url":"http://a b"}"#,
        );
        assert_eq!(bad.ai.provider, Provider::Anthropic);
        assert_eq!(bad.ai.keys[Provider::OpenAi.index()], "");
        assert_eq!(bad.ai.models[Provider::OpenAi.index()], "gpt-6.1-sol");
        assert_eq!(bad.ai.models[Provider::Gemini.index()], "gemini-3.5-flash-lite");
        assert_eq!(bad.ai.base_url, "");
    }

    #[test]
    fn discard_leaves_the_document_alone() {
        let mut app = PdfCraftApp::new();
        app.open_bytes("three.pdf", None, three_paragraphs()).unwrap();
        let (_, doc) = app.active_ids().unwrap();
        let generation = app.session.get(doc).unwrap().edit_generation();
        app.ai.chats.entry(doc).or_default().entries.push(answer_with_changes());
        app.discard_ai_edits(doc, 0);
        assert_eq!(fates(&app, doc), vec![Fate::Discarded, Fate::Discarded]);
        assert_eq!(app.session.get(doc).unwrap().edit_generation(), generation);
    }
}
