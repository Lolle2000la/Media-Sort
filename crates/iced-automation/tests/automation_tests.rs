use iced_automation::{
    AutomationState, AutomationStep, AutomationStyle, AutomationTarget, CURSOR_CLICK_PULSE,
    CURSOR_MAX_SCALE, CURSOR_MIN_SCALE, CURSOR_POINTER_UNITS, CURSOR_REFERENCE_HEIGHT,
    CURSOR_UNIT_SCALE, JsonAutomationFlow, JsonAutomationStep, JsonTarget, build_automation_steps,
    cursor_click_ring, cursor_pointer_offsets, cursor_render_scale, handle_automation_virtual_tick,
};
use std::time::Duration;

#[test]
fn test_build_automation_steps_basic() {
    let flow = JsonAutomationFlow {
        flow_name: "test".into(),
        steps: vec![JsonAutomationStep {
            delay_ms: 100,
            target: JsonTarget::Widget { id: "btn1".into() },
            keycap_label: Some("Click Me".into()),
            message: serde_json::Value::Null,
        }],
    };
    let steps = build_automation_steps(
        &flow,
        |_id| "resolved-btn1".into(),
        |_msg| "Click Me".into(),
    );
    assert_eq!(steps.len(), 1);
    assert_eq!(steps[0].execution_delay, Duration::from_millis(100));
    assert_eq!(steps[0].keycap_label, Some("Click Me".to_string()));
}

#[test]
fn test_build_automation_steps_empty() {
    let flow: JsonAutomationFlow = JsonAutomationFlow {
        flow_name: "empty".into(),
        steps: vec![],
    };
    let steps: Vec<AutomationStep<serde_json::Value>> = build_automation_steps::<serde_json::Value>(
        &flow,
        |_id| "unused".into(),
        |_msg| "unused".into(),
    );
    assert!(steps.is_empty());
}

#[test]
fn test_build_automation_steps_coordinate_target() {
    let flow = JsonAutomationFlow {
        flow_name: "test".into(),
        steps: vec![JsonAutomationStep {
            delay_ms: 50,
            target: JsonTarget::Coordinate { x: 42.0, y: 99.0 },
            keycap_label: None,
            message: serde_json::Value::Null,
        }],
    };
    let steps = build_automation_steps(&flow, |_id| "unused".into(), |_msg| "unused".into());
    assert_eq!(steps.len(), 1);
    match &steps[0].target {
        AutomationTarget::Pixel(p) => {
            assert_eq!(p.x, 42.0);
            assert_eq!(p.y, 99.0);
        }
        _ => panic!("expected Pixel target"),
    }
}

// --- Cursor geometry ---

fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() < 1e-4,
        "expected {expected}, got {actual}"
    );
}

/// Width and height of the silhouette's bounding box.
fn cursor_extent(offsets: [(f32, f32); 7]) -> (f32, f32) {
    let mut min = (f32::INFINITY, f32::INFINITY);
    let mut max = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for (x, y) in offsets {
        min = (min.0.min(x), min.1.min(y));
        max = (max.0.max(x), max.1.max(y));
    }
    (max.0 - min.0, max.1 - min.1)
}

/// Indices of the polygon's reflex (concave) vertices.
fn reflex_vertices(vertices: &[(f32, f32)]) -> Vec<usize> {
    let n = vertices.len();
    (0..n)
        .filter(|&i| {
            let prev = vertices[(i + n - 1) % n];
            let cur = vertices[i];
            let next = vertices[(i + 1) % n];
            let incoming = (cur.0 - prev.0, cur.1 - prev.1);
            let outgoing = (next.0 - cur.0, next.1 - cur.1);
            let cross = incoming.0 * outgoing.1 - incoming.1 * outgoing.0;
            // The silhouette winds clockwise in screen coordinates (y down),
            // so a reflex corner is the one turning counter-clockwise.
            cross > 0.0
        })
        .collect()
}

