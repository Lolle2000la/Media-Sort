use iced_automation::{
    AutomationStep, AutomationStyle, AutomationTarget, CURSOR_CLICK_PULSE, CURSOR_MAX_SCALE,
    CURSOR_MIN_SCALE, CURSOR_POINTER_UNITS, CURSOR_REFERENCE_HEIGHT, CURSOR_SHADOW_ALPHA,
    CURSOR_SHADOW_OFFSET_UNITS, CURSOR_UNIT_SCALE, JsonAutomationFlow, JsonAutomationStep,
    JsonTarget, build_automation_steps, cursor_click_ring, cursor_pointer_offsets,
    cursor_render_scale,
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
fn test_cursor_tip_stays_on_automation_target() {
    // The emulator aims widget centres at the tip; any offset in the first
    // vertex would draw the click ring and the pointer somewhere else.
    for height in [200.0, 540.0, CURSOR_REFERENCE_HEIGHT, 2160.0, 4320.0] {
        let offsets = cursor_pointer_offsets(height);
        assert_eq!(offsets[0], (0.0, 0.0), "tip moved at height {height}");
    }
    assert_eq!(CURSOR_POINTER_UNITS[0], (0.0, 0.0));
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
fn test_default_outline_is_visible_at_1080p() {
    // A sub-2 px stroke on the old 19 px arrow fringed at the yuv420p chroma
    // limit; on the 30 px pointer the stroke must instead stay thin enough
    // that the white body dominates the glyph.
    let style = AutomationStyle::default();
    let outline = style.cursor_outline_width * cursor_render_scale(CURSOR_REFERENCE_HEIGHT);
    assert_close(outline, 1.7515);
    let width = cursor_extent(cursor_pointer_offsets(CURSOR_REFERENCE_HEIGHT)).0;
    assert!(
        outline / width < 0.12,
        "outline {outline} px is too heavy for a {width} px wide pointer (ratio {})",
        outline / width
    );
}

#[test]
fn test_cursor_shadow_stays_subtle() {
    // The shadow is depth, not a second body: on the old 19 px arrow a 0.3
    // alpha shadow plus a 2 px stroke turned the whole glyph dark.
    let style = AutomationStyle::default();
    assert!(
        CURSOR_SHADOW_ALPHA < 0.25 * style.outline_color.a,
        "shadow alpha {CURSOR_SHADOW_ALPHA} is too close to the edge's opacity"
    );
    let offset_px = CURSOR_SHADOW_OFFSET_UNITS * cursor_render_scale(CURSOR_REFERENCE_HEIGHT);
    assert_close(offset_px, 2.015);
    let width = cursor_extent(cursor_pointer_offsets(CURSOR_REFERENCE_HEIGHT)).0;
    assert!(offset_px < width * 0.2, "shadow offset {offset_px} px");
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
