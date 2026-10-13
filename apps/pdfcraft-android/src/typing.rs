//! Typing on Android through the system's text dialog.
//!
//! A NativeActivity's window gets soft-keyboard typing only as key events, and the characters the
//! keyboard map can't produce as keys (accented letters, other alphabets, predictive text, voice
//! typing) never reach it. So when an egui text field takes the keyboard, PdfCraft shows the
//! system's own text dialog (an `EditText`, `jvm::TextDialog`), where every input method works, and
//! puts what was typed back into the field.
//!
//! The field's text is learnt by selecting all and copying (any egui text field answers that, and
//! the copy is kept out of the clipboard), and written back by selecting all and typing the
//! dialog's text, then Enter in a one-line field: the dialog's OK is the keyboard's Done, which is
//! how page numbers, zoom and search take what was typed. A password is never read into the
//! dialog. With a hardware keyboard, or once the dialog has failed, typing goes straight to the
//! window as before.
//!
//! This module holds the egui side, with the dialog behind [`Host`], so the host runs its tests on
//! real egui text fields.

/// What the text dialog shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The field's text, to edit.
    pub text: String,
    /// Hide what is typed (a password field).
    pub password: bool,
    /// Allow line breaks.
    pub multiline: bool,
}

/// How a text dialog ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// OK, with the text typed.
    Done(String),
    /// Cancel, the back key, or the dialog closed some other way.
    Cancelled,
    /// It couldn't be shown (or checked): type into the window instead.
    Failed,
}

/// What the system gives typing: the dialog and the window's soft keyboard.
pub trait Host {
    type Dialog;
    /// Show the dialog.
    fn open(&mut self, request: Request) -> Self::Dialog;
    /// How `dialog` ended, or `None` while it shows.
    fn poll(&mut self, dialog: &Self::Dialog) -> Option<Outcome>;
    /// Show or hide the window's own soft keyboard.
    fn soft_keyboard(&mut self, show: bool);
}

/// A text field at least this tall (points) is a multi-line one: line breaks are allowed.
pub const MULTILINE_HEIGHT: f32 = 44.0;
/// How often an open text dialog is checked.
pub const POLL: std::time::Duration = std::time::Duration::from_millis(120);

enum State<D> {
    /// Nothing under way.
    Idle,
    /// Select all and copy in `field` (`sent` once the events are queued).
    Reading { field: egui::Id, sent: bool, password: bool, multiline: bool },
    /// The dialog shows the field's text.
    Editing { field: egui::Id, dialog: D, multiline: bool },
    /// Put `text` into the field on the next frame.
    Writing { field: egui::Id, text: String, multiline: bool },
}

/// Typing through the dialog, frame by frame: [`Typing::queue`] before egui reads the input,
/// [`Typing::frame`] after the app has drawn.
pub struct Typing<D> {
    state: State<D>,
    /// The field the dialog last answered for: it opens again only for another field, or when
    /// this one is tapped again.
    handled: Option<egui::Id>,
    /// The dialog failed once: type straight into the window from then on.
    direct: bool,
    /// Whether the window's soft keyboard was asked for (typing straight into the window).
    keyboard: bool,
}

impl<D> Default for Typing<D> {
    fn default() -> Self {
        Self { state: State::Idle, handled: None, direct: false, keyboard: false }
    }
}

impl<D> Typing<D> {
    /// Whether this frame's copy reads a field for the dialog (it stays out of the clipboard).
    pub fn reading(&self) -> bool {
        matches!(self.state, State::Reading { sent: true, .. })
    }

    /// Queue the events typing needs this frame: select all and copy to read a field, or select
    /// all and type to write it.
    pub fn queue(&mut self, raw: &mut egui::RawInput) {
        match &mut self.state {
            State::Reading { sent, .. } if !*sent => {
                raw.events.extend(command_key(egui::Key::A));
                raw.events.push(egui::Event::Copy);
                *sent = true;
            }
            State::Writing { .. } => {
                let State::Writing { field, text, multiline } = std::mem::replace(&mut self.state, State::Idle) else { return };
                raw.events.extend(command_key(egui::Key::A));
                let text = field_text(&text, multiline);
                if text.is_empty() {
                    raw.events.extend(plain_key(egui::Key::Delete));
                } else {
                    raw.events.push(egui::Event::Text(text));
                }
                if !multiline {
                    raw.events.extend(plain_key(egui::Key::Enter));
                }
                self.handled = Some(field);
            }
            _ => {}
        }
    }

