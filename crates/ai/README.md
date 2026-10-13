# pdfcraft-ai

The optional AI assistant (layer 4): summarize a document, answer questions about it, translate a
page. Acrobat's AI Assistant is a cloud service tied to Adobe accounts; PdfCraft's uses the user's
own Anthropic API key instead.

- **Off until asked.** Nothing is sent until the user has entered a key and asks a question. Each
  request carries the open document and the conversation about it, nothing else.
- **The document goes as a PDF block**, so Claude reads its text and sees its pages (scans too). It
  is marked for prompt caching: follow-up questions read it back from the cache.
- **Cost guards.** The user sets the most pages a request may send (at most 600, the API's limit),
  PDFs over 22 MB are refused before sending, and every answer reports its tokens and an estimate
  of what it cost at list prices.
- **Models:** Claude Opus 5.5 by default; Sonnet 5.5 and Haiku 5.5 cost less. Opus and Sonnet
  requests ask for server-side refusal fallbacks (`fallbacks: "default"`).
- **Transport:** one blocking HTTPS request (`ureq` over rustls) with the system's trusted
  certificate authorities: the OS store on desktops, `/system/etc/security/cacerts` on Android.
  Run [`send`] on a worker thread.

`request_body` and `parse_response` are pure and unit-tested; `a_real_question_gets_an_answer`
asks the real API when `ANTHROPIC_API_KEY` is set (`cargo test -p pdfcraft-ai -- --ignored`).