#[test]
fn test_cursor_tip_is_the_pointer_origin() {
    // `draw()` places every vertex at `position + offset`, so the drawn tip —
    // and therefore the click ring, which is centred on `position` — lands
    // exactly on the widget centre the emulator aimed at only while the tip
    // vertex is the origin. It also has to be the silhouette's extreme point,
    // or the visible tip would sit elsewhere.
    assert_eq!(CURSOR_POINTER_UNITS[0], (0.0, 0.0));
    let topmost = CURSOR_POINTER_UNITS
        .iter()
        .map(|(_, y)| *y)
        .fold(f32::INFINITY, f32::min);
    let leftmost = CURSOR_POINTER_UNITS
        .iter()
        .map(|(x, _)| *x)
        .fold(f32::INFINITY, f32::min);
    assert_eq!((leftmost, topmost), (0.0, 0.0));

    for height in [200.0, 540.0, CURSOR_REFERENCE_HEIGHT, 2160.0, 4320.0] {
        let offsets = cursor_pointer_offsets(height);
        assert_eq!(offsets[0], (0.0, 0.0), "tip moved at height {height}");
        // Nothing hangs above or left of the tip either: the whole pointer is
        // drawn down-right of the click point it represents.
        for (x, y) in &offsets[1..] {
            assert!(
                *x >= 0.0 && *y >= 0.0,
                "vertex ({x}, {y}) sits up/left of the tip at height {height}"
            );
        }
    }
}

#[test]
fn test_cursor_is_a_tall_narrow_pointer_at_1080p() {
    let (width, height) = cursor_extent(cursor_pointer_offsets(CURSOR_REFERENCE_HEIGHT));
    // ~17 × 30 px: the size of a tutorial/screen-recording cursor in a 1080p
    // video. The previous 19.4 px-tall pointer read as a fleck.
    assert_close(width, 17.05);
    assert_close(height, 30.07);
    // A real pointer is roughly twice as tall as it is wide; the old wedge
    // was 28×34 (aspect 0.82) and the head was its widest part.
    let aspect = width / height;
    assert!(
        (0.5..=0.6).contains(&aspect),
        "aspect {aspect} is not pointer-like"
    );
}

#[test]
fn test_cursor_scales_with_render_height() {
    // 2160p lands at 3.1×, inside the clamp, so a 4K export scales exactly.
    let (four_k_width, four_k_height) = cursor_extent(cursor_pointer_offsets(2160.0));
    assert_close(four_k_width, 34.1);
    assert_close(four_k_height, 60.14);
    assert_close(cursor_render_scale(2160.0), 3.1);

    // A short window sits below the floor, so the pointer is clamped to the
    // minimum scale rather than shrinking into invisibility.
    assert_close(cursor_render_scale(400.0), CURSOR_MIN_SCALE);
    let (small_width, small_height) = cursor_extent(cursor_pointer_offsets(400.0));
    assert_close(small_width, 8.25);
    assert_close(small_height, 14.55);

    // A huge render is clamped too, so the arrow cannot cover the screen.
    assert_close(cursor_render_scale(4320.0), CURSOR_MAX_SCALE);
    let (huge_width, _) = cursor_extent(cursor_pointer_offsets(4320.0));
    assert_close(huge_width, 38.5);
}

#[test]
fn test_cursor_scale_falls_back_for_degenerate_height() {
    // A zero/negative/NaN canvas height must not produce a zero-size path
    // (an invisible cursor) or a NaN path (nothing drawn at all); it falls
    // back to the reference scale, i.e. the 1080p pointer.
    for height in [0.0, -1.0, f32::NAN, f32::INFINITY] {
        assert_close(cursor_render_scale(height), CURSOR_UNIT_SCALE);
        let (width, height_px) = cursor_extent(cursor_pointer_offsets(height));
        assert_close(width, 17.05);
        assert_close(height_px, 30.07);
    }
}

#[test]
fn test_outline_width_scales_with_the_render_height() {
    // The field is in reference units, not pixels: it must grow with the
    // render. A stroke that ignored the scale would be the same px at 1080p
    // and 2160p.
    let style = AutomationStyle::default();
    let at_1080 = style.cursor_outline_width * cursor_render_scale(CURSOR_REFERENCE_HEIGHT);
    let at_2160 = style.cursor_outline_width * cursor_render_scale(2160.0);
    assert_close(at_1080, 1.7515);
    assert_close(at_2160, 2.0 * at_1080);

    // And the white body has to stay wider than the edge drawn around it (the
    // stroke is centred, so it eats half its width on each side).
    let width = cursor_extent(cursor_pointer_offsets(CURSOR_REFERENCE_HEIGHT)).0;
    let body = width - at_1080;
    assert!(
        body > at_1080,
        "outline {at_1080} px leaves only {body} px of white on a {width} px wide pointer"
    );
}

