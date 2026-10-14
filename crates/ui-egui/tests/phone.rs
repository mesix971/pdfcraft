//! The phone layout (`phone.rs`) in the real shell: Home, the reader's bars and gestures, tool
//! modes, the tool drawer, Back, sheets and dialogs.

use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton, Pos2, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use pdfcraft_ui_egui::canvas::Fit;
use pdfcraft_ui_egui::phone::{Sheet, ToolMode};
use pdfcraft_ui_egui::{PdfCraftApp, QuickTool};

/// Three 300×400 pages of Helvetica text: taller than the window together, so they scroll.
const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >> endobj
4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >> endobj
5 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >> endobj
6 0 obj << /Length 50 >> stream
BT /F1 14 Tf 20 250 Td (The quick brown fox) Tj ET
endstream endobj
7 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
trailer << /Root 1 0 R >>
%%EOF";

/// The same three pages, the first with a link to the third over its top half.
const LINK_FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> /Annots [8 0 R] >> endobj
4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >> endobj
5 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] /Contents 6 0 R /Resources << /Font << /F1 7 0 R >> >> >> endobj
6 0 obj << /Length 50 >> stream
BT /F1 14 Tf 20 250 Td (The quick brown fox) Tj ET
endstream endobj
7 0 obj << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> endobj
8 0 obj << /Type /Annot /Subtype /Link /Rect [20 300 280 380] /Border [0 0 0] /Dest [5 0 R /Fit] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

/// A phone held upright (412 points wide, Material's common width), in the phone layout. Steps
/// are 50 ms, so two taps can make a double tap.
fn harness(open: bool) -> Harness<'static, PdfCraftApp> {
    harness_with(open.then_some(FIXTURE), |_| {})
}

fn harness_with(pdf: Option<&'static [u8]>, setup: impl FnOnce(&mut PdfCraftApp) + 'static) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(vec2(412.0, 892.0)).with_step_dt(0.05).build_eframe(move |_| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.set_option("shell", "phone").unwrap();
        if let Some(bytes) = pdf {
            app.open_bytes("text.pdf", None, bytes.to_vec()).unwrap();
        }
        setup(&mut app);
        app
    });
    h.run_steps(4);
    for _ in 0..200 {
        h.run_steps(2);
        if !h.state().render_pending() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.run_steps(4);
    h
}

/// A point on page `page` in PDF user space → screen (MediaBox 300×400).
fn at(h: &Harness<'static, PdfCraftApp>, page: usize, x: f32, y: f32) -> Pos2 {
    let r = h.state().views[0].page_screen_rect(page).expect("page on screen");
    pos2(r.left() + x / 300.0 * r.width(), r.top() + (400.0 - y) / 400.0 * r.height())
}

fn press(h: &Harness<'static, PdfCraftApp>, p: Pos2, pressed: bool) {
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
}

/// One tap, then long enough for it not to be the start of a double tap.
fn tap(h: &mut Harness<'static, PdfCraftApp>, p: Pos2) {
    h.hover_at(p);
    h.run_steps(1);
    press(h, p, true);
    h.run_steps(1);
    press(h, p, false);
    h.run_steps(12);
}

fn back(h: &mut Harness<'static, PdfCraftApp>) {
    h.key_press(Key::Escape);
    h.run_steps(3);
}

fn click(h: &mut Harness<'static, PdfCraftApp>, label: &str) {
    h.get_by_label(label).click();
    h.run_steps(4);
}

/// A finger drawn from `from` to `to` in a few moves.
fn finger_drag(h: &mut Harness<'static, PdfCraftApp>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.run_steps(1);
    press(h, from, true);
    h.run_steps(1);
    for k in 1..=6 {
        h.hover_at(from + (to - from) * (k as f32 / 6.0));
        h.run_steps(1);
    }
    press(h, to, false);
    h.run_steps(4);
}

#[test]
fn a_signature_drawn_with_a_finger_can_be_applied() {
    let mut h = harness(true);
    click(&mut h, "Fill & Sign");
    click(&mut h, "Fill & Sign: sign");
    assert_eq!(h.state().dialog, Some(pdfcraft_ui_egui::Dialog::Signature), "no signature yet: the pad opens");
    click(&mut h, "Draw");
    let hint = h.get_by_label("Draw your signature below.").rect();
    let width = 412.0;
    // The pad is just below its hint, as wide as the dialog.
    let y = hint.bottom() + 60.0;
    finger_drag(&mut h, pos2(80.0, y), pos2(300.0, y + 20.0));
    assert!(h.state().signature_draft.strokes.iter().any(|s| s.len() > 1), "the finger drew");
    let apply = h.get_by_label("Apply");
    let r = apply.rect();
    assert!(r.right() <= width && r.left() >= 0.0, "Apply is on screen: {r:?}");
    apply.click();
    h.run_steps(4);
    assert_eq!(h.state().dialog, None, "Apply closes the pad");
    assert!(h.state().signature.is_some(), "and keeps the signature");
    // A tap on the page signs there.
    let annotations = |h: &Harness<'static, PdfCraftApp>| h.state().session.docs().first().map_or(0, |d| d.info.annotations.len());
    assert_eq!(annotations(&h), 0);
    let p = at(&h, 0, 150.0, 150.0);
    tap(&mut h, p);
    assert_eq!(annotations(&h), 1, "the signature is on the page");
}

/// Every dialog keeps its controls on a phone's screen: a dialog scrolls down, never sideways, so
/// a button past the right edge can't be reached at all (the signature pad's Apply was).
#[test]
fn every_dialog_keeps_its_controls_on_a_phone_screen() {
    use pdfcraft_engine::MarkKind;
    use pdfcraft_ui_egui::{Dialog, PropsTab};
    let mut dialogs = vec![
        Dialog::CreateImages,
        Dialog::About,
        Dialog::Shortcuts,
        Dialog::Split,
        Dialog::NumberPages,
        Dialog::Protect,
        Dialog::PageBoxes,
        Dialog::Signature,
        Dialog::RedactPages,
        Dialog::RedactSearch,
        Dialog::RedactProps,
        Dialog::Print,
        Dialog::Revert,
        Dialog::Extract,
        Dialog::RotatePages,
        Dialog::RemoveHidden,
        Dialog::Sanitize,
        Dialog::SummarizeComments,
        Dialog::Optimize,
        Dialog::AccessibilityOptions,
        Dialog::RecognizeText,
        Dialog::JsConsole,
        Dialog::DocumentJs,
        Dialog::Preferences,
        Dialog::CompareFiles,
        Dialog::ActionWizard,
        Dialog::PdfA,
        Dialog::CreateStamp,
        Dialog::AuditSpace,
    ];
    for tab in [PropsTab::Description, PropsTab::InitialView, PropsTab::Security, PropsTab::Fonts, PropsTab::Advanced] {
        dialogs.push(Dialog::Properties(tab));
    }
    for kind in [MarkKind::HeaderFooter, MarkKind::Watermark, MarkKind::Background] {
        dialogs.push(Dialog::Marks(kind));
    }
    // Opened the way the menus open them first: a command seeds the dialog's draft (the
    // editable fields Properties shows, the page range), which a bare dialog doesn't have.
    // (Revert and Summarize comments open only for a changed file and one with comments; they
    // are in the list above.)
    let commands = [
        "file.properties",
        "protect.properties",
        "page.number",
        "edit.header_footer",
        "edit.watermark",
        "edit.background",
        "export.image",
        "export.text",
        "export.all_images",
        "ocr.recognize",
        "tools.js_console",
        "tools.document_js",
        "app.preferences",
        "a11y.reading_options",
        "redact.pages",
        "redact.search",
        "redact.properties",
        "redact.sanitize",
        "optimize.advanced",
        "doc.compare",
        "standards.pdfa",
        "sign.fill.signature",
        "page.rotate_dialog",
        "page.split",
        "help.shortcuts",
        "help.about",
    ];
    // Controls, and text too: a line too long to wrap is cut off, and widens the whole dialog,
    // which pushes its buttons off the screen.
    // Controls, and text too: a line too long to wrap is cut off, and widens the whole dialog,
    // which pushes its buttons off the screen.
    let containers = [Role::Window, Role::GenericContainer, Role::ScrollView, Role::Group, Role::Unknown, Role::ScrollBar];
    // The narrowest common phone (360 points), in languages whose words run long.
    const WIDTH: f32 = 360.0;
    let mut off_screen = Vec::new();
    let mut silent = Vec::new();
    let mut checked = 0;
    for lang in ["en", "fr", "de", "ru"] {
        let mut h = Harness::builder().with_size(vec2(WIDTH, 780.0)).with_step_dt(0.05).build_eframe(move |_| {
            let mut app = PdfCraftApp::new();
            app.set_option("language", lang).unwrap();
            app.set_option("shell", "phone").unwrap();
            app.open_bytes("text.pdf", None, FIXTURE.to_vec()).unwrap();
            app
        });
        h.run_steps(8);
        let mut check = |h: &mut Harness<'static, PdfCraftApp>, what: &str| {
            checked += 1;
            for node in h.query_all_by(|n| !containers.contains(&n.role())) {
                let r = node.rect();
                if r.right() > WIDTH + 0.5 || r.left() < -0.5 {
                    let n = node.accesskit_node();
                    let name = n.label().or_else(|| n.value()).unwrap_or_default();
                    off_screen.push(format!("{lang} {what}: {:?} “{name}” at x {:.0}–{:.0}", n.role(), r.left(), r.right()));
                }
            }
        };
        for id in commands {
            h.state_mut().execute(id);
            h.run_steps(6);
            let Some(dialog) = h.state().dialog else {
                silent.push(id);
                continue;
            };
            check(&mut h, &format!("{id} ({dialog:?})"));
            if let Dialog::Properties(_) = dialog {
                // Its other tabs, as the tab row switches to them.
                for tab in [PropsTab::Description, PropsTab::InitialView, PropsTab::Security, PropsTab::Fonts, PropsTab::Advanced] {
                    h.state_mut().dialog = Some(Dialog::Properties(tab));
                    h.run_steps(4);
                    check(&mut h, &format!("{id} ({tab:?})"));
                }
            }
            h.state_mut().dialog = None;
            h.run_steps(3);
        }
        for dialog in dialogs.iter().copied() {
            h.state_mut().dialog = Some(dialog);
            h.run_steps(6);
            if h.state().dialog != Some(dialog) {
                // Closed itself: it needs something this document hasn't got.
                continue;
            }
            check(&mut h, &format!("{dialog:?}"));
            h.state_mut().dialog = None;
            h.run_steps(3);
        }
    }
    assert!(off_screen.is_empty(), "controls past the screen's edge:\n{}", off_screen.join("\n"));
    assert!(silent.is_empty(), "commands that opened no dialog: {silent:?}");
    // Every command's dialog and most of the bare ones were looked at, in every language.
    assert!(checked >= 4 * (commands.len() + 20), "only {checked} dialogs checked");
}

