//! Phone-sized windows (PdfCraft on Android): every mode and every panel stays reachable.

use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use pdfcraft_ui_egui::{Mode, PdfCraftApp, RightPanel};

/// A tiny two-page PDF.
const FIXTURE: &[u8] = b"%PDF-1.7
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >> endobj
4 0 obj << /Type /Page /Parent 2 0 R /MediaBox [0 0 300 400] >> endobj
trailer << /Root 1 0 R >>
%%EOF";

fn harness(size: egui::Vec2) -> Harness<'static, PdfCraftApp> {
    let mut h = Harness::builder().with_size(size).build_eframe(|_| {
        let mut app = PdfCraftApp::new();
        app.set_option("language", "en").unwrap();
        app.open_bytes("two.pdf", None, FIXTURE.to_vec()).unwrap();
        app.set_option("left", "closed").unwrap();
        app
    });
    h.run_steps(6);
    h
}

/// A phone held upright (412 dp wide, drawn at 80%): the mode tabs don't fit, so they become one
/// menu of the current mode, and the search box its icon.
#[test]
fn a_narrow_window_keeps_every_mode_in_one_menu() {
    let mut h = harness(egui::vec2(515.0, 1144.0));
    assert!(h.query_by_label("Read").is_none(), "no row of tabs to overlap the toolbar");
    h.get_by_label("All tools \u{25BE}").click();
    h.run_steps(3);
    h.get_by_label("Edit").click();
    h.run_steps(4);
    assert_eq!(h.state().mode, Mode::Edit);
    h.get_by_label("Edit \u{25BE}");
    h.query_all_by_label("Find tools and commands").find(|n| n.accesskit_node().role() == Role::Button).expect("the search icon").click();
    h.run_steps(3);
    assert!(h.state().palette_open);
}

/// A desktop window keeps the tabs and the search box.
#[test]
fn a_wide_window_keeps_the_tabs_and_the_search_box() {
    let h = harness(egui::vec2(1440.0, 900.0));
    for tab in ["All tools", "Read", "Edit", "Convert", "E-Sign"] {
        h.get_by_label(tab);
    }
    assert!(h.query_by_label_contains("\u{25BE}").is_none());
    h.get_by_label_contains("Find tools and commands");
}

/// A phone held sideways (about 515 points tall): the panel buttons scroll above the page
/// controls instead of running into them, and the last one can still be opened.
#[test]
fn a_short_window_scrolls_the_rail_panels_above_the_page_controls() {
    let mut h = harness(egui::vec2(1144.0, 515.0));
    let top_control = h.get_by_label("Previous page").rect();
    let comments = h.get_by_label("Comments").rect();
    assert!(comments.max.y <= top_control.min.y, "{comments:?} runs into the page controls at {top_control:?}");
    h.get_by_label("Tags").scroll_to_me();
    h.run_steps(4);
    let tags = h.get_by_label("Tags").rect();
    let top_control = h.get_by_label("Previous page").rect();
    assert!(tags.max.y <= top_control.min.y, "scrolled into view, {tags:?} stays above the page controls at {top_control:?}");
    h.get_by_label("Tags").click();
    h.run_steps(4);
    assert_eq!(h.state().right, Some(RightPanel::Tags));
}