    /// Typing after the app drew this frame: open the dialog for a text field that took the
    /// keyboard, check on an open one, or (with `dialog_allowed` false, a hardware keyboard) ask
    /// for the window's soft keyboard. `copied` is the text this frame's copy put out.
    pub fn frame(&mut self, ctx: &egui::Context, copied: Option<String>, dialog_allowed: bool, host: &mut impl Host<Dialog = D>) {
        let focused = if ctx.egui_wants_keyboard_input() { ctx.memory(|m| m.focused()) } else { None };
        if focused.is_none() {
            self.handled = None;
        }
        let released = ctx.input(|i| if i.pointer.any_released() { i.pointer.interact_pos() } else { None });
        let field_rect = focused.and_then(|id| ctx.read_response(id)).map(|r| r.rect);
        let tapped = released.zip(field_rect).is_some_and(|(at, rect)| rect.contains(at));
        match std::mem::replace(&mut self.state, State::Idle) {
            State::Idle => {
                let ime = ctx.output(|o| o.ime);
                if dialog_allowed && !self.direct {
                    if let (Some(field), Some(ime)) = (focused, ime)
                        && (self.handled != Some(field) || tapped)
                    {
                        // winit asked for the window's keyboard; the dialog brings its own.
                        host.soft_keyboard(false);
                        self.keyboard = false;
                        let password = ime.purpose == egui::IMEPurpose::Password;
                        let multiline = ime.rect.height() >= MULTILINE_HEIGHT;
                        self.state = State::Reading { field, sent: false, password, multiline };
                        ctx.request_repaint();
                    }
                } else {
                    self.soft_keyboard(focused.is_some(), tapped, host);
                }
            }
            State::Reading { field, sent: false, password, multiline } => {
                self.state = State::Reading { field, sent: false, password, multiline };
                ctx.request_repaint();
            }
            State::Reading { field, sent: true, password, multiline } => {
                let text = if password { String::new() } else { copied.unwrap_or_default() };
                self.state = State::Editing { field, dialog: host.open(Request { text, password, multiline }), multiline };
                ctx.request_repaint_after(POLL);
            }
            State::Editing { field, dialog, multiline } => match host.poll(&dialog) {
                None => {
                    self.state = State::Editing { field, dialog, multiline };
                    ctx.request_repaint_after(POLL);
                }
                Some(Outcome::Done(text)) => {
                    self.state = State::Writing { field, text, multiline };
                    ctx.request_repaint();
                }
                Some(Outcome::Cancelled) => self.handled = Some(field),
                Some(Outcome::Failed) => {
                    log::warn!("the system text dialog failed; typing goes straight to the window");
                    self.direct = true;
                    self.handled = Some(field);
                    self.soft_keyboard(true, true, host);
                }
            },
            // `queue` writes it before the next frame.
            State::Writing { field, text, multiline } => {
                self.state = State::Writing { field, text, multiline };
                ctx.request_repaint();
            }
        }
    }

    /// Typing straight into the window: winit asks for the keyboard implicitly, which some
    /// devices ignore, so ask outright while a text field has focus, and again when the field is
    /// tapped after the keyboard was put away (focus didn't change, so nothing else would bring
    /// it back).
    fn soft_keyboard(&mut self, want: bool, tapped: bool, host: &mut impl Host<Dialog = D>) {
        if want != self.keyboard || (want && tapped) {
            host.soft_keyboard(want);
            self.keyboard = want;
        }
    }
}

/// A key press and release with the platform's command modifier (Ctrl).
fn command_key(key: egui::Key) -> [egui::Event; 2] {
    let event = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::COMMAND };
    [event(true), event(false)]
}

/// A key press and release without modifiers.
fn plain_key(key: egui::Key) -> [egui::Event; 2] {
    let event = |pressed| egui::Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE };
    [event(true), event(false)]
}

