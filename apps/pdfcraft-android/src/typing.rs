//! Typing on Android, in place.
//!
//! A NativeActivity's window gets soft-keyboard typing only as key events, and the characters the
//! keyboard map can't produce as keys (accented letters, other alphabets, predictive text, voice
//! typing) never reach it. So when an egui text field takes the keyboard, an Android text view
//! takes it instead, out of sight (`jvm::TextMirror`): every input method works there, and what it
//! holds is copied into the field as it changes, caret and all, so the typing shows in the field
//! itself. A caret moved in the field moves the view's too. Enter in a one-line field is its Done:
//! it ends the typing and presses Enter in the field, which is how page numbers, zoom and search
//! take what was typed. The window gives that view the keys meanwhile (winit's
//! `set_keys_to_views`), all but Back, which closes the field as Escape does.
//!
//! A password is typed in the system's text dialog instead (`jvm::TextDialog`), which never shows
//! it in the field's place, and it is never read into the dialog.
//!
//! The field's text is learnt by selecting all and copying (any egui text field answers that, and
//! the copy is kept out of the clipboard), and written by selecting all and typing. With a
//! hardware keyboard, or once the system's views have failed, typing goes straight to the window.
//!
//! This module holds the egui side, with the system's views behind [`Host`], so the host runs its
//! tests on real egui text fields.

/// What the system's view starts with.
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

/// What the mirroring text view holds, as last read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub text: String,
    /// The selection, in UTF-16 units as Android counts them.
    pub start: usize,
    pub end: usize,
    /// The view still has the focus, and with it the keyboard.
    pub focused: bool,
}

/// How the mirroring text view is doing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorPoll {
    /// Not shown yet.
    Opening,
    /// Shown; what it held at the last read (`None` before the first).
    Open(Option<Seen>),
    /// It couldn't be shown (or read): type into the window instead.
    Failed,
    /// It went away (the activity went to the background, say).
    Gone,
}

/// What the system gives typing: its text views and the window's soft keyboard.
pub trait Host {
    type Dialog;
    type Mirror;
    /// Show the text dialog (passwords).
    fn open(&mut self, request: Request) -> Self::Dialog;
    /// How `dialog` ended, or `None` while it shows.
    fn poll(&mut self, dialog: &Self::Dialog) -> Option<Outcome>;
    /// Show the mirroring text view, with the keyboard.
    fn open_mirror(&mut self, request: Request) -> Self::Mirror;
    /// What it holds now (a read reflects the view a moment ago).
    fn poll_mirror(&mut self, mirror: &Self::Mirror) -> MirrorPoll;
    /// Select `start..end` there (UTF-16 units).
    fn select_mirror(&mut self, mirror: &Self::Mirror, start: usize, end: usize);
    /// Give it the keyboard again.
    fn reshow_mirror(&mut self, mirror: &Self::Mirror);
    /// Put it away, with the keyboard.
    fn close_mirror(&mut self, mirror: Self::Mirror);
    /// Show or hide the window's own soft keyboard.
    fn soft_keyboard(&mut self, show: bool);
}

/// A text field at least this tall (points) is a multi-line one: line breaks are allowed.
pub const MULTILINE_HEIGHT: f32 = 44.0;
/// How often an open text dialog is checked.
pub const POLL: std::time::Duration = std::time::Duration::from_millis(120);
/// How often the mirroring view is read while typing: often enough that typing shows at once.
pub const MIRROR_POLL: std::time::Duration = std::time::Duration::from_millis(30);
/// How long the window is drawn again after the keyboard goes (seconds): the room it leaves comes
/// back after its animation, and nothing else would draw the window then.
pub const SETTLE: f64 = 1.0;

/// The field's text and caret as last agreed with the mirroring view.
struct Live {
    /// The view's text as last put into the field.
    shown: String,
    /// The field's caret (char indices) as last set or seen.
    cursor: (usize, usize),
    /// The view's caret (char indices) as it last reported it.
    view: (usize, usize),
    /// A write for [`Typing::queue`] to send, and the caret to give the field after it.
    write: Option<(String, (usize, usize))>,
    /// The caret to give the field once a write has gone through.
    place: Option<(usize, usize)>,
}

