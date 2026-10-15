# pdfcraft-ai

The optional AI assistant (layer 4): summarize a document, answer questions about it, translate a
page, propose changes to its text. Acrobat's AI Assistant is a cloud service tied to Adobe
accounts; PdfCraft's talks to the service the user chooses, with the user's own key:

| Service | API | The document goes as |
|---|---|---|
| Claude (Anthropic) | Messages API (`anthropic.rs`) | a PDF `document` block, cached |
| ChatGPT (OpenAI) | Responses API (`openai.rs`) | an `input_file` |
| Gemini (Google) | Interactions API (`gemini.rs`) | a `document` |
| Any OpenAI-compatible service: OpenRouter, Mistral, DeepSeek, Groq, a local Ollama… | Chat Completions (`compatible.rs`) | its text, page by page, or a `file` part where the service reads PDFs |

- **Off until asked.** Nothing is sent until the service is set up (its key; a compatible
  service's address and model) and the user asks a question. Each request carries the open
  document and the conversation about it, nothing else, and nothing is stored on the service's
  side between questions (`store: false` where the API keeps conversations).
- **The conversation goes back as the service gave it**: every answer exactly as it came (Claude's
  thinking, OpenAI's encrypted reasoning, Gemini's thoughts and their signatures), what became of
  each change it proposed, then the next question. A new conversation starts with another service
  or model.
- **Proposed changes** come through one `edit_text` tool, the same for every service; every
  proposal is checked here before the user sees it.
- **Cost guards.** The user sets the most pages a request may send (at most 600), PDFs over 22 MB
  and texts over 1.6 million characters are refused before sending, and every answer reports its
  tokens and, for the models known here, an estimate of what it cost at list prices (Gemini's at
  its regular prices, above the introductory ones of late 2026). A compatible service's prices
  aren't known.
- **Models:** Claude Opus 5.5, Sonnet 5.5 and Haiku 5.5 (Opus and Sonnet ask for server-side
  refusal fallbacks, `fallbacks: "default"`); GPT-6 Astra, GPT-6.1 Sol (OpenAI's default) and
  GPT-6 Luna; Gemini 4 Argon, Gemini 3.8 Flash and Gemini 3.5 Flash-Lite (the last two on
  Google's free tier too, where what is sent may be used to improve Google's products). A
  compatible service takes the model it names.
- **Transport:** one blocking HTTP request (`ureq` over rustls) with the system's trusted
  certificate authorities: the OS store on desktops, `/system/etc/security/cacerts` on Android. A
  compatible service on the local network may be plain `http://`. Run [`send`] on a worker thread.

`request_body` and `parse_response` are pure and unit-tested for every service. The live checks
ask the real APIs when their key is set: `ANTHROPIC_API_KEY`, `OPENAI_API_KEY` or `GEMINI_API_KEY`
(`cargo test -p pdfcraft-ai -- --ignored live_`); each costs a fraction of a cent, Gemini's nothing
on its free tier.