#[test]
fn home_offers_open_files_and_lists_open_documents() {
    let mut h = harness(false);
    // Nothing opened yet: Open file on the page itself, and in the "+" sheet.
    assert_eq!(h.query_all_by_label("Open file").count(), 1);
    click(&mut h, "Open…");
    assert_eq!(h.state().phone_sheet(), Some(Sheet::New));
    assert_eq!(h.query_all_by_label("Open file").count(), 2);
    back(&mut h);
    assert_eq!(h.state().phone_sheet(), None, "Back closes the sheet");

    let mut h = harness(true);
    assert_eq!(h.state().active, Some(0), "an opened document shows in the reader");
    click(&mut h, "Back");
    assert_eq!(h.state().active, None, "Back goes Home");
    assert_eq!(h.state().views.len(), 1, "and keeps the document open there");
    click(&mut h, "text.pdf");
    assert_eq!(h.state().active, Some(0));
}

#[test]
fn a_tap_hides_the_bars_and_another_shows_them() {
    let mut h = harness(true);
    assert!(h.state().phone_bars_shown());
    h.get_by_label("Menu");
    let p = at(&h, 0, 150.0, 120.0);
    tap(&mut h, p);
    assert!(!h.state().phone_bars_shown());
    assert!(h.query_by_label("Menu").is_none(), "hidden bars aren't drawn");
    tap(&mut h, p);
    assert!(h.state().phone_bars_shown());
    h.get_by_label("Menu");
}

#[test]
fn a_finger_drag_scrolls_the_pages_instead_of_selecting_text() {
    let mut h = harness(true);
    let before = h.state().views[0].page_screen_rect(0).unwrap();
    let from = at(&h, 0, 60.0, 254.0);
    let to = from - vec2(0.0, 300.0);
    h.hover_at(from);
    h.run_steps(1);
    press(&h, from, true);
    h.run_steps(1);
    for k in 1..=6 {
        h.hover_at(from + (to - from) * (k as f32 / 6.0));
        h.run_steps(1);
    }
    press(&h, to, false);
    h.run_steps(12);
    let after = h.state().views[0].page_screen_rect(0).unwrap();
    assert!(before.top() - after.top() > 200.0, "the pages followed the finger: {before:?} → {after:?}");
    assert_eq!(h.state().views[0].selected_text(), None, "no text was selected");
    assert!(h.state().phone_bars_shown(), "a drag isn't a tap");
}

#[test]
fn a_double_tap_zooms_in_and_another_goes_back_to_the_page_width() {
    let mut h = harness(true);
    let fit_zoom = h.state().views[0].zoom;
    assert_eq!(h.state().views[0].fit, Fit::Width);
    let p = at(&h, 0, 150.0, 120.0);
    let double = |h: &mut Harness<'static, PdfCraftApp>| {
        h.hover_at(p);
        h.run_steps(1);
        for _ in 0..2 {
            press(h, p, true);
            h.run_steps(1);
            press(h, p, false);
            h.run_steps(1);
        }
        h.run_steps(12);
    };
    double(&mut h);
    let v = &h.state().views[0];
    assert_eq!(v.fit, Fit::None);
    assert!(v.zoom > fit_zoom * 2.0, "{} after {fit_zoom}", v.zoom);
    assert!(h.state().phone_bars_shown(), "the double tap's first tap doesn't hide the bars");
    double(&mut h);
    let v = &h.state().views[0];
    assert_eq!(v.fit, Fit::Width);
    assert!((v.zoom - fit_zoom).abs() < 1e-3, "{} instead of {fit_zoom}", v.zoom);
}

#[test]
fn a_double_tap_on_text_zooms_without_selecting_it() {
    let mut h = harness(true);
    let p = at(&h, 0, 60.0, 254.0);
    h.hover_at(p);
    h.run_steps(1);
    for _ in 0..2 {
        press(&h, p, true);
        h.run_steps(1);
        press(&h, p, false);
        h.run_steps(1);
    }
    h.run_steps(12);
    assert_eq!(h.state().views[0].fit, Fit::None, "zoomed in");
    assert_eq!(h.state().views[0].selected_text(), None, "the word isn't selected as a desktop double click would");
}

#[test]
fn a_long_press_selects_the_word_under_the_finger() {
    let mut h = harness(true);
    let p = at(&h, 0, 60.0, 254.0);
    let touch = |h: &Harness<'static, PdfCraftApp>, phase| {
        h.event(Event::Touch { device_id: egui::TouchDeviceId(1), id: egui::TouchId(1), phase, pos: p, force: None });
    };
    h.hover_at(p);
    touch(&h, egui::TouchPhase::Start);
    press(&h, p, true);
    h.run_steps(24);
    press(&h, p, false);
    touch(&h, egui::TouchPhase::End);
    h.run_steps(4);
    assert_eq!(h.state().views[0].selected_text().as_deref(), Some("quick"));
}

#[test]
fn a_tool_mode_keeps_its_tool_until_done() {
    let mut h = harness(true);
    assert!(!h.state().comment_prefs.pinned);
    click(&mut h, "Comments");
    assert_eq!(h.state().phone_tool(), Some(ToolMode::Comment));
    assert_eq!(h.state().quick_tool, QuickTool::Comment(pdfcraft_ui_egui::comments::CommentTool::Highlight), "highlighting comes first");
    assert!(h.state().comment_prefs.pinned, "a mode keeps its tool after each use");
    assert_eq!(h.state().right, None, "no Comments panel covers the page the tool is for");
    click(&mut h, "Underline");
    assert_eq!(h.state().quick_tool, QuickTool::Comment(pdfcraft_ui_egui::comments::CommentTool::Underline));
    assert_eq!(h.state().right, None, "nor after another tool");
    click(&mut h, "Done");
    assert_eq!(h.state().phone_tool(), None);
    assert_eq!(h.state().quick_tool, QuickTool::Select);
    assert!(!h.state().comment_prefs.pinned, "Done gives the pin back");
}

#[test]
fn back_closes_one_thing_at_a_time_and_never_leaves_home() {
    let mut h = harness(true);
    click(&mut h, "Menu");
    assert_eq!(h.state().phone_sheet(), Some(Sheet::Menu));
    back(&mut h);
    assert_eq!(h.state().phone_sheet(), None);
    assert_eq!(h.state().active, Some(0));
    click(&mut h, "Draw");
    assert_eq!(h.state().phone_tool(), Some(ToolMode::Draw));
    back(&mut h);
    assert_eq!(h.state().phone_tool(), None);
    assert_eq!(h.state().quick_tool, QuickTool::Select);
    assert_eq!(h.state().active, Some(0));
    click(&mut h, "Edit");
    assert_eq!(h.state().phone_tool(), Some(ToolMode::Edit));
    back(&mut h);
    assert_eq!(h.state().phone_tool(), None, "Back leaves Edit");
    assert_eq!(h.state().active, Some(0));
    click(&mut h, "Tools");
    click(&mut h, "Protect a PDF");
    assert!(h.state().phone_drawer());
    back(&mut h);
    assert!(!h.state().phone_drawer(), "Back closes the tool drawer");
    assert_eq!(h.state().active, Some(0));
    click(&mut h, "Search");
    back(&mut h);
    assert!(h.state().views[0].find.is_none(), "Back closes the search");
    assert_eq!(h.state().active, Some(0));
    back(&mut h);
    assert_eq!(h.state().active, None, "then goes Home");
    back(&mut h);
    assert_eq!(h.state().active, None);
    assert_eq!(h.state().views.len(), 1);
}

