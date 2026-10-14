//! The AI assistant's screen: summarize the open document, ask about it, translate a page, with
//! the user's own Anthropic API key (`pdfcraft-ai`). Off until a key is entered; each request
//! runs on a worker thread and its answer arrives in [`PdfCraftApp::poll_ai`].

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Align2, Color32, CornerRadius, Rect, Sense, Stroke, vec2};
use pdfcraft_ai::{AiError, Model, ProposedEdit, Reply, Task, Turn};
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
    Answered { shown: String, prompt: String, reply: Reply, model: Model, edits: Vec<(ProposedEdit, Fate)> },
    /// A question that got no answer, and why.
    Failed { shown: String, error: String },
    /// A new conversation starts here: the document or the model changed, or the last one grew
    /// long. Earlier answers stay on screen but aren't sent again.
    NewConversation,
}

/// What a conversation was had about: everything that goes into what the model has already
/// read (the document, its title, the system prompt's language and tool). The conversation as
/// the model saw it can't change after the fact, so when any of this does — an edit, an Undo,
/// Save As, another interface language — a new one starts.
#[derive(Clone, Debug, PartialEq, Eq)]
struct About {
    /// The document's edit generation: it moves with every edit, Undo, Redo and save.
    generation: u64,
    model: Model,
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
    prompt: String,
    model: Model,
    rx: Receiver<Result<Reply, AiError>>,
}

/// The assistant's settings and conversations.
pub struct AiState {
    /// The user's Anthropic API key. Empty: the assistant is off and nothing is ever sent.
    pub key: String,
    pub model: Model,
    /// The most pages a question may send.
    pub max_pages: usize,
    chats: HashMap<DocId, Chat>,
    pending: Option<Pending>,
    input: String,
    key_draft: String,
    settings_open: bool,
    /// The language pages are translated into (a language's own name, "Français").
    translate_to: String,
}

impl Default for AiState {
    fn default() -> Self {
        Self {
            key: String::new(),
            model: Model::default(),
            max_pages: DEFAULT_MAX_PAGES,
            chats: HashMap::new(),
            pending: None,
            input: String::new(),
            key_draft: String::new(),
            settings_open: false,
            translate_to: String::new(),
        }
    }
}

impl AiState {
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

    fn fail(&mut self, doc: DocId, shown: String, error: String) {
        self.chats.entry(doc).or_default().entries.push(Entry::Failed { shown, error });
    }
}

/// The interface language's own name, which answers are written in.
fn reply_language() -> String {
    crate::i18n::current().name().to_owned()
}

/// What to tell the user when a question got no answer.
fn error_text(e: &AiError) -> String {
    use crate::i18n::fmt;
    match e {
        AiError::NoKey => tl!("Add your Anthropic API key to use the assistant.").to_owned(),
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
        AiError::NotAllowed(m) => fmt(tl!("Your Anthropic account can't use this model, or has no credit left: {error}"), &[("error", m)]),
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
        // The working file as it is now, edits included. It stays the same object until the
        // document changes, so later questions send the very same bytes and read them back
        // from the cache; nothing is written for it.
        let pdf = doc.bytes.clone();
        let model = self.ai.model;
        let (key, max_pages, pages, title, language) =
            (self.ai.key.clone(), self.ai.max_pages, doc.info.pages.len(), doc.display_name(), reply_language());
        // Changes are offered only where the user could make them by hand.
        let edits = doc.allows_modification();
        let chat = self.ai.chats.entry(id).or_default();
        chat.prepare(About { generation: doc.edit_generation(), model, title: title.clone(), language: language.clone(), edits });
        let history = chat.history();
        let prompt = task.prompt();
        let (tx, rx) = channel();
        let ctx = self.ctx.clone();
        let spawned = std::thread::Builder::new().name("pdfcraft-ai".into()).spawn(move || {
            let request = pdfcraft_ai::Request {
                key: &key,
                model,
                title: &title,
                pdf: &pdf,
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
            Ok(_) => self.ai.pending = Some(Pending { doc: id, shown, prompt, model, rx }),
            Err(e) => self.ai.fail(id, shown, e.to_string()),
        }
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
        let Some(Pending { doc, shown, prompt, model, .. }) = self.ai.pending.take() else { return };
        if self.session.get(doc).is_none() {
            // Closed while the question was on its way.
            return;
        }
        let entry = match result {
            Ok(reply) => {
                let edits = reply.edits.iter().map(|e| (e.clone(), Fate::Proposed)).collect();
                Entry::Answered { shown, prompt, reply, model, edits }
            }
            Err(e) => Entry::Failed { shown, error: error_text(&e) },
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
    let settings = app.ai.settings_open || app.ai.key.trim().is_empty();
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
        app.ai.settings_open = !app.ai.settings_open;
        app.ai.key_draft = app.ai.key.clone();
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
        // Opening: the draft starts from the saved key.
        app.ai.settings_open = true;
        app.ai.key_draft = app.ai.key.clone();
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
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        egui::Frame::NONE.inner_margin(egui::Margin::symmetric(16, 14)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 8.0;
            ui.label(
                egui::RichText::new(tl!(
                    "Your question and the open document are sent to Anthropic (Claude) with your own API key, only when you ask. Requests are billed to your Anthropic account."
                ))
                .color(t.text_muted),
            );
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tl!("Anthropic API key")).font(theme::semibold(14.0)));
            ui.add(widgets::line(&mut app.ai.key_draft).password(true).hint_text("sk-ant-…").desired_width(f32::INFINITY).char_limit(512));
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tl!("Model")).font(theme::semibold(14.0)));
            for m in Model::ALL {
                let p = price_line(m);
                widgets::radio_value(ui, &mut app.ai.model, m, format!("{}  ·  {p}", m.name()));
            }
            ui.add_space(4.0);
            ui.label(egui::RichText::new(tl!("Most pages sent per question")).font(theme::semibold(14.0)));
            ui.spacing_mut().slider_width = (ui.available_width() - 90.0).max(80.0);
            ui.add(egui::Slider::new(&mut app.ai.max_pages, 1..=pdfcraft_ai::API_MAX_PAGES).logarithmic(true));
            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if pill(ui, tl!("Save"), true, t).clicked() {
                    // A key pasted from a mail or a PDF may carry line breaks or spaces: a key has none.
                    app.ai.key = app.ai.key_draft.split_whitespace().collect();
                    app.ai.settings_open = false;
                }
                if !app.ai.key.is_empty() && pill(ui, tl!("Remove"), false, t).clicked() {
                    app.ai.key.clear();
                    app.ai.key_draft.clear();
                }
            });
        });
    });
}