#[test]
fn test_cursor_has_shoulder_notch_and_leg() {
    // A convex 7-gon (a blob, or the old wedge) has no reflex corner at all;
    // the canonical pointer is concave both where the tail leg leaves the head
    // and where it rejoins the head's right edge.
    assert_eq!(reflex_vertices(&CURSOR_POINTER_UNITS), vec![2, 5]);
}

#[test]
fn test_click_ring_expands_and_fades_then_disappears() {
    let (start_radius, start_alpha) = cursor_click_ring(Duration::ZERO).expect("ring at click");
    // The ring starts wide enough that its arc clears the arrow's body (the
    // earlier 5 px start was swallowed by the pointer and read as no feedback).
    assert_close(start_radius, 9.0);
    assert_close(start_alpha, 0.9);

    let (mid_radius, mid_alpha) = cursor_click_ring(CURSOR_CLICK_PULSE / 2).expect("ring mid");
    assert_close(mid_radius, 16.5);
    assert_close(mid_alpha, 0.45);
    assert!(mid_radius > start_radius);
    assert!(mid_alpha < start_alpha);

    // Past the pulse there is no ring: a click at rest must not leave a hoop
    // painted at the tip for the rest of the demo.
    assert_eq!(cursor_click_ring(CURSOR_CLICK_PULSE), None);
    assert_eq!(cursor_click_ring(CURSOR_CLICK_PULSE * 2), None);
}

/// Two-step flow: click at (60, 0), then a 400 ms delay before the next step's
/// target is scheduled. Returns the state, ready to be ticked.
fn click_then_transition_state() -> AutomationState<String> {
    AutomationState::new(
        vec![
            AutomationStep {
                execution_delay: Duration::ZERO,
                target: AutomationTarget::Pixel(iced::Point::new(60.0, 0.0)),
                underlying_message: Some("click".to_string()),
                keycap_label: None,
            },
            AutomationStep {
                execution_delay: Duration::from_millis(400),
                target: AutomationTarget::Pixel(iced::Point::ORIGIN),
                underlying_message: Some("next".to_string()),
                keycap_label: None,
            },
        ],
        "click pulse",
        1920.0,
        1080.0,
        AutomationStyle::default(),
    )
}

#[test]
fn test_click_pulse_fires_once_per_click() {
    // Regression: the ring used to read `step_elapsed`, which the step
    // scheduler also resets when a step's delay expires, so the frame after
    // every step transition restarted the pulse at full opacity (visible as a
    // one-frame flash in every exported video).
    let delta = Duration::from_millis(16);
    let mut state = click_then_transition_state();
    assert_eq!(state.click_elapsed(), None, "ring before any click");

    let mut pulse_starts = 0;
    let mut clicked = false;
    let mut transition = None;
    let mut ticks = 0;
    while transition.is_none() {
        ticks += 1;
        assert!(ticks < 500, "the second step never became ready");

        let before = state.current_pixel_target;
        let _ = handle_automation_virtual_tick(&mut state, delta);

        if let Some(elapsed) = state.click_elapsed() {
            clicked = true;
            // A tick at the very start of the pulse is a pulse *start*.
            if elapsed < delta {
                pulse_starts += 1;
            }
        }
        // The scheduler re-targeting the cursor is the step transition.
        if before != state.current_pixel_target && clicked {
            transition = Some(state.click_elapsed());
        }
    }

    assert!(
        clicked,
        "the flow never clicked, so the test proves nothing"
    );
    assert_eq!(pulse_starts, 1, "the click pulse started more than once");

    let elapsed = transition
        .expect("the second step's delay never expired")
        .expect("a clicked cursor keeps its pulse state until it moves");
    assert!(
        elapsed >= CURSOR_CLICK_PULSE,
        "the ring restarted at the step transition ({elapsed:?} into a {CURSOR_CLICK_PULSE:?} pulse)"
    );
    assert_eq!(cursor_click_ring(elapsed), None);
}
