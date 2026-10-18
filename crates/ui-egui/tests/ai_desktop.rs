//! The AI assistant in the desktop layout: All tools ▸ AI assistant, and its commands.

use egui::accesskit::Role;
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use pdfcraft_ui_egui::PdfCraftApp;

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

fn desktop() -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(|_| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("three.pdf", None, FIXTURE.to_vec()).unwrap();
        app
    });
    h.run_steps(6);
    h
}

fn shows(h: &Harness<'static, PdfCraftApp>, part: &str) -> bool {
    h.query_all_by(|n| n.label().is_some_and(|l| l.contains(part)) || n.value().is_some_and(|v| v.contains(part))).next().is_some()
}

#[test]
fn the_tool_panel_holds_the_assistant_its_settings_first() {
    let mut h = desktop();
    h.state_mut().set_option("tool", "ai").unwrap();
    h.run_steps(4);
    // Nothing set up: the settings, saying what is sent and to whom, before any key.
    assert!(shows(&h, "with your own API key"), "the settings come first");
    h.get_by_label("Save");
    // Set up: the conversation, with its quick actions and its question box.
    h.state_mut().ai.set_key("sk-ant-test");
    h.get_by_label("Settings").click();
    h.run_steps(4);
    // (The old list of tools had "Summarize" too: Send is the conversation's alone.)
    h.get_by_label("Send");
    assert!(!shows(&h, "with your own API key"), "the settings closed");
}

#[test]
fn the_summarize_command_asks_once_the_service_is_set_up() {
    let mut h = desktop();
    h.state_mut().ai.set_key("sk-ant-test");
    // A cost guard that refuses before anything is sent: three pages, at most one.
    h.state_mut().ai.max_pages = 1;
    h.state_mut().run_command("ai.summary");
    assert!(h.state().ai.busy(), "the command asks right away");
    for _ in 0..400 {
        h.run_steps(1);
        if !h.state().ai.busy() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.run_steps(3);
    assert!(!h.state().ai.busy());
    assert!(shows(&h, "the assistant reads at most 1"), "the panel shows why, with the way out");
}

#[test]
fn the_sparkles_button_opens_the_assistant_and_puts_it_away() {
    let mut h = desktop();
    // The tool panel closed: the mode bar's button is the only "AI assistant" on screen.
    h.state_mut().set_option("left", "closed").unwrap();
    h.run_steps(4);
    h.get_by_label("AI assistant").click();
    h.run_steps(4);
    assert!(shows(&h, "with your own API key"), "the assistant opened, on its settings");
    // The panel's header says "AI assistant" too: the button is the one to click.
    h.get_by_role_and_label(Role::Button, "AI assistant").click();
    h.run_steps(4);
    assert!(!shows(&h, "with your own API key"), "a second click put it away");
}
