pub mod automation;
pub mod demo_setup;

#[cfg(feature = "headless")]
pub mod headless;

pub use automation::{
    AutomationContext, AutomationMessage, AutomationState, AutomationStateTrait, AutomationStep,
    AutomationStyle, AutomationTarget, AutomationTickResult, CURSOR_CLICK_PULSE, CURSOR_MAX_SCALE,
    CURSOR_MIN_SCALE, CURSOR_POINTER_UNITS, CURSOR_REFERENCE_HEIGHT, JsonAutomationFlow,
    JsonAutomationStep, JsonTarget, VIRTUAL_CURSOR, VirtualTickMessage, build_automation_steps,
    cursor_click_ring, cursor_pointer_offsets, cursor_render_scale, find_bounds_task,
    handle_automation_message, handle_automation_tick, handle_automation_virtual_tick,
    handle_bounds_resolved, static_widget_id, try_tick, try_tick_state, wrap_view,
};

pub use demo_setup::{DemoApp, DemoBootstrap, DemoConfig, FixtureSpec, init_demo};

#[cfg(feature = "headless")]
pub use demo_setup::ExportVideoConfig;

#[cfg(feature = "headless")]
pub use headless::{HeadlessApp, HeadlessBootConfig, default_headless_settings, export_video};

pub use iced_automation_macros::{message, state};