/// "4 $ / 20 $" for a million tokens read / written, as the radio buttons show it.
fn price_line(m: Model) -> String {
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
                                answer(ui, &reply.text, t, Some((reply, *model)));
                                if let Some(apply) = proposals(ui, edits, t) {
                                    decide = Some((index, apply));
                                }
                            }
                            Entry::Failed { shown, error } => {
                                bubble(ui, shown, t);
                                answer(ui, error, t, None);
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
        let r = ui.add(edit);
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        ui.add_space(8.0);
        let ready = !busy && !app.ai.input.trim().is_empty();
        let send = ui.add_enabled_ui(ready, |ui| icons::button(ui, "send", send_w, ready, tl!("Send"))).inner.clicked();
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
fn cost_line(reply: &Reply, asked: Model) -> String {
    use crate::i18n::fmt;
    let tokens = reply.usage.total().to_string();
    let mut line = match reply.cost_usd() {
        Some(cost) => fmt(tl!("≈ {cost} $ · {tokens} tokens"), &[("cost", &format!("{cost:.3}")), ("tokens", &tokens)]),
        None => fmt(tl!("{tokens} tokens"), &[("tokens", &tokens)]),
    };
    // A dated id ("…-20261001") is still the model asked.
    if (reply.fell_back || !reply.model.starts_with(asked.id())) && !reply.model.is_empty() {
        line.push_str(" · ");
        match Model::from_id(&reply.model) {
            Some(m) => line.push_str(m.name()),
            None => line.push_str(&reply.model),
        }
    }
    line
}

/// An answer (or why there is none), with what it cost and a Copy button.
fn answer(ui: &mut egui::Ui, text: &str, t: &Tokens, reply: Option<(&Reply, Model)>) {
    egui::Frame::NONE
        .fill(t.card)
        .stroke(Stroke::new(1.0, t.divider))
        .corner_radius(CornerRadius::same(14))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            // Why there is no answer, in a red that keeps its contrast on either theme's card.
            let error = if t.dark() { Color32::from_rgb(0xFF, 0x8A, 0x80) } else { Color32::from_rgb(0xC6, 0x28, 0x28) };
            let color = if reply.is_some() { t.text } else { error };
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
                    if icons::button(ui, "copy", 36.0, false, tl!("Copy")).clicked() {
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
            model: Model::Opus,
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

    /// What the model was told about a proposal never changes once told, and a conversation
    /// about something else starts afresh.
    #[test]
    fn asking_again_settles_waiting_proposals_and_a_changed_document_starts_over() {
        let about = |generation| About { generation, model: Model::Opus, title: "a.pdf".into(), language: "English".into(), edits: true };
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