enum State<D, M> {
    /// Nothing under way.
    Idle,
    /// Select all and copy in `field` (`sent` once the events are queued), for the dialog
    /// (`password`) or the mirroring view.
    Reading { field: egui::Id, sent: bool, password: bool, multiline: bool },
    /// The dialog shows the field's text.
    Editing { field: egui::Id, dialog: D, multiline: bool },
    /// The mirroring view has the keyboard.
    Mirroring { field: egui::Id, mirror: M, multiline: bool, live: Live },
    /// Put `text` into the field on the next frame, then Enter in a one-line field.
    Writing { field: egui::Id, text: String, multiline: bool },
}

/// Typing frame by frame: [`Typing::queue`] before egui reads the input, [`Typing::frame`] after
/// the app has drawn.
pub struct Typing<D, M> {
    state: State<D, M>,
    /// The field typing last ended in: it starts again only for another field, or when this one
    /// is tapped again.
    handled: Option<egui::Id>,
    /// The system's views failed once: type straight into the window from then on.
    direct: bool,
    /// Whether the window's soft keyboard was asked for (typing straight into the window).
    keyboard: bool,
    /// Draw the window again until this time (the keyboard just went).
    settle_until: f64,
}

impl<D, M> Default for Typing<D, M> {
    fn default() -> Self {
        Self { state: State::Idle, handled: None, direct: false, keyboard: false, settle_until: 0.0 }
    }
}