#[test]
fn the_page_pill_goes_to_a_page() {
    let mut h = harness(true);
    click(&mut h, "Page 1 of 3");
    assert_eq!(h.state().phone_sheet(), Some(Sheet::GoTo));
    h.get_by_role(Role::TextInput).click();
    h.run_steps(2);
    h.event(Event::Key { key: Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND });
    h.event(Event::Text("3".into()));
    h.run_steps(2);
    click(&mut h, "OK");
    h.run_steps(6);
    assert_eq!(h.state().phone_sheet(), None);
    assert_eq!(h.state().views[0].current, 2);
}

#[test]
fn every_tool_opens_in_the_drawer_under_the_page() {
    let mut h = harness(true);
    h.state_mut().phone_screen = true;
    click(&mut h, "Tools");
    assert_eq!(h.state().phone_sheet(), Some(Sheet::Tools));
    click(&mut h, "Protect a PDF");
    assert!(h.state().phone_drawer());
    assert!(h.state().phone_layout(), "a phone never gets the desktop layout");
    h.get_by_label("Protect with password");
    let pages = h.state().views[0].viewport_rect();
    assert!(pages.bottom() < 892.0 * 0.7, "the pages stay above the drawer: {pages:?}");
    click(&mut h, "Done");
    assert!(!h.state().phone_drawer());
    assert_eq!(h.state().quick_tool, QuickTool::Select);
}

#[test]
fn a_dialog_is_a_screen_of_its_own_on_a_phone() {
    let mut h = harness(true);
    assert!(h.state_mut().execute("app.preferences"));
    h.run_steps(4);
    let card: egui::Rect = h.ctx.data(|d| d.get_temp(egui::Id::new("dialog").with("card"))).expect("the dialog's card");
    assert!(card.left() >= -0.5 && card.right() <= 412.5, "{card:?} stays on a 412-point screen");
    assert!(card.height() > 892.0 * 0.8, "{card:?} takes the screen");
}
/// A finger's tap as Android delivers it: the release and `PointerGone` in one frame, through
/// the app's raw-input step. kittest runs neither that step nor two queued events in one frame
/// (`Harness::event` gives each its own), so the frames are built here.
fn finger_tap(h: &mut Harness<'static, PdfCraftApp>, p: Pos2) {
    let button = |pressed| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    let mut pending = false;
    for events in [vec![Event::PointerMoved(p), button(true)], vec![button(false), Event::PointerGone], Vec::new()] {
        let mut raw = egui::RawInput { events, ..Default::default() };
        pdfcraft_ui_egui::touch::defer_pointer_gone(&mut raw, &mut pending);
        h.input_mut().events.extend(raw.events);
        h.run_steps(1);
    }
    h.run_steps(12);
}

#[test]
fn a_finger_tap_on_a_link_follows_it() {
    let mut h = harness_with(Some(LINK_FIXTURE), |_| {});
    assert_eq!(h.state().views[0].current, 0);
    let p = at(&h, 0, 150.0, 340.0);
    finger_tap(&mut h, p);
    h.run_steps(6);
    assert_eq!(h.state().views[0].current, 2, "the link went to page 3");
    assert!(h.state().phone_bars_shown(), "a tap on a link doesn't hide the bars");
}

#[test]
fn back_after_a_long_press_only_clears_the_selection() {
    let mut h = harness(true);
    h.state_mut().views[0].select_text(0, 4, 8);
    h.run_steps(2);
    assert!(h.state().views[0].selected_text().is_some());
    back(&mut h);
    assert_eq!(h.state().views[0].selected_text(), None);
    assert_eq!(h.state().active, Some(0), "one press, one thing: the document stays open");
}

#[test]
fn a_notice_shows_below_the_top_bar_and_takes_its_taps() {
    let mut h = harness(true);
    h.state_mut().views[0].disk_changed = true;
    h.run_steps(4);
    let notice = h.get_by_label_contains("This file changed on disk").rect();
    assert!(notice.top() >= 56.0 - 0.5, "{notice:?} is under the top bar");
    let dismiss = h.get_by_label("Dismiss").rect();
    assert!(dismiss.right() <= 412.0, "{dismiss:?} is on screen: the message wraps beside it");
    h.get_by_label("Dismiss").click();
    h.run_steps(4);
    assert!(!h.state().views[0].disk_changed, "the bar didn't take the tap");
}

#[test]
fn a_tool_panel_left_open_in_the_desktop_layout_doesnt_stop_scrolling() {
    let mut h = harness_with(Some(FIXTURE), |app| app.set_option("tool", "edit").unwrap());
    let before = h.state().views[0].page_screen_rect(0).unwrap();
    let from = at(&h, 0, 150.0, 120.0);
    let to = from - vec2(0.0, 300.0);
    h.hover_at(from);
    h.run_steps(1);
    press(&h, from, true);
    h.run_steps(1);
    for k in 1..=6 {
        h.hover_at(from + (to - from) * (k as f32 / 6.0));
        h.run_steps(1);
    }
    press(&h, to, false);
    h.run_steps(12);
    let after = h.state().views[0].page_screen_rect(0).unwrap();
    assert!(before.top() - after.top() > 200.0, "the pages followed the finger: {before:?} → {after:?}");
}

/// Whether some text on screen contains `part`.
fn shows(h: &Harness<'static, PdfCraftApp>, part: &str) -> bool {
    h.query_all_by(|n| n.value().is_some_and(|v| v.contains(part))).next().is_some()
}

