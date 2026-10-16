//! Touch screens: where a lifted finger was.
//!
//! When a finger lifts, egui-winit reports the release and `PointerGone` in the same frame
//! (`egui-winit` 0.36, `on_touch`), so on the frame of a tap `hover_pos` is `None`, and whatever
//! asks where the pointer is when it sees the click (a link, a form field, a comment) misses the
//! tap. [`defer_pointer_gone`] keeps the pointer for that frame and lets it go in the next one.

use egui::Event;

/// Drop a `PointerGone` that follows a button release in this frame's input, and deliver it at
/// the start of the next frame instead (`pending` carries it over). A pointer that moves or
/// presses again after it in the same frame is back, so nothing is carried over then.
pub fn defer_pointer_gone(raw: &mut egui::RawInput, pending: &mut bool) {
    if std::mem::take(pending) {
        raw.events.insert(0, Event::PointerGone);
    }
    let mut released = false;
    let mut gone = false;
    raw.events.retain(|e| match e {
        Event::PointerButton { pressed: false, .. } => {
            released = true;
            true
        }
        Event::PointerGone if released => {
            gone = true;
            false
        }
        Event::PointerMoved(_) | Event::PointerButton { pressed: true, .. } => {
            released = false;
            gone = false;
            true
        }
        _ => true,
    });
    *pending = gone;
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Modifiers, PointerButton, pos2};

    fn button(pressed: bool) -> Event {
        Event::PointerButton { pos: pos2(10.0, 20.0), button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }

    fn frame(events: Vec<Event>, pending: &mut bool) -> Vec<Event> {
        let mut raw = egui::RawInput { events, ..Default::default() };
        defer_pointer_gone(&mut raw, pending);
        raw.events
    }

    #[test]
    fn a_tap_keeps_its_pointer_until_the_next_frame() {
        let mut pending = false;
        let tap = frame(vec![Event::PointerMoved(pos2(10.0, 20.0)), button(true), button(false), Event::PointerGone], &mut pending);
        assert_eq!(tap, vec![Event::PointerMoved(pos2(10.0, 20.0)), button(true), button(false)]);
        assert!(pending);
        assert_eq!(frame(Vec::new(), &mut pending), vec![Event::PointerGone], "gone one frame later");
        assert!(!pending);
        assert!(frame(Vec::new(), &mut pending).is_empty(), "and only once");
    }

    #[test]
    fn a_pointer_that_leaves_without_a_release_goes_at_once() {
        let mut pending = false;
        assert_eq!(frame(vec![Event::PointerMoved(pos2(1.0, 1.0)), Event::PointerGone], &mut pending).last(), Some(&Event::PointerGone));
        assert!(!pending);
    }

    #[test]
    fn a_new_touch_in_the_same_frame_is_not_dropped_next_frame() {
        let mut pending = false;
        let events = frame(vec![button(false), Event::PointerGone, Event::PointerMoved(pos2(5.0, 5.0)), button(true)], &mut pending);
        assert!(!events.contains(&Event::PointerGone));
        assert!(!pending, "the second finger is down: no PointerGone may follow it");
        assert!(frame(Vec::new(), &mut pending).is_empty());
    }
}