impl<D, M> Typing<D, M> {
    /// Whether this frame's copy reads a field (it stays out of the clipboard).
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
            State::Mirroring { live, multiline, .. } => {
                if let Some((text, cursor)) = live.write.take() {
                    replace(raw, &field_text(&text, *multiline));
                    live.place = Some(cursor);
                }
            }
            State::Writing { .. } => {
                let State::Writing { field, text, multiline } = std::mem::replace(&mut self.state, State::Idle) else { return };
                replace(raw, &field_text(&text, multiline));
                if !multiline {
                    raw.events.extend(plain_key(egui::Key::Enter));
                }
                self.handled = Some(field);
            }
            _ => {}
        }
    }

    /// Typing after the app drew this frame: start it for a text field that took the keyboard,
    /// follow it while it goes on, or (with `system_views` false, a hardware keyboard) ask for
    /// the window's soft keyboard. `copied` is the text this frame's copy put out.
    pub fn frame(&mut self, ctx: &egui::Context, copied: Option<String>, system_views: bool, host: &mut impl Host<Dialog = D, Mirror = M>) {
        let now = ctx.input(|i| i.time);
        if now < self.settle_until {
            ctx.request_repaint_after(MIRROR_POLL);
        }
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
                if system_views && !self.direct {
                    if let (Some(field), Some(ime)) = (focused, ime)
                        && (self.handled != Some(field) || tapped)
                    {
                        // winit asked for the window's keyboard; the system's view brings its own.
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
            State::Reading { field, sent: true, password: true, multiline } => {
                // A password field is never copied (egui doesn't), nor shown.
                self.state = State::Editing { field, dialog: host.open(Request { text: String::new(), password: true, multiline }), multiline };
                ctx.request_repaint_after(POLL);
            }
            State::Reading { field, sent: true, password: false, multiline } => {
                let text = copied.unwrap_or_default();
                // Reading selected all: the caret goes back to the end, where typing goes on.
                let end = text.chars().count();
                set_cursor(ctx, field, (end, end));
                let mirror = host.open_mirror(Request { text: text.clone(), password: false, multiline });
                let live = Live { shown: text, cursor: (end, end), view: (end, end), write: None, place: None };
                self.state = State::Mirroring { field, mirror, multiline, live };
                ctx.request_repaint_after(MIRROR_POLL);
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
                Some(Outcome::Failed) => self.fail(field, host),
            },
            State::Mirroring { field, mirror, multiline, live } => {
                if focused != Some(field) {
                    // Another field, or none: this one's typing is over.
                    host.close_mirror(mirror);
                    self.settle(ctx);
                    return;
                }
                // The keyboard went (Back, its own button) and the field was tapped: it comes back.
                if tapped {
                    host.reshow_mirror(&mirror);
                }
                self.mirror(ctx, field, mirror, multiline, live, host);
            }
            // `queue` writes it before the next frame.
            State::Writing { field, text, multiline } => {
                self.state = State::Writing { field, text, multiline };
                ctx.request_repaint();
            }
        }
    }

    /// Follow the mirroring view of `field` for a frame.
    fn mirror(
        &mut self,
        ctx: &egui::Context,
        field: egui::Id,
        mirror: M,
        multiline: bool,
        mut live: Live,
        host: &mut impl Host<Dialog = D, Mirror = M>,
    ) {
        // A write went through this frame: the caret goes where the view has it.
        if let Some(cursor) = live.place.take() {
            set_cursor(ctx, field, cursor);
            live.cursor = cursor;
        }
        let seen = match host.poll_mirror(&mirror) {
            MirrorPoll::Opening | MirrorPoll::Open(None) => None,
            MirrorPoll::Open(Some(seen)) if seen.focused => Some(seen),
            // The view lost the keyboard: the field keeps its text; a tap types again.
            MirrorPoll::Open(Some(_)) | MirrorPoll::Gone => {
                host.close_mirror(mirror);
                self.settle(ctx);
                self.handled = Some(field);
                return;
            }
            MirrorPoll::Failed => {
                host.close_mirror(mirror);
                self.settle(ctx);
                self.fail(field, host);
                return;
            }
        };
        if let Some(seen) = seen {
            // Enter in a one-line field is its Done: the line break the keyboard put just before the
            // caret. Others (pasted) become spaces as the field takes them.
            let caret = char_at(&seen.text, seen.end);
            let enter = caret.checked_sub(1).filter(|_| !multiline).filter(|at| seen.text.chars().nth(*at).is_some_and(|c| matches!(c, '\n' | '\r')));
            if let Some(at) = enter {
                let text: String = seen.text.chars().enumerate().filter(|(i, _)| *i != at).map(|(_, c)| c).collect();
                host.close_mirror(mirror);
                self.settle(ctx);
                self.state = State::Writing { field, text, multiline };
                ctx.request_repaint();
                return;
            }
            let view = (char_at(&seen.text, seen.start), char_at(&seen.text, seen.end));
            if seen.text != live.shown {
                live.write = Some((seen.text.clone(), view));
                live.shown = seen.text;
                live.view = view;
                ctx.request_repaint();
            } else if live.write.is_none() {
                let field_cursor = cursor_of(ctx, field);
                if view != live.view {
                    // The keyboard moved the caret (its arrows, a swipe on the space bar).
                    set_cursor(ctx, field, view);
                    live.cursor = view;
                    live.view = view;
                } else if let Some(moved) = field_cursor.filter(|c| *c != live.cursor) {
                    // A finger moved it in the field: the view's follows.
                    host.select_mirror(&mirror, utf16_at(&live.shown, moved.0), utf16_at(&live.shown, moved.1));
                    live.cursor = moved;
                }
            }
        }
        self.state = State::Mirroring { field, mirror, multiline, live };
        ctx.request_repaint_after(MIRROR_POLL);
    }

    /// The keyboard just went: draw the window again for a moment, so the page takes back its room.
    fn settle(&mut self, ctx: &egui::Context) {
        self.settle_until = ctx.input(|i| i.time) + SETTLE;
        ctx.request_repaint_after(MIRROR_POLL);
    }

    /// The system's views failed: type straight into the window from now on.
    fn fail(&mut self, field: egui::Id, host: &mut impl Host<Dialog = D, Mirror = M>) {
        log::warn!("the system's text views failed; typing goes straight to the window");
        self.direct = true;
        self.handled = Some(field);
        self.soft_keyboard(true, true, host);
    }

    /// Typing straight into the window: winit asks for the keyboard implicitly, which some
    /// devices ignore, so ask outright while a text field has focus, and again when the field is
    /// tapped after the keyboard was put away (focus didn't change, so nothing else would bring
    /// it back).
    fn soft_keyboard(&mut self, want: bool, tapped: bool, host: &mut impl Host<Dialog = D, Mirror = M>) {
        if want != self.keyboard || (want && tapped) {
            host.soft_keyboard(want);
            self.keyboard = want;
        }
    }
}

/// Replace a field's text: select all, then type `text` (or delete, for none).
fn replace(raw: &mut egui::RawInput, text: &str) {
    raw.events.extend(command_key(egui::Key::A));
    if text.is_empty() {
        raw.events.extend(plain_key(egui::Key::Delete));
    } else {
        raw.events.push(egui::Event::Text(text.to_owned()));
    }
}