/// The dialog's text as the field takes it: no line breaks in a one-line field, and no control
/// characters but line breaks and tabs.
pub fn field_text(text: &str, multiline: bool) -> String {
    text.chars()
        .map(|c| if !multiline && (c == '\n' || c == '\r') { ' ' } else { c })
        .filter(|c| !c.is_control() || (multiline && *c == '\n') || *c == '\t')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dialog that answers with `answer` once it has been polled `after` times.
    struct Fake {
        answer: Outcome,
        after: usize,
        opened: Vec<Request>,
        polls: usize,
        keyboard: Vec<bool>,
    }

    impl Fake {
        fn new(answer: Outcome) -> Self {
            Self { answer, after: 2, opened: Vec::new(), polls: 0, keyboard: Vec::new() }
        }
    }

    impl Host for Fake {
        type Dialog = ();
        fn open(&mut self, request: Request) {
            self.opened.push(request);
        }
        fn poll(&mut self, _: &()) -> Option<Outcome> {
            self.polls += 1;
            (self.polls > self.after).then(|| self.answer.clone())
        }
        fn soft_keyboard(&mut self, show: bool) {
            self.keyboard.push(show);
        }
    }

    /// The app: one text field (multi-line or password as asked), and a counter of Enters.
    struct App {
        text: String,
        multiline: bool,
        password: bool,
        enters: usize,
        id: egui::Id,
    }

    /// Run one frame the way the Android shell does: queue, draw, then [`Typing::frame`] inside
    /// the same pass (the shell calls it at the end of `App::ui`). Returns what a read copied.
    fn frame(ctx: &egui::Context, app: &mut App, typing: &mut Typing<()>, host: &mut Fake, focus: bool) -> Option<String> {
        let mut raw =
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 800.0))), ..Default::default() };
        typing.queue(&mut raw);
        let mut read = None;
        let mut output = ctx.run_ui(raw, |ui| {
            let edit = if app.multiline { egui::TextEdit::multiline(&mut app.text) } else { egui::TextEdit::singleline(&mut app.text) };
            let r = ui.add(edit.password(app.password).id(app.id));
            if focus {
                r.request_focus();
            }
            if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                app.enters += 1;
            }
            let ctx = ui.ctx().clone();
            let copied = ctx.output(|o| {
                o.commands.iter().rev().find_map(|c| match c {
                    egui::OutputCommand::CopyText(t) => Some(t.clone()),
                    _ => None,
                })
            });
            if typing.reading() {
                read = copied.clone();
            }
            typing.frame(&ctx, copied, true, host);
        });
        // No renderer here: the font atlas upload is dropped on purpose.
        output.textures_delta.clear();
        read
    }

    fn run(app: &mut App, answer: Outcome) -> (Fake, Vec<String>) {
        let ctx = egui::Context::default();
        let mut typing = Typing::default();
        let mut host = Fake::new(answer);
        let mut leaked = Vec::new();
        for i in 0..12 {
            if let Some(copy) = frame(&ctx, app, &mut typing, &mut host, i < 2) {
                leaked.push(copy);
            }
        }
        (host, leaked)
    }

    fn app(text: &str, multiline: bool, password: bool) -> App {
        App { text: text.into(), multiline, password, enters: 0, id: egui::Id::new("field") }
    }

    #[test]
    fn the_dialog_gets_the_fields_text_and_its_answer_replaces_it_then_enter() {
        let mut a = app("Facture", false, false);
        let (host, read) = run(&mut a, Outcome::Done("Été 2026 — Œuvre".into()));
        assert_eq!(host.opened, [Request { text: "Facture".into(), password: false, multiline: false }]);
        assert_eq!(read, ["Facture"], "the read copy is handed to the dialog, not the clipboard");
        assert_eq!(a.text, "Été 2026 — Œuvre", "accents and other letters arrive whole");
        assert_eq!(a.enters, 1, "OK is Done: a one-line field gets Enter");
        assert_eq!(host.opened.len(), 1, "the dialog doesn't reopen for the field it answered");
    }

    #[test]
    fn a_multiline_field_keeps_line_breaks_and_gets_no_enter() {
        let mut a = app("Ligne 1", true, false);
        let (host, _) = run(&mut a, Outcome::Done("Première\nseconde".into()));
        assert!(host.opened.first().is_some_and(|r| r.multiline));
        assert_eq!(a.text, "Première\nseconde");
        assert_eq!(a.enters, 0);
    }

    /// Two layers keep a password out of the dialog: egui doesn't copy a password field, and
    /// `frame` never passes one on. This pins the behaviour, whichever layer holds.
    #[test]
    fn a_password_is_never_read_into_the_dialog() {
        let mut a = app("secret", false, true);
        let (host, _) = run(&mut a, Outcome::Done("nouveau".into()));
        assert_eq!(host.opened, [Request { text: String::new(), password: true, multiline: false }]);
        assert_eq!(a.text, "nouveau");
    }

    #[test]
    fn cancel_leaves_the_field_as_it_was() {
        let mut a = app("Facture", false, false);
        let (host, _) = run(&mut a, Outcome::Cancelled);
        assert_eq!(host.opened.len(), 1);
        assert_eq!(a.text, "Facture");
        assert_eq!(a.enters, 0);
    }

    #[test]
    fn ok_with_nothing_typed_empties_the_field() {
        let mut a = app("Facture", false, false);
        let _ = run(&mut a, Outcome::Done(String::new()));
        assert_eq!(a.text, "");
    }

    #[test]
    fn a_failed_dialog_falls_back_to_the_soft_keyboard_for_good() {
        let mut a = app("Facture", false, false);
        let (host, _) = run(&mut a, Outcome::Failed);
        assert_eq!(a.text, "Facture");
        assert_eq!(host.keyboard.last(), Some(&true), "typing goes to the window's keyboard");
        assert_eq!(host.opened.len(), 1, "and the dialog isn't tried again");
    }

    #[test]
    fn line_breaks_and_control_characters_are_cleaned_for_the_field() {
        assert_eq!(field_text("a\nb\r\nc\u{7}", false), "a b  c");
        assert_eq!(field_text("a\nb\r\nc\u{7}\td", true), "a\nb\nc\td");
    }
}