/// The assistant asks for a key first, and refuses what it can't send before sending anything:
/// this test never reaches the network (a 3-page document over a 1-page limit).
#[test]
fn the_assistant_asks_for_a_key_and_says_why_it_cant_answer() {
    let mut h = harness(true);
    click(&mut h, "AI assistant");
    h.get_by_label("Save");
    assert!(shows(&h, "with your own API key"), "it says what is sent, and to whom, before any key");
    h.state_mut().ai.key = "sk-ant-test".into();
    h.state_mut().ai.max_pages = 1;
    h.run_steps(2);
    click(&mut h, "Summarize");
    for _ in 0..200 {
        h.run_steps(1);
        if !h.state().ai.busy() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.run_steps(2);
    assert!(!h.state().ai.busy());
    assert!(shows(&h, "the assistant reads at most 1"), "why it couldn't, with the way out");
    back(&mut h);
    assert_eq!(h.state().active, Some(0), "Back closes the assistant, not the document");
    h.get_by_label("Menu");
}

// ---------------------------------------------------------------------------------------------
// Home

/// A folder of its own for a test's files, emptied first.
fn folder(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("pdfcraft-home-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run frames until `done`, for at most five seconds (a worker thread's answer).
fn wait(h: &mut Harness<'static, PdfCraftApp>, done: impl Fn(&PdfCraftApp) -> bool) {
    for _ in 0..500 {
        h.run_steps(1);
        if done(h.state()) {
            h.run_steps(2);
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("timed out");
}

#[test]
fn home_greets_by_name_and_the_profile_sets_it() {
    let mut h = harness_with(None, |app| app.comment_prefs.author = "Guest".into());
    // "Guest" is the system's default, not a name: the field starts empty.
    assert!(shows(&h, "Welcome to PdfCraft"), "no name yet");
    assert!(shows(&h, "Make PdfCraft yours"), "a tip offers to add it");
    click(&mut h, "Profile");
    let field = h.get_by_role_and_label(Role::TextInput, "Your name");
    field.focus();
    // A frame between the words: the space typed last stays in the field (the name kept is
    // trimmed, the field isn't).
    field.type_text("Ana ");
    h.run_steps(2);
    h.get_by_role_and_label(Role::TextInput, "Your name").type_text("Diaz ");
    h.run_steps(2);
    back(&mut h);
    back(&mut h);
    // By first name; the whole name signs comments.
    assert!(shows(&h, "Hello, Ana") && !shows(&h, "Hello, Ana Diaz"));
    assert!(!shows(&h, "Make PdfCraft yours"), "the tip has done its job");
    assert_eq!(h.state().comment_prefs.author, "Ana Diaz", "the same name signs comments");
}

#[test]
fn a_tip_can_be_dismissed_for_good() {
    let mut h = harness(false);
    assert!(shows(&h, "Your AI assistant"));
    // Each ✕ says which tip it dismisses.
    let dismiss = |h: &Harness<'static, PdfCraftApp>| h.query_all_by(|n| n.label().is_some_and(|l| l.starts_with("Dismiss · "))).count();
    let before = dismiss(&h);
    h.get_by_label("Dismiss · Your AI assistant").click();
    h.run_steps(3);
    assert_eq!(dismiss(&h), before - 1);
    assert!(h.state().home_tips_dismissed.iter().any(|t| t == "ai"));
    // And it stays dismissed after a restart.
    let saved = h.state().persist();
    let mut again = PdfCraftApp::new();
    again.restore(&saved);
    assert_eq!(again.home_tips_dismissed, h.state().home_tips_dismissed);
}

#[test]
fn a_quick_action_opens_a_file_then_its_tool() {
    let dir = folder("quick");
    let path = dir.join("form.pdf");
    std::fs::write(&path, FIXTURE).unwrap();
    let file = path.to_string_lossy().into_owned();
    let mut h = harness_with(None, move |app| app.pick_override = Some(vec![file]));
    click(&mut h, "Edit a PDF");
    wait(&mut h, |app| app.active.is_some());
    assert_eq!(h.state().phone_tool(), Some(ToolMode::Edit), "the file opens ready to edit");
    // With a document open, an action asks which one first.
    back(&mut h);
    back(&mut h);
    assert_eq!(h.state().active, None);
    click(&mut h, "Fill & Sign");
    assert_eq!(h.state().phone_sheet(), Some(Sheet::Pick));
    // The sheet's row (Home's list has one too, under the sheet).
    h.query_all_by_label("form.pdf").last().unwrap().click();
    h.run_steps(4);
    assert_eq!(h.state().active, Some(0));
    assert_eq!(h.state().phone_tool(), Some(ToolMode::FillSign));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn files_are_starred_listed_from_the_device_and_previewed() {
    let dir = folder("files");
    std::fs::write(dir.join("first.pdf"), FIXTURE).unwrap();
    std::fs::create_dir_all(dir.join("Reports")).unwrap();
    std::fs::write(dir.join("Reports").join("second.pdf"), FIXTURE).unwrap();
    std::fs::write(dir.join("notes.txt"), b"not a pdf").unwrap();
    let first = dir.join("first.pdf").to_string_lossy().into_owned();
    let folders = vec![dir.clone()];
    let opened = first.clone();
    let mut h = harness_with(None, move |app| {
        app.device_folders = folders;
        app.open_path(&opened);
    });
    click(&mut h, "Back");
    assert_eq!(h.state().active, None);
    // Its first page is previewed.
    let previewed = first.clone();
    wait(&mut h, move |app| app.home_thumbnail_ready(&previewed));
    // Starred from its ⋮ menu.
    click(&mut h, "More for first.pdf");
    assert_eq!(h.state().phone_sheet(), Some(Sheet::Item));
    click(&mut h, "Add to Starred");
    assert_eq!(h.state().starred, vec![first.clone()]);
    click(&mut h, "Starred");
    assert!(h.query_by_label("first.pdf").is_some(), "listed under Starred");
    // The device's PDFs, the subfolder's too, nothing else.
    click(&mut h, "On this device");
    wait(&mut h, |app| app.home_device_files().len() == 2);
    assert!(h.query_by_label("second.pdf").is_some());
    assert!(h.query_by_label("notes.txt").is_none());
    // A file from the list opens.
    click(&mut h, "second.pdf");
    assert_eq!(h.state().views.len(), 2);
    assert!(h.state().active.is_some());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_tabs_along_the_bottom_switch_and_back_returns_home() {
    let mut h = harness(false);
    click(&mut h, "Files");
    assert!(shows(&h, "PDFs on this device"));
    click(&mut h, "Create");
    assert!(h.query_by_label("Combine files").is_some(), "what can be created");
    click(&mut h, "Tools");
    assert!(h.query_by_label("Edit a PDF").is_some(), "every tool group");
    back(&mut h);
    assert!(shows(&h, "What would you like to do?"), "Back goes to Home's first tab");
    back(&mut h);
    assert!(shows(&h, "What would you like to do?"), "and never leaves Home");
}

#[test]
fn search_finds_files_by_name() {
    let mut h = harness(true);
    click(&mut h, "Back");
    click(&mut h, "Search");
    let field = h.get_by_role_and_label(Role::TextInput, "Search files");
    field.focus();
    field.type_text("TEXT");
    h.run_steps(3);
    assert!(h.query_by_label("text.pdf").is_some(), "found whatever the case");
    h.get_by_role_and_label(Role::TextInput, "Search files").type_text("zzz");
    h.run_steps(3);
    assert!(h.query_by_label("text.pdf").is_none());
    assert!(shows(&h, "No files match."));
    // The first Back leaves the field (as Android's closes the keyboard), the next ends the search.
    back(&mut h);
    back(&mut h);
    assert!(shows(&h, "What would you like to do?"), "Back ends the search");
}

#[test]
fn the_profile_photo_is_kept_with_the_settings() {
    let mut picture = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(400, 300, image::Rgba([10, 120, 90, 255])).write_to(&mut picture, image::ImageFormat::Png).unwrap();
    let png = pdfcraft_engine::signature_image::profile_picture(picture.get_ref()).unwrap();
    let mut app = PdfCraftApp::new();
    app.profile_photo = Some(std::sync::Arc::new(png.clone()));
    app.starred = vec!["/docs/a.pdf".into()];
    let mut again = PdfCraftApp::new();
    again.restore(&app.persist());
    assert_eq!(again.profile_photo.as_deref(), Some(&png));
    assert_eq!(again.starred, app.starred);
    // Settings are untrusted: what isn't a photo is dropped, and only files are starred, once.
    let mut tampered = PdfCraftApp::new();
    tampered.restore(r#"{"profile_photo": "bm90IGEgcGljdHVyZQ==", "starred": ["doc:0", "/a.pdf", "", "/a.pdf", 7]}"#);
    assert_eq!(tampered.profile_photo, None);
    assert_eq!(tampered.starred, ["/a.pdf"]);
}

#[test]
fn a_photo_is_chosen_from_a_picture_and_a_wrong_file_says_why() {
    let dir = folder("photo");
    let picture = dir.join("me.png");
    image::RgbaImage::from_pixel(640, 480, image::Rgba([200, 40, 40, 255])).save(&picture).unwrap();
    std::fs::write(dir.join("notes.txt"), b"not a picture").unwrap();
    let photos = vec![dir.clone()];
    let mut h = harness_with(None, move |app| app.photo_folders = photos);
    click(&mut h, "Profile");
    // The device's photos, in a grid (not a folder to find them in).
    h.query_all_by_label("Choose a photo").last().unwrap().click();
    for _ in 0..500 {
        h.run_steps(1);
        if h.query_by_label("me.png").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(h.query_by_label("notes.txt").is_none(), "pictures only");
    click(&mut h, "me.png");
    // Made small on a thread: the screen goes on meanwhile, back on the profile.
    wait(&mut h, |app| app.profile_photo.is_some());
    assert!(h.query_by_role_and_label(Role::TextInput, "Your name").is_some());
    assert_eq!(
        pdfcraft_engine::signature_image::profile_picture_pixels(h.state().profile_photo.as_ref().unwrap()).map(|(w, h, _)| (w, h)),
        Some((256, 256))
    );
    // Other folders: a file that isn't a picture is refused in plain words, and the photo stays.
    let text = dir.join("notes.png");
    std::fs::write(&text, b"not a picture").unwrap();
    h.state_mut().pick_override = Some(vec![text.to_string_lossy().into_owned()]);
    h.query_all_by_label("Choose a photo").last().unwrap().click();
    h.run_steps(2);
    click(&mut h, "Other folders…");
    wait(&mut h, |app| app.toast.is_some());
    assert_eq!(h.state().toast.as_ref().map(|t| t.0.as_str()), Some("Choose a PNG or JPEG picture."));
    assert!(h.state().profile_photo.is_some());
    // A damaged picture says what's wrong with it, not with a "signature image".
    let damaged = dir.join("damaged.png");
    std::fs::write(&damaged, b"\x89PNG\r\n\x1a\n and then nothing that decodes").unwrap();
    h.state_mut().toast = None;
    h.state_mut().pick_override = Some(vec![damaged.to_string_lossy().into_owned()]);
    h.query_all_by_label("Choose a photo").last().unwrap().click();
    h.run_steps(2);
    click(&mut h, "Other folders…");
    wait(&mut h, |app| app.toast.is_some());
    let said = h.state().toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
    assert!(said.starts_with("Couldn't use this picture") && !said.contains("signature"), "{said}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A tip is one big button: a tap anywhere on it (not only on its link) does what it offers.
#[test]
fn a_tip_card_is_tapped_anywhere() {
    // With a key, the assistant's tip is gone and this one comes first.
    let mut h = harness_with(None, |app| {
        app.comment_prefs.author = "Guest".into();
        app.ai.key = "sk-ant-test".into();
    });
    // On its words, away from its link and its ✕.
    let words = h.get_by_role_and_label(Role::Label, "Make PdfCraft yours").rect().center();
    let link = h.get_by_label("Add my name").rect();
    assert!(!link.contains(words));
    tap(&mut h, words);
    assert!(h.query_by_role_and_label(Role::TextInput, "Your name").is_some(), "the profile opened");
}

/// With a touch screen, taps in the rows that swipe (the tips, the quick actions) are still taps.
#[test]
fn taps_in_the_swiping_rows_reach_their_buttons() {
    let mut h = harness(false);
    for phase in [egui::TouchPhase::Start, egui::TouchPhase::End] {
        h.event(Event::Touch { device_id: egui::TouchDeviceId(0), id: egui::TouchId(0), phase, pos: pos2(4.0, 4.0), force: None });
    }
    h.run_steps(2);
    let set_up = h.get_by_label("Set up").rect().center();
    tap(&mut h, set_up);
    h.run_steps(3);
    assert!(h.query_by_label("Save").is_some(), "the tip's link opened the assistant's settings");
}

/// With a touch screen, a quick action tapped with a finger does what it says (with a document
/// open, it asks which one).
#[test]
fn a_quick_action_is_tapped_with_a_finger() {
    let mut h = harness(true);
    click(&mut h, "Back");
    for phase in [egui::TouchPhase::Start, egui::TouchPhase::End] {
        h.event(Event::Touch { device_id: egui::TouchDeviceId(0), id: egui::TouchId(0), phase, pos: pos2(4.0, 4.0), force: None });
    }
    h.run_steps(2);
    let tile = h.get_by_label("Comments").rect().center();
    tap(&mut h, tile);
    assert_eq!(h.state().phone_sheet(), Some(Sheet::Pick));
}

#[test]
fn back_from_the_assistants_settings_shows_what_was_under_them() {
    let mut h = harness_with(Some(FIXTURE), |app| app.ai.key = "sk-ant-test".into());
    click(&mut h, "Back");
    // From the profile: Back shows the profile again, then Home.
    click(&mut h, "Profile");
    click(&mut h, "AI assistant");
    h.get_by_label("Save");
    back(&mut h);
    assert!(h.query_by_role_and_label(Role::TextInput, "Your name").is_some(), "the profile, under the settings");
    back(&mut h);
    assert!(shows(&h, "What would you like to do?"));
    // The settings closed for the assistant too: in the document, it shows the conversation.
    click(&mut h, "text.pdf");
    click(&mut h, "AI assistant");
    assert!(h.query_by_label("Summarize").is_some(), "the conversation, not the settings left open");
    assert!(h.query_by_label("Save").is_none());
    // Its settings over the conversation: its own ‹ closes them first, as Back does.
    click(&mut h, "Settings");
    h.get_by_label("Save");
    click(&mut h, "Back");
    assert!(h.query_by_label("Summarize").is_some(), "back to the conversation");
    click(&mut h, "Settings");
    back(&mut h);
    assert!(h.query_by_label("Summarize").is_some(), "Back does the same");
    back(&mut h);
    assert_eq!(h.state().active, Some(0));
    h.get_by_label("Menu");
}

#[test]
fn only_the_rows_on_screen_are_previewed() {
    let dir = folder("many");
    let files: Vec<String> = (0..30)
        .map(|i| {
            let path = dir.join(format!("file{i:02}.pdf"));
            std::fs::write(&path, FIXTURE).unwrap();
            path.to_string_lossy().into_owned()
        })
        .collect();
    let folders = vec![dir.clone()];
    let mut h = harness_with(None, move |app| app.device_folders = folders);
    click(&mut h, "Files");
    wait(&mut h, |app| app.home_device_files().len() == 30);
    h.run_steps(3);
    let asked = |app: &PdfCraftApp| files.iter().filter(|f| app.home_preview_asked(f)).cloned().collect::<Vec<_>>();
    let first = asked(h.state());
    assert!((1..=12).contains(&first.len()), "{} previews asked for about ten rows on screen", first.len());
    // Scrolled down, the rows coming into view ask for theirs.
    h.hover_at(pos2(200.0, 600.0));
    for _ in 0..12 {
        h.event(Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, -300.0),
            phase: egui::TouchPhase::Move,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(2);
    }
    assert!(asked(h.state()).iter().any(|f| !first.contains(f)), "rows scrolled into view are previewed");
    // And previews come back from the worker (for rows on screen: the ones scrolled past left
    // the queue).
    wait(&mut h, move |app| files.iter().any(|f| app.home_thumbnail_ready(f)));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_profile_shows_a_name_changed_elsewhere() {
    let mut h = harness_with(None, |app| app.comment_prefs.author = "Ana Diaz".into());
    click(&mut h, "Profile");
    let name = |h: &Harness<'static, PdfCraftApp>| h.get_by_role_and_label(Role::TextInput, "Your name").value().map(|v| v.to_string());
    assert_eq!(name(&h).as_deref(), Some("Ana Diaz"));
    // Changed in Preferences while the profile is open.
    h.state_mut().comment_prefs.author = "Bea Cruz".into();
    h.run_steps(2);
    assert_eq!(name(&h).as_deref(), Some("Bea Cruz"));
}

#[test]
fn a_wrong_home_tab_leaves_the_document_shown() {
    let mut h = harness(true);
    assert!(h.state_mut().set_option("home_tab", "nowhere").is_err());
    assert_eq!(h.state().active, Some(0));
}

/// A swipe up that starts on the quick actions moves the page (the row only scrolls sideways,
/// and egui gives a drag to the innermost scroll area); a swipe sideways moves the row.
#[test]
fn a_swipe_on_a_row_moves_the_page_or_the_row() {
    let dir = folder("swipe");
    for i in 0..30 {
        std::fs::write(dir.join(format!("file{i:02}.pdf")), FIXTURE).unwrap();
    }
    let folders = vec![dir.clone()];
    let mut h = harness_with(None, move |app| app.device_folders = folders);
    click(&mut h, "On this device");
    wait(&mut h, |app| app.home_device_files().len() == 30);
    // A touch screen: egui scrolls by dragging only where it has seen one.
    for phase in [egui::TouchPhase::Start, egui::TouchPhase::End] {
        h.event(Event::Touch { device_id: egui::TouchDeviceId(0), id: egui::TouchId(0), phase, pos: pos2(4.0, 4.0), force: None });
    }
    h.run_steps(2);
    let page = |h: &Harness<'static, PdfCraftApp>| h.get_by_label("Recent").rect().top();
    let row = |h: &Harness<'static, PdfCraftApp>| h.get_by_label("Comments").rect().left();
    let (page_before, row_before) = (page(&h), row(&h));
    let tile = h.get_by_label("Comments").rect().center();
    finger_drag(&mut h, tile, tile - vec2(0.0, 240.0));
    h.run_steps(10);
    assert!(page_before - page(&h) > 150.0, "the page moved up: {} → {}", page_before, page(&h));
    assert!((row(&h) - row_before).abs() < 2.0, "the row stayed put");
    // Sideways, the row moves and the page doesn't.
    let (page_now, row_now) = (page(&h), row(&h));
    let tile = h.get_by_label("Comments").rect().center();
    finger_drag(&mut h, tile, tile - vec2(150.0, 0.0));
    h.run_steps(10);
    assert!(row_now - row(&h) > 100.0, "the row moved left: {} → {}", row_now, row(&h));
    assert!((page(&h) - page_now).abs() < 2.0, "the page stayed put");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The profile chooses the theme (and the choice is kept, so a phone stops following the
/// system's), and turns the pages dark for reading at night.
#[test]
fn the_profile_chooses_the_theme_and_dark_pages() {
    let mut h = harness(false);
    click(&mut h, "Profile");
    h.get_by_role_and_label(Role::RadioButton, "Dark").click();
    h.run_steps(3);
    assert_eq!(h.state().theme_preference, pdfcraft_ui_egui::theme::ThemePreference::Dark);
    assert_eq!(h.state().theme, pdfcraft_ui_egui::theme::ThemeKind::Dark);
    assert!(h.state().theme_chosen);
    let saved = h.state().persist();
    let mut again = PdfCraftApp::new();
    again.restore(&saved);
    assert!(again.theme_chosen, "the choice is kept");
    assert_eq!(again.theme_preference, pdfcraft_ui_egui::theme::ThemePreference::Dark);
    assert!(!h.state().dark_pages());
    h.get_by_role_and_label(Role::CheckBox, "Dark pages").click();
    h.run_steps(3);
    assert!(h.state().dark_pages());
    assert_eq!(h.state().reading_colors, pdfcraft_render::ReadingColors::NIGHT);
    h.get_by_role_and_label(Role::CheckBox, "Dark pages").click();
    h.run_steps(3);
    assert!(!h.state().dark_pages(), "and back");
}

/// The reader's menu switches the dark mode and the dark pages while reading.
#[test]
fn the_readers_menu_switches_dark_mode_and_dark_pages() {
    let mut h = harness(true);
    // Low in the menu: scrolled to, as a finger would.
    let pick = |h: &mut Harness<'static, PdfCraftApp>, label: &str| {
        click(h, "Menu");
        h.get_by_label(label).scroll_to_me();
        h.run_steps(4);
        click(h, label);
    };
    pick(&mut h, "Dark mode");
    assert_eq!(h.state().theme, pdfcraft_ui_egui::theme::ThemeKind::Dark);
    pick(&mut h, "Dark pages");
    assert!(h.state().dark_pages());
}

/// The system's bars take the colours of the screen's top and bottom: Home's ground and tab bar,
/// the reader's bars (the page's surround when they hide), true black in the dark theme.
#[test]
fn the_system_bars_follow_the_screen() {
    use pdfcraft_ui_egui::theme::{ThemeKind, Tokens};
    let light = Tokens::for_kind(ThemeKind::Light);
    let mut h = harness(true);
    assert_eq!(h.state().phone_system_bars(), Some((light.chrome, light.chrome)), "the reader's bars");
    let p = at(&h, 0, 150.0, 120.0);
    tap(&mut h, p);
    assert_eq!(h.state().phone_system_bars(), Some((light.pasteboard, light.pasteboard)), "bars hidden: the page's surround");
    tap(&mut h, p);
    click(&mut h, "Back");
    assert_eq!(h.state().phone_system_bars(), Some((egui::Color32::from_rgb(0xf5, 0xf8, 0xf7), egui::Color32::WHITE)), "Home's ground and tab bar");
    click(&mut h, "Profile");
    assert_eq!(h.state().phone_system_bars(), Some((light.chrome, light.pasteboard)), "a screen with a top bar");
    h.state_mut().set_option("theme", "dark").unwrap();
    h.run_steps(3);
    // True black on a phone (an OLED screen's pixels off): the bars, and the ground under them.
    assert_eq!(h.state().phone_system_bars(), Some((egui::Color32::BLACK, egui::Color32::BLACK)), "true black in the dark theme");
    assert_eq!(pdfcraft_ui_egui::theme::Tokens::get(&h.ctx).pasteboard, egui::Color32::BLACK, "the area behind the pages too");
    click(&mut h, "Back");
    assert_eq!(h.state().phone_system_bars(), Some((egui::Color32::BLACK, egui::Color32::BLACK)), "Home's ground and tab bar");
    // The desktop keeps its dark greys (black there is harsher, and saves nothing).
    h.state_mut().set_option("shell", "auto").unwrap();
    h.run_steps(3);
    assert_eq!(pdfcraft_ui_egui::theme::Tokens::get(&h.ctx).pasteboard, Tokens::for_kind(ThemeKind::Dark).pasteboard);
}

/// Settings from before the choice was kept: a theme other than the system's was the user's
/// choice, and stays one. Dark pages switched off leave other reading colours alone.
#[test]
fn an_earlier_theme_choice_stays_and_dark_pages_leave_other_colours_alone() {
    let mut app = PdfCraftApp::new();
    app.restore(r#"{"theme": "Dark"}"#);
    assert!(app.theme_chosen);
    let mut app = PdfCraftApp::new();
    app.restore(r#"{"theme": "System"}"#);
    assert!(!app.theme_chosen, "the system's theme is what a phone follows anyway");
    app.set_reading_colors(pdfcraft_render::ReadingColors::WHITE_ON_BLACK);
    app.set_dark_pages(false);
    assert_eq!(app.reading_colors, pdfcraft_render::ReadingColors::WHITE_ON_BLACK);
    assert!(app.set_option("dark_pages", "maybe").is_err());
}

/// The texts of page 1's paragraphs, and the size of the one with `word` in it.
fn page_one(h: &Harness<'static, PdfCraftApp>, word: &str) -> (Vec<String>, Option<f64>) {
    let id = h.state().views[0].id;
    let blocks = h.state().session.get(id).expect("the document").text_blocks(0);
    let size = blocks.iter().find(|b| b.text.contains(word)).map(|b| b.size);
    (blocks.into_iter().map(|b| b.text).collect(), size)
}

/// Replace everything in the focused field with `text`.
fn retype(h: &mut Harness<'static, PdfCraftApp>, text: &str) {
    h.key_press_modifiers(Modifiers::COMMAND, Key::A);
    h.run_steps(1);
    h.event(Event::Text(text.to_owned()));
    h.run_steps(3);
}

/// Edit goes straight to changing the page: no list of tools first, the paragraphs ready for a
/// tap, which opens the text large on a screen of its own; Done changes the paragraph.
#[test]
fn edit_goes_straight_to_the_text_and_types_it_on_a_screen_of_its_own() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    assert_eq!(h.state().phone_tool(), Some(ToolMode::Edit));
    assert_eq!(h.state().quick_tool, QuickTool::EditText, "ready for a tap");
    assert!(!h.state().phone_drawer(), "no list of tools to go through");
    let p = at(&h, 0, 60.0, 254.0);
    tap(&mut h, p);
    assert!(h.state().phone_text_editing(), "the tap opened the paragraph");
    assert!(h.query_by_label("Larger text").is_some(), "with the formats over it");
    retype(&mut h, "The quick red fox");
    click(&mut h, "Larger text");
    click(&mut h, "Done");
    assert!(!h.state().phone_text_editing());
    h.run_steps(4);
    let (texts, size) = page_one(&h, "red");
    assert!(texts.iter().any(|t| t.contains("The quick red fox")), "{texts:?}");
    assert_eq!(size, Some(16.0), "14 points, one step larger");
    assert_eq!(h.state().quick_tool, QuickTool::EditText, "still editing, for the next paragraph");
    click(&mut h, "Undo");
    assert!(page_one(&h, "brown").0.iter().any(|t| t.contains("The quick brown fox")), "one undo takes it back");
}

/// Back keeps what was typed; ✕ throws it away.
#[test]
fn back_keeps_the_typing_and_cancel_throws_it_away() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    let p = at(&h, 0, 60.0, 254.0);
    tap(&mut h, p);
    retype(&mut h, "Kept by Back");
    // The field gives up the keyboard first (Android's own Back does that), then Back.
    h.ctx.memory_mut(|m| m.request_focus(egui::Id::NULL));
    h.run_steps(2);
    back(&mut h);
    h.run_steps(4);
    assert!(!h.state().phone_text_editing());
    assert!(page_one(&h, "Kept").0.iter().any(|t| t.contains("Kept by Back")));
    let p = at(&h, 0, 60.0, 254.0);
    tap(&mut h, p);
    retype(&mut h, "Thrown away");
    click(&mut h, "Cancel");
    h.run_steps(4);
    assert!(!page_one(&h, "Thrown").0.iter().any(|t| t.contains("Thrown away")));
    assert_eq!(h.state().phone_tool(), Some(ToolMode::Edit), "still in Edit");
}

/// In Edit a finger scrolls the pages: it never drags a paragraph along.
#[test]
fn a_finger_scrolls_the_pages_while_editing() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    let id = h.state().views[0].id;
    let generation = h.state().session.get(id).unwrap().edit_generation();
    let before = h.state().views[0].page_screen_rect(0).unwrap().top();
    let from = at(&h, 0, 60.0, 254.0);
    finger_drag(&mut h, from, from - vec2(0.0, 220.0));
    h.run_steps(6);
    let after = h.state().views[0].page_screen_rect(0).unwrap().top();
    assert!(after < before - 100.0, "the pages scrolled: {before} → {after}");
    assert_eq!(h.state().session.get(id).unwrap().edit_generation(), generation, "nothing moved on the page");
    assert!(!h.state().phone_text_editing());
}

/// Add text: a tap where it goes, then the same typing screen; the text lands there.
#[test]
fn add_text_types_new_text_where_the_page_was_tapped() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    click(&mut h, "Add text");
    assert_eq!(h.state().quick_tool, QuickTool::AddText);
    let p = at(&h, 0, 100.0, 100.0);
    tap(&mut h, p);
    assert!(h.state().phone_text_editing());
    h.event(Event::Text("New words".to_owned()));
    h.run_steps(3);
    click(&mut h, "Done");
    h.run_steps(4);
    let id = h.state().views[0].id;
    let added = h.state().session.get(id).unwrap().added.clone();
    assert!(added.iter().any(|a| matches!(&a.content, pdfcraft_engine::AddedContent::Text(t) if t.text == "New words") && a.page == 0), "{added:?}");
    assert_eq!(h.state().quick_tool, QuickTool::EditText, "back to changing what is on the page");
}

/// More has the rest of Edit a PDF, and nothing a phone can't use.
#[test]
fn more_has_the_rest_of_edit_a_pdf() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    click(&mut h, "More");
    assert_eq!(h.state().phone_sheet(), Some(Sheet::EditMore));
    for label in ["Watermark", "Header and footer", "Rotate pages"] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    assert!(h.query_by_label("Link").is_none(), "links are drawn with a mouse");
    // (The sheet scrolls: the pages' rows come after the content's.)
    h.get_by_label("Organize pages").scroll_to_me();
    h.run_steps(3);
    click(&mut h, "Organize pages");
    assert_eq!(h.state().phone_tool(), None, "the page grid is a screen of its own");
    assert!(h.state().views[0].organize);
}

/// A picture tapped in Edit: a bar of what can be done to it (turn, flip, replace, save,
/// delete) in place of the tools, and Back lets go of it.
#[test]
fn a_picture_tapped_in_edit_gets_a_bar_of_what_can_be_done_to_it() {
    let mut png = Vec::new();
    image::RgbImage::from_pixel(80, 40, image::Rgb([200, 40, 40])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let mut h = harness_with(None, move |app| {
        app.open_bytes("picture.png", None, png.clone()).expect("a picture opens as a page");
    });
    let id = h.state().views[0].id;
    let size = |h: &Harness<'static, PdfCraftApp>| {
        let r = h.state().session.get(id).unwrap().page_images(0).first().map(|i| i.rect);
        r.map(|r| ((r[2] - r[0]).abs().round(), (r[3] - r[1]).abs().round()))
    };
    let before = size(&h).expect("the picture");
    click(&mut h, "Edit");
    let c = h.state().views[0].page_screen_rect(0).unwrap().center();
    tap(&mut h, c);
    assert!(h.state().views[0].image_selection.is_some(), "the tap picked the picture");
    assert!(h.query_by_label("Edit text").is_none(), "its bar replaces the tools");
    click(&mut h, "Rotate");
    h.run_steps(4);
    assert_eq!(size(&h), Some((before.1, before.0)), "a quarter turn");
    back(&mut h);
    assert!(h.state().views[0].image_selection.is_none(), "Back lets go of it");
    assert_eq!(h.state().phone_tool(), Some(ToolMode::Edit), "and stays in Edit");
    tap(&mut h, c);
    click(&mut h, "Delete");
    h.run_steps(4);
    assert_eq!(size(&h), None, "gone");
    assert!(h.query_by_label("Edit text").is_some(), "the tools are back");
}

/// Text added earlier, tapped in Edit: its own bar (Edit text, Delete), and the typing screen's
/// formats reach it as well as its words.
#[test]
fn added_text_is_retyped_and_restyled_from_its_bar() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    click(&mut h, "Add text");
    let p = at(&h, 0, 100.0, 100.0);
    tap(&mut h, p);
    h.event(Event::Text("Added".to_owned()));
    h.run_steps(3);
    click(&mut h, "Done");
    h.run_steps(4);
    let id = h.state().views[0].id;
    let first = |h: &Harness<'static, PdfCraftApp>| match h.state().session.get(id).unwrap().added.first().map(|a| a.content.clone()) {
        Some(pdfcraft_engine::AddedContent::Text(t)) => Some((t.text, t.size, t.bold)),
        _ => None,
    };
    let (_, size, _) = first(&h).expect("the text was added");
    // Selected once it exists: its bar.
    assert!(h.query_by_label("Delete").is_some(), "the added text's bar");
    click(&mut h, "Edit text");
    assert!(h.state().phone_text_editing());
    retype(&mut h, "Added, then changed");
    click(&mut h, "Bold");
    click(&mut h, "Larger text");
    click(&mut h, "Done");
    h.run_steps(4);
    let (text, bigger, bold) = first(&h).expect("still there");
    assert_eq!(text, "Added, then changed");
    assert!(bold && bigger > size, "the formats reached it: {size} → {bigger}, bold {bold}");
}

/// One thing picked at a time: an added item tapped lets go of the page's picture, so its bar is
/// the item's (Delete deletes the item, not the picture); leaving Edit lets go of what was picked.
#[test]
fn a_picture_and_an_added_item_are_never_picked_together() {
    let mut png = Vec::new();
    image::RgbImage::from_pixel(80, 40, image::Rgb([200, 40, 40])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let mut h = harness_with(None, move |app| {
        app.open_bytes("picture.png", None, png.clone()).expect("a picture opens as a page");
    });
    click(&mut h, "Edit");
    let r = h.state().views[0].page_screen_rect(0).unwrap();
    let (c, corner) = (r.center(), r.left_bottom() + vec2(8.0, -8.0));
    click(&mut h, "Add text");
    tap(&mut h, c);
    h.event(Event::Text("Over it".to_owned()));
    h.run_steps(3);
    click(&mut h, "Done");
    h.run_steps(6);
    assert!(h.query_by_label("Delete").is_some(), "the new text is picked, with its bar");
    tap(&mut h, corner);
    assert!(h.state().views[0].image_selection.is_some(), "now the picture");
    assert!(h.query_by_label("Rotate").is_some());
    // The text again: the middle of its box (display space, y up, on the 80 × 40 point page).
    let id = h.state().views[0].id;
    let rect = match h.state().session.get(id).unwrap().added.first().map(|a| a.content.clone()) {
        Some(pdfcraft_engine::AddedContent::Text(t)) => t.rect,
        other => panic!("the text was added: {other:?}"),
    };
    let (w, ht) = (h.state().session.get(id).unwrap().info.pages[0].width, h.state().session.get(id).unwrap().info.pages[0].height);
    assert!(rect[2] <= f64::from(w), "the box stays on the page: {rect:?}");
    let mid =
        pos2(r.left() + ((rect[0] + rect[2]) / 2.0) as f32 / w * r.width(), r.top() + (ht - ((rect[1] + rect[3]) / 2.0) as f32) / ht * r.height());
    tap(&mut h, mid);
    assert!(h.state().phone_text_editing(), "a tap on added text opens it to retype");
    assert!(h.state().views[0].image_selection.is_none(), "and lets go of the picture");
    click(&mut h, "Cancel");
    assert!(h.state().views[0].content.selected.is_some(), "the text stays picked");
    assert!(h.query_by_label("Rotate").is_none(), "its bar, not the picture's");
    tap(&mut h, corner);
    assert!(h.state().views[0].image_selection.is_some());
    // Done ends Edit.
    click(&mut h, "Done");
    assert_eq!(h.state().phone_tool(), None);
    assert!(h.state().views[0].image_selection.is_none(), "leaving Edit lets go of the picture");
}

/// A tap on a format leaves the keyboard with the text, so typing carries on.
#[test]
fn the_keyboard_stays_with_the_text_after_a_format() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    let p = at(&h, 0, 60.0, 254.0);
    tap(&mut h, p);
    click(&mut h, "Italic");
    h.run_steps(2);
    assert!(h.ctx.memory(|m| m.has_focus(egui::Id::new("phone-typing-input"))), "the text has the keyboard again");
}

/// Hold a finger at `from` long enough to pick up what is there, move it to `to`, lift it.
fn long_press_drag(h: &mut Harness<'static, PdfCraftApp>, from: Pos2, to: Pos2) {
    h.hover_at(from);
    h.run_steps(1);
    press(h, from, true);
    // 0.6 s: a long press.
    h.run_steps(12);
    for k in 1..=6 {
        h.hover_at(from + (to - from) * (k as f32 / 6.0));
        h.run_steps(1);
    }
    press(h, to, false);
    h.run_steps(6);
}

/// The paragraph with `word` in it on page 1: its box (user space, y up).
fn paragraph_box(h: &Harness<'static, PdfCraftApp>, word: &str) -> [f64; 4] {
    let id = h.state().views[0].id;
    h.state().session.get(id).unwrap().text_blocks(0).into_iter().find(|b| b.text.contains(word)).map(|b| b.rect).expect("the paragraph")
}

/// In Edit a long press picks a paragraph up: it follows the finger, and lands where it lifts.
#[test]
fn a_long_press_moves_a_paragraph_with_the_finger() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    let before = paragraph_box(&h, "quick");
    let top = h.state().views[0].page_screen_rect(0).unwrap().top();
    let from = at(&h, 0, 60.0, 254.0);
    long_press_drag(&mut h, from, from + vec2(60.0, 80.0));
    assert_eq!(h.state().views[0].page_screen_rect(0).unwrap().top(), top, "the pages held still while it moved");
    assert!(!h.state().phone_text_editing(), "the drop opens nothing to type in");
    let after = paragraph_box(&h, "quick");
    assert!(after[0] > before[0] + 20.0 && after[1] < before[1] - 20.0, "moved right and down: {before:?} → {after:?}");
    assert!(page_one(&h, "quick").0.iter().any(|t| t.contains("The quick brown fox")), "the same words");
    click(&mut h, "Undo");
    assert_eq!(paragraph_box(&h, "quick"), before, "one undo puts it back");
}

/// A long press let go without moving drops it where it was: nothing moves, nothing opens.
#[test]
fn a_long_press_let_go_in_place_changes_nothing() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    let id = h.state().views[0].id;
    let generation = h.state().session.get(id).unwrap().edit_generation();
    let from = at(&h, 0, 60.0, 254.0);
    // Lifted soon after it became a long press: soon enough for egui to call it a tap too.
    h.hover_at(from);
    h.run_steps(1);
    press(&h, from, true);
    h.run_steps(10);
    press(&h, from, false);
    h.run_steps(6);
    assert!(!h.state().phone_text_editing(), "the lift isn't also a tap");
    assert_eq!(h.state().session.get(id).unwrap().edit_generation(), generation);
    // A tap afterwards still opens the paragraph.
    tap(&mut h, from);
    assert!(h.state().phone_text_editing());
}

/// Text added earlier, and a picture, move the same way.
#[test]
fn a_long_press_moves_added_text_and_pictures() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    click(&mut h, "Add text");
    let p = at(&h, 0, 150.0, 60.0);
    tap(&mut h, p);
    h.event(Event::Text("Moved".to_owned()));
    h.run_steps(3);
    click(&mut h, "Done");
    h.run_steps(4);
    let id = h.state().views[0].id;
    let rect = |h: &Harness<'static, PdfCraftApp>| match h.state().session.get(id).unwrap().added.first().map(|a| a.content.clone()) {
        Some(pdfcraft_engine::AddedContent::Text(t)) => t.rect,
        other => panic!("the text: {other:?}"),
    };
    let before = rect(&h);
    // Its box hangs below where the page was tapped.
    let on_it = p + vec2(12.0, 8.0);
    long_press_drag(&mut h, on_it, on_it - vec2(40.0, 60.0));
    let after = rect(&h);
    assert!(after[0] < before[0] - 15.0 && after[3] > before[3] + 15.0, "moved left and up: {before:?} → {after:?}");
    assert!(!h.state().phone_text_editing());

    let mut png = Vec::new();
    image::RgbImage::from_pixel(80, 40, image::Rgb([200, 40, 40])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let mut h = harness_with(None, move |app| {
        app.open_bytes("picture.png", None, png.clone()).expect("a picture opens as a page");
    });
    let id = h.state().views[0].id;
    let left = |h: &Harness<'static, PdfCraftApp>| h.state().session.get(id).unwrap().page_images(0)[0].rect[0];
    let before = left(&h);
    click(&mut h, "Edit");
    let c = h.state().views[0].page_screen_rect(0).unwrap().center();
    long_press_drag(&mut h, c, c + vec2(60.0, 0.0));
    assert!(left(&h) > before + 5.0, "the picture moved right: {before} → {}", left(&h));
}

/// A long press on something over the page (a sheet, a bar) is that thing's: the paragraph
/// under it stays where it is.
#[test]
fn a_long_press_over_a_sheet_leaves_the_page_alone() {
    let mut h = harness(true);
    click(&mut h, "Edit");
    click(&mut h, "More");
    let id = h.state().views[0].id;
    let generation = h.state().session.get(id).unwrap().edit_generation();
    // The sheet's dimmed cover lies over the paragraph.
    let from = at(&h, 0, 60.0, 254.0);
    long_press_drag(&mut h, from, from + vec2(60.0, 80.0));
    assert_eq!(h.state().session.get(id).unwrap().edit_generation(), generation, "nothing moved under the sheet");
}

/// Fill & Sign keeps the saved signature and initials within reach: a sheet to place, change or
/// remove them, or to make them.
#[test]
fn fill_and_sign_has_the_saved_signature_to_use_change_or_remove() {
    let mut h = harness_with(Some(FIXTURE), |app| app.signature = Some(pdfcraft_ui_egui::fill_sign::SavedSig::Typed("Jean D.".into())));
    click(&mut h, "Fill & Sign");
    click(&mut h, "Sign yourself");
    assert_eq!(h.state().phone_sheet(), Some(Sheet::Sign));
    assert!(h.query_by_label("Use signature").is_some(), "the saved signature, to place");
    assert!(h.query_by_label("Add initials").is_some(), "no initials yet: the way to make them");
    click(&mut h, "Use signature");
    assert_eq!(h.state().quick_tool, QuickTool::Fill(pdfcraft_ui_egui::fill_sign::FillTool::Signature), "ready to place");
    assert_eq!(h.state().phone_sheet(), None);
    click(&mut h, "Sign yourself");
    click(&mut h, "Remove saved signature");
    assert!(h.state().signature.is_none(), "removed");
    click(&mut h, "Sign yourself");
    click(&mut h, "Add signature");
    assert_eq!(h.state().dialog, Some(pdfcraft_ui_egui::Dialog::Signature), "making one opens its dialog");
}

/// A picture picked in Edit has handles at its corners, added text at its sides: a finger on one
/// resizes it at once, and the pages hold still.
#[test]
fn a_finger_on_a_handle_resizes_what_is_picked() {
    let mut png = Vec::new();
    image::RgbImage::from_pixel(80, 40, image::Rgb([200, 40, 40])).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
    let mut h = harness_with(None, move |app| {
        app.open_bytes("picture.png", None, png.clone()).expect("a picture opens as a page");
    });
    let id = h.state().views[0].id;
    let width = |h: &Harness<'static, PdfCraftApp>| {
        let r = h.state().session.get(id).unwrap().page_images(0)[0].rect;
        (r[2] - r[0]).abs()
    };
    let before = width(&h);
    click(&mut h, "Edit");
    let r = h.state().views[0].page_screen_rect(0).unwrap();
    tap(&mut h, r.center());
    assert!(h.state().views[0].image_selection.is_some());
    let top = r.top();
    // The bottom-right handle, in towards the middle.
    finger_drag(&mut h, r.right_bottom(), r.right_bottom() - vec2(r.width() / 2.0, r.height() / 2.0));
    h.run_steps(4);
    assert!(width(&h) < before * 0.75, "smaller: {before} → {}", width(&h));
    assert_eq!(h.state().views[0].page_screen_rect(0).unwrap().top(), top, "the pages held still");

    // Added text: its box wider from its right edge.
    let mut h = harness(true);
    click(&mut h, "Edit");
    click(&mut h, "Add text");
    let p = at(&h, 0, 40.0, 60.0);
    tap(&mut h, p);
    h.event(Event::Text("Wide".to_owned()));
    h.run_steps(3);
    click(&mut h, "Done");
    h.run_steps(4);
    let id = h.state().views[0].id;
    let rect = |h: &Harness<'static, PdfCraftApp>| match h.state().session.get(id).unwrap().added.first().map(|a| a.content.clone()) {
        Some(pdfcraft_engine::AddedContent::Text(t)) => t.rect,
        other => panic!("the text: {other:?}"),
    };
    let before = rect(&h);
    let page = h.state().views[0].page_screen_rect(0).unwrap();
    let k = page.width() / 300.0;
    // Its top-left corner on screen is where the page was tapped: its right side's handle is its
    // width on, halfway down.
    let side = pos2(p.x + (before[2] - before[0]) as f32 * k, p.y + (before[3] - before[1]) as f32 * k / 2.0);
    finger_drag(&mut h, side, side - vec2(40.0, 0.0));
    h.run_steps(4);
    let after = rect(&h);
    assert!(after[2] < before[2] - 10.0, "narrower from its right edge: {before:?} → {after:?}");
}

/// On a phone a dialog's buttons and choices are a finger tall; the desktop keeps its own.
#[test]
fn a_dialogs_controls_are_finger_sized_on_a_phone() {
    let mut h = harness(true);
    h.state_mut().set_option("dialog", "background").unwrap();
    h.run_steps(6);
    let height = |h: &Harness<'static, PdfCraftApp>, label: &str| h.query_all_by_label(label).last().map(|n| n.rect().height()).unwrap_or(0.0);
    assert!(height(&h, "OK") >= 40.0, "OK: {}", height(&h, "OK"));
    assert!(height(&h, "All pages") >= 36.0, "a radio button: {}", height(&h, "All pages"));
    h.state_mut().set_option("shell", "auto").unwrap();
    h.run_steps(6);
    assert!(height(&h, "OK") < 32.0, "the desktop's: {}", height(&h, "OK"));
}

/// A dialog's drop-down list opens on rows a finger tall on a phone; the desktop keeps its own.
#[test]
fn a_dialogs_lists_open_on_finger_sized_rows() {
    let mut h = harness(true);
    h.state_mut().set_option("dialog", "background").unwrap();
    h.run_steps(6);
    let row = |h: &mut Harness<'static, PdfCraftApp>| {
        // (A list is named by its choice, its value.)
        h.query_all_by(|n| n.value().as_deref() == Some("All pages in range")).last().expect("the subset list").click();
        h.run_steps(4);
        let height = h.query_all_by_label("Odd pages only").last().map(|n| n.rect().height()).unwrap_or(0.0);
        h.key_press(Key::Escape);
        h.run_steps(3);
        height
    };
    let phone = row(&mut h);
    assert!(phone >= 36.0, "a phone's row: {phone}");
    h.state_mut().set_option("shell", "auto").unwrap();
    h.state_mut().set_option("dialog", "background").unwrap();
    h.run_steps(6);
    let desktop = row(&mut h);
    assert!(desktop > 0.0 && desktop < 30.0, "the desktop's row: {desktop}");
}