/// The field's caret (char indices, in order), if it has one.
fn cursor_of(ctx: &egui::Context, field: egui::Id) -> Option<(usize, usize)> {
    let range = egui::text_edit::TextEditState::load(ctx, field)?.cursor.char_range()?.as_sorted_char_range();
    Some((range.start.0, range.end.0))
}

/// Put the field's caret at `start..end` (char indices).
fn set_cursor(ctx: &egui::Context, field: egui::Id, (start, end): (usize, usize)) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ctx, field) {
        let range = egui::text_selection::CCursorRange::two(egui::text::CCursor::new(start), egui::text::CCursor::new(end));
        state.cursor.set_char_range(Some(range));
        state.store(ctx, field);
    }
}

/// The char index at `units` UTF-16 units into `text` (its end past that).
fn char_at(text: &str, units: usize) -> usize {
    let mut counted = 0;
    for (i, c) in text.chars().enumerate() {
        if counted >= units {
            return i;
        }
        counted += c.len_utf16();
    }
    text.chars().count()
}

/// The UTF-16 offset of char index `chars` in `text` (its length past the end).
fn utf16_at(text: &str, chars: usize) -> usize {
    text.chars().take(chars).map(char::len_utf16).sum()
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

/// The view's text as the field takes it: no line breaks in a one-line field, and no control
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

    /// The system's views: a dialog that answers `answer` once polled `after` times, and a
    /// mirroring view that shows, in turn, each of `views` (one per poll, the last one staying).
    struct Fake {
        answer: Outcome,
        after: usize,
        opened: Vec<Request>,
        polls: usize,
        mirrors: Vec<Request>,
        views: Vec<MirrorPoll>,
        selected: Vec<(usize, usize)>,
        reshown: usize,
        closed: usize,
        keyboard: Vec<bool>,
    }

    impl Fake {
        fn new(answer: Outcome) -> Self {
            Self {
                answer,
                after: 2,
                opened: Vec::new(),
                polls: 0,
                mirrors: Vec::new(),
                views: Vec::new(),
                selected: Vec::new(),
                reshown: 0,
                closed: 0,
                keyboard: Vec::new(),
            }
        }

        fn mirror(views: Vec<MirrorPoll>) -> Self {
            Self { views, ..Self::new(Outcome::Cancelled) }
        }
    }

    impl Host for Fake {
        type Dialog = ();
        type Mirror = ();
        fn open(&mut self, request: Request) {
            self.opened.push(request);
        }
        fn poll(&mut self, _: &()) -> Option<Outcome> {
            self.polls += 1;
            (self.polls > self.after).then(|| self.answer.clone())
        }
        fn open_mirror(&mut self, request: Request) {
            self.mirrors.push(request);
        }
        fn poll_mirror(&mut self, _: &()) -> MirrorPoll {
            if self.views.len() > 1 { self.views.remove(0) } else { self.views.first().cloned().unwrap_or(MirrorPoll::Opening) }
        }
        fn select_mirror(&mut self, _: &(), start: usize, end: usize) {
            self.selected.push((start, end));
        }
        fn reshow_mirror(&mut self, _: &()) {
            self.reshown += 1;
        }
        fn close_mirror(&mut self, _: ()) {
            self.closed += 1;
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
    /// the same pass (the shell calls it at the end of `App::ui`). `click` taps the field at a
    /// char index (the caret moves there). Returns what a read copied.
    fn frame(ctx: &egui::Context, app: &mut App, typing: &mut Typing<(), ()>, host: &mut Fake, focus: bool) -> Option<String> {
        frame_with(ctx, app, typing, host, focus, Vec::new())
    }

    /// [`frame`] with `events` (a tap) in its input.
    fn frame_with(
        ctx: &egui::Context,
        app: &mut App,
        typing: &mut Typing<(), ()>,
        host: &mut Fake,
        focus: bool,
        events: Vec<egui::Event>,
    ) -> Option<String> {
        let mut raw =
            egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 800.0))), events, ..Default::default() };
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

    fn run_with(app: &mut App, host: &mut Fake, frames: usize) -> Vec<String> {
        let ctx = egui::Context::default();
        let mut typing = Typing::default();
        let mut leaked = Vec::new();
        for i in 0..frames {
            if let Some(copy) = frame(&ctx, app, &mut typing, host, i < 2) {
                leaked.push(copy);
            }
        }
        leaked
    }

    fn app(text: &str, multiline: bool, password: bool) -> App {
        App { text: text.into(), multiline, password, enters: 0, id: egui::Id::new("field") }
    }

    fn view(text: &str, at: usize) -> MirrorPoll {
        MirrorPoll::Open(Some(Seen { text: text.into(), start: at, end: at, focused: true }))
    }

    #[test]
    fn typing_shows_in_the_field_as_it_goes() {
        let mut a = app("Facture", false, false);
        let mut host = Fake::mirror(vec![MirrorPoll::Opening, view("Facture", 7), view("Facture é", 9), view("Facture été", 11)]);
        let read = run_with(&mut a, &mut host, 14);
        assert_eq!(host.mirrors, [Request { text: "Facture".into(), password: false, multiline: false }]);
        assert_eq!(read, ["Facture"], "the read copy is handed to the view, not the clipboard");
        assert_eq!(a.text, "Facture été", "accents arrive whole, and in the field itself");
        assert_eq!(a.enters, 0, "still typing");
        assert!(host.opened.is_empty(), "no dialog");
        assert_eq!(host.closed, 0);
    }

    #[test]
    fn enter_in_a_one_line_field_is_done() {
        let mut a = app("Fac", false, false);
        let mut host = Fake::mirror(vec![view("Fac", 3), view("Facture", 7), view("Facture\n", 8)]);
        run_with(&mut a, &mut host, 14);
        assert_eq!(a.text, "Facture");
        assert_eq!(a.enters, 1, "Enter is the field's Done");
        assert_eq!(host.closed, 1, "and the keyboard goes");
        assert_eq!(host.mirrors.len(), 1, "it doesn't start again for the field it answered");
    }

    /// After Enter, a field that keeps the focus (the app gives it back) isn't typed in again until
    /// it is tapped.
    #[test]
    fn a_field_that_keeps_the_focus_after_enter_isnt_typed_in_again() {
        let ctx = egui::Context::default();
        let mut a = app("ab", false, false);
        let mut typing = Typing::default();
        let mut host = Fake::mirror(vec![view("ab", 2), view("ab\n", 3)]);
        for _ in 0..14 {
            frame(&ctx, &mut a, &mut typing, &mut host, true);
        }
        assert_eq!(a.text, "ab");
        assert_eq!(host.closed, 1, "Enter ended the typing");
        assert_eq!(host.mirrors.len(), 1, "and it didn't start again by itself");
    }

    /// A line break pasted into a one-line field is a space there, not its Done.
    #[test]
    fn a_line_break_pasted_in_a_one_line_field_is_a_space() {
        let mut a = app("", false, false);
        let mut host = Fake::mirror(vec![view("", 0), view("un\ndeux", 7)]);
        run_with(&mut a, &mut host, 12);
        assert_eq!(a.text, "un deux");
        assert_eq!(a.enters, 0);
        assert_eq!(host.closed, 0, "still typing");
    }

    /// The keyboard went (Back, its own button); a tap in the field brings it back.
    #[test]
    fn a_tap_in_the_field_brings_the_keyboard_back() {
        let ctx = egui::Context::default();
        let mut a = app("abc", false, false);
        let mut typing = Typing::default();
        let mut host = Fake::mirror(vec![view("abc", 3)]);
        for i in 0..4 {
            frame(&ctx, &mut a, &mut typing, &mut host, i < 2);
        }
        let at = ctx.read_response(a.id).map(|r| r.rect.center()).unwrap_or_default();
        let button =
            |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        frame_with(&ctx, &mut a, &mut typing, &mut host, false, vec![egui::Event::PointerMoved(at), button(true)]);
        frame_with(&ctx, &mut a, &mut typing, &mut host, false, vec![button(false)]);
        assert_eq!(host.reshown, 1);
        assert_eq!(host.mirrors.len(), 1, "the same view");
    }

    /// The view went with its window (the app went to the background): the field keeps what was
    /// typed, and the view isn't made again until the field is tapped.
    #[test]
    fn a_view_that_goes_away_leaves_the_field_as_typed() {
        let mut a = app("a", false, false);
        let mut host = Fake::mirror(vec![view("a", 1), view("ab", 2), view("ab", 2), MirrorPoll::Gone]);
        run_with(&mut a, &mut host, 12);
        assert_eq!(a.text, "ab");
        assert_eq!(host.closed, 1);
        assert_eq!(host.mirrors.len(), 1);
    }

    #[test]
    fn a_multiline_field_keeps_line_breaks() {
        let mut a = app("Ligne 1", true, false);
        let mut host = Fake::mirror(vec![view("Ligne 1", 7), view("Première\nseconde", 16)]);
        run_with(&mut a, &mut host, 14);
        assert!(host.mirrors.first().is_some_and(|r| r.multiline));
        assert_eq!(a.text, "Première\nseconde");
        assert_eq!(a.enters, 0);
    }

    /// The view's caret is the field's after typing; one moved in the field moves the view's.
    #[test]
    fn the_caret_follows_both_ways() {
        let ctx = egui::Context::default();
        let mut a = app("abc", false, false);
        let mut typing = Typing::default();
        let mut host = Fake::mirror(vec![view("abc", 3), view("abXc", 3)]);
        for i in 0..6 {
            frame(&ctx, &mut a, &mut typing, &mut host, i < 2);
        }
        assert_eq!(a.text, "abXc");
        assert_eq!(cursor_of(&ctx, a.id), Some((3, 3)), "after the X, where the view has it");
        // A finger puts the caret at the start of the field.
        set_cursor(&ctx, a.id, (0, 0));
        for _ in 0..3 {
            frame(&ctx, &mut a, &mut typing, &mut host, false);
        }
        assert_eq!(host.selected.last(), Some(&(0, 0)), "the view's caret follows");
    }

    #[test]
    fn leaving_the_field_puts_the_keyboard_away() {
        let ctx = egui::Context::default();
        let mut a = app("abc", false, false);
        let mut typing = Typing::default();
        let mut host = Fake::mirror(vec![view("abc", 3)]);
        for i in 0..4 {
            frame(&ctx, &mut a, &mut typing, &mut host, i < 2);
        }
        ctx.memory_mut(|m| m.surrender_focus(a.id));
        for _ in 0..3 {
            frame(&ctx, &mut a, &mut typing, &mut host, false);
        }
        assert_eq!(host.closed, 1);
        assert_eq!(a.text, "abc");
        // The window is drawn again for a moment, so the page takes back the keyboard's room.
        assert!(typing.settle_until > 0.0);
    }

    /// Two layers keep a password out of the dialog: egui doesn't copy a password field, and
    /// `frame` never passes one on. This pins the behaviour, whichever layer holds.
    #[test]
    fn a_password_is_typed_in_the_dialog_and_never_read_into_it() {
        let mut a = app("secret", false, true);
        let mut host = Fake::new(Outcome::Done("nouveau".into()));
        run_with(&mut a, &mut host, 12);
        assert_eq!(host.opened, [Request { text: String::new(), password: true, multiline: false }]);
        assert!(host.mirrors.is_empty(), "a password never shows in a mirroring view");
        assert_eq!(a.text, "nouveau");
        assert_eq!(a.enters, 1);
    }

    #[test]
    fn a_failed_view_falls_back_to_the_soft_keyboard_for_good() {
        let mut a = app("Facture", false, false);
        let mut host = Fake::mirror(vec![MirrorPoll::Failed]);
        run_with(&mut a, &mut host, 12);
        assert_eq!(a.text, "Facture");
        assert_eq!(host.keyboard.last(), Some(&true), "typing goes to the window's keyboard");
        assert_eq!(host.mirrors.len(), 1, "and the view isn't tried again");
    }

    #[test]
    fn utf16_and_char_offsets_meet() {
        // "é" is one unit, "😀" two.
        let text = "aé😀b";
        assert_eq!(char_at(text, 0), 0);
        assert_eq!(char_at(text, 2), 2);
        assert_eq!(char_at(text, 4), 3);
        assert_eq!(char_at(text, 99), 4, "past the end: the end");
        assert_eq!(utf16_at(text, 3), 4);
        assert_eq!(utf16_at(text, 9), 5);
    }

    #[test]
    fn line_breaks_and_control_characters_are_cleaned_for_the_field() {
        assert_eq!(field_text("a\nb\r\nc\u{7}", false), "a b  c");
        assert_eq!(field_text("a\nb\r\nc\u{7}\td", true), "a\nb\nc\td");
    }
}
