use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::Deserialize;

use iced::advanced::mouse;
use iced::advanced::widget::Id;
use iced::advanced::widget::operation::{Operation, Outcome, Scrollable, TextInput};
use iced::widget::{canvas, column, container, stack, text};
use iced::{
    Alignment, Color, Element, Length, Point, Rectangle, Renderer, Shadow, Task, Theme, Vector,
};

// --- FindBounds custom operation ---
struct FindBounds {
    target: Id,
    found: Option<Rectangle>,
}

impl Operation<Option<Rectangle>> for FindBounds {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation<Option<Rectangle>>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.target) {
            self.found = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        _translation: Vector,
        _state: &mut dyn Scrollable,
    ) {
        if id == Some(&self.target) {
            self.found = Some(bounds);
        }
    }

    fn text_input(&mut self, id: Option<&Id>, bounds: Rectangle, _state: &mut dyn TextInput) {
        if id == Some(&self.target) {
            self.found = Some(bounds);
        }
    }

    fn text(&mut self, id: Option<&Id>, bounds: Rectangle, _text: &str) {
        if id == Some(&self.target) {
            self.found = Some(bounds);
        }
    }

    fn finish(&self) -> Outcome<Option<Rectangle>> {
        Outcome::Some(self.found)
    }
}

pub fn find_bounds_task(target: Id) -> Task<Option<Rectangle>> {
    iced::advanced::widget::operate(FindBounds {
        target,
        found: None,
    })
}

// --- Automation target ---
#[derive(Debug, Clone)]
pub enum AutomationTarget {
    Widget(Id),
    Pixel(Point),
}

// --- Automation step ---
#[derive(Debug, Clone)]
pub struct AutomationStep<Message> {
    pub execution_delay: Duration,
    pub target: AutomationTarget,
    pub underlying_message: Option<Message>,
    pub keycap_label: Option<String>,
}

// --- Automation style ---
#[derive(Debug, Clone)]
pub struct AutomationStyle {
    pub completed_label: String,
    pub active_label: String,
    pub cursor_color: Color,
    pub click_color: Color,
    pub outline_color: Color,
    pub keycap_timeout: Duration,
    pub easing_factor: f32,
    pub hud_bg: Color,
    pub hud_border: Color,
    pub completed_text_color: Color,
    pub completed_bg: Color,
    pub completed_border: Color,
    pub completed_border_width: f32,
    pub hud_border_width: f32,
    pub cursor_outline_width: f32,
}

impl Default for AutomationStyle {
    fn default() -> Self {
        Self {
            completed_label: "Demo Complete — Close this window".into(),
            active_label: "Automation Active".into(),
            cursor_color: Color::from_rgb(0.95, 0.95, 0.95),
            click_color: Color::from_rgb(0.9, 0.2, 0.2),
            outline_color: Color::BLACK,
            keycap_timeout: Duration::from_millis(1800),
            easing_factor: 0.18,
            hud_bg: Color::from_rgba(0.08, 0.08, 0.1, 0.95),
            hud_border: Color::from_rgb(0.25, 0.45, 0.85),
            completed_text_color: Color::from_rgb(0.85, 0.85, 1.0),
            completed_bg: Color::from_rgba(0.05, 0.05, 0.08, 0.92),
            completed_border: Color::from_rgb(0.2, 0.65, 0.2),
            completed_border_width: 2.0,
            hud_border_width: 1.5,
            // Reference units: 1.13 × `CURSOR_UNIT_SCALE` is a 1.75 px outline
            // at 1080p on a ~30 px tall pointer — a crisp edge that still
            // leaves the white fill dominant (the old 2.0-unit stroke on a
            // 19 px arrow ate most of the interior).
            cursor_outline_width: 1.13,
        }
    }
}

// --- Automation state ---
pub struct AutomationState<Message> {
    pub virtual_cursor: Point,
    pub current_pixel_target: Point,
    pub is_clicking: bool,
    pub active_keycap: Option<(String, Duration)>,
    pub script_index: usize,
    pub steps: Vec<AutomationStep<Message>>,
    pub step_timer: Instant,
    pub virtual_elapsed: Duration,
    pub step_elapsed: Duration,
    pub window_width: f32,
    pub window_height: f32,
    pub flow_name: String,
    pub completed: bool,
    pub pending_bounds_id: Option<Id>,
    pub step_ready: bool,
    pub style: AutomationStyle,
    /// Whether the automation advances on real-time ticks (`Instant`-based,
    /// via `try_tick`). Interactive demos use this. Headless video export
    /// must set this to `false` so the flow is driven exclusively by
    /// deterministic virtual ticks — otherwise real wall-clock time
    /// fast-forwards the steps relative to the rendered frames.
    pub real_time: bool,
}

thread_local! {
    /// Cursor position mirrored from the automation state on each virtual
    /// tick, read by the headless export loop to drive hover rendering.
    /// Thread-local on purpose: parallel exports (rayon) each run their whole
    /// render loop on one worker thread, and a process-global position makes
    /// hover states flicker as concurrent renders overwrite each other.
    pub static VIRTUAL_CURSOR: std::cell::RefCell<Point> = const { std::cell::RefCell::new(Point::ORIGIN) };
}

impl<Message> AutomationState<Message> {
    pub fn new(
        steps: Vec<AutomationStep<Message>>,
        flow_name: &str,
        window_width: f32,
        window_height: f32,
        style: AutomationStyle,
    ) -> Self {
        Self {
            virtual_cursor: Point::ORIGIN,
            current_pixel_target: Point::ORIGIN,
            is_clicking: false,
            active_keycap: None,
            script_index: 0,
            steps,
            step_timer: Instant::now(),
            window_width,
            window_height,
            flow_name: flow_name.to_string(),
            completed: false,
            pending_bounds_id: None,
            step_ready: false,
            virtual_elapsed: Duration::ZERO,
            step_elapsed: Duration::ZERO,
            style,
            real_time: true,
        }
    }

    pub fn update_window_size(&mut self, width: f32, height: f32) {
        self.window_width = width;
        self.window_height = height;
    }
}

// --- Tick result ---
pub enum AutomationTickResult<Message> {
    Message(Message),
    FindBounds(Id),
}

// --- Tick handlers ---
pub fn handle_automation_tick<Message>(
    automation: &mut AutomationState<Message>,
    now: Instant,
) -> Option<AutomationTickResult<Message>>
where
    Message: Clone,
{
    let delta = now
        .duration_since(automation.step_timer)
        .min(Duration::from_millis(500));
    automation.step_timer = now;
    handle_automation_virtual_tick(automation, delta)
}

pub fn handle_automation_virtual_tick<Message>(
    automation: &mut AutomationState<Message>,
    delta: Duration,
) -> Option<AutomationTickResult<Message>>
where
    Message: Clone,
{
    automation.virtual_elapsed += delta;
    automation.step_elapsed += delta;

    let dx = automation.current_pixel_target.x - automation.virtual_cursor.x;
    let dy = automation.current_pixel_target.y - automation.virtual_cursor.y;
    let cursor_arrived = dx.abs() <= 0.5 && dy.abs() <= 0.5;

    if !cursor_arrived {
        let factor = automation.style.easing_factor;
        automation.virtual_cursor.x += dx * factor;
        automation.virtual_cursor.y += dy * factor;
        if automation.is_clicking {
            automation.is_clicking = false;
        }
    }

    if let Some((_, set_at)) = &automation.active_keycap
        && automation.virtual_elapsed.saturating_sub(*set_at) > automation.style.keycap_timeout
    {
        automation.active_keycap = None;
    }

    if automation.pending_bounds_id.is_some() {
        return None;
    }

    if automation.step_ready && !cursor_arrived {
        return None;
    }

    if automation.step_ready && cursor_arrived {
        let step = &automation.steps[automation.script_index];
        automation.is_clicking = true;
        if let Some(ref label) = step.keycap_label {
            automation.active_keycap = Some((label.clone(), automation.virtual_elapsed));
        }
        let msg = step.underlying_message.clone();
        automation.script_index += 1;
        automation.step_elapsed = Duration::ZERO;
        automation.step_ready = false;
        automation.step_timer = Instant::now();
        if let Some(m) = msg {
            return Some(AutomationTickResult::Message(m));
        }
        return None;
    }

    if automation.script_index < automation.steps.len() {
        let step = &automation.steps[automation.script_index];
        if automation.step_elapsed >= step.execution_delay {
            match &step.target {
                AutomationTarget::Widget(id) => {
                    automation.pending_bounds_id = Some(id.clone());
                    return Some(AutomationTickResult::FindBounds(id.clone()));
                }
                AutomationTarget::Pixel(point) => {
                    automation.current_pixel_target = *point;
                }
            }
            automation.step_elapsed = Duration::ZERO;
            automation.step_ready = true;
        }
    } else {
        automation.completed = true;
    }

    None
}

pub fn handle_bounds_resolved<Message>(
    automation: &mut AutomationState<Message>,
    rect_opt: Option<Rectangle>,
) {
    automation.pending_bounds_id = None;

    if let Some(rect) = rect_opt {
        automation.current_pixel_target = Point::new(rect.center_x(), rect.center_y());
    }

    automation.step_ready = true;
}

// --- Cursor geometry ---
/// Render height the cursor geometry is authored against. The overlay scales
/// every offset by the actual render height divided by this, so the pointer
/// keeps its proportions at 1080p, at a downscaled demo window and at a 4K
/// export alike.
pub const CURSOR_REFERENCE_HEIGHT: f32 = 1080.0;

/// Pixels per reference unit at [`CURSOR_REFERENCE_HEIGHT`]. The silhouette is
/// authored in a 20-unit box; this factor makes it a ~30 px tall pointer at
/// 1080p (a 19 px one read as a fleck next to the UI text and was smaller than
/// the wedge it replaced).
pub const CURSOR_UNIT_SCALE: f32 = 1.55;

/// Lower clamp of the resolution scale factor: a small interactive demo
/// window still gets a visible pointer instead of a few faint pixels.
pub const CURSOR_MIN_SCALE: f32 = 0.75;

/// Upper clamp of the resolution scale factor: a huge render must not paint a
/// screen-sized arrow. 2160p lands at 3.1, so a 4K export is not clamped.
pub const CURSOR_MAX_SCALE: f32 = 3.5;

/// Canonical arrow pointer in reference units: tip at the origin, x right,
/// y down. 11.0 wide × 19.4 tall (aspect ≈0.57, like an OS pointer, unlike the
/// old 28×34 wedge) with the shoulder notch at (4.3, 12.7) and the narrow tail
/// leg (6.8, 19.4) → (9.3, 18.1). Multiplied by [`CURSOR_UNIT_SCALE`] that is
/// ≈17.1 × 30.1 px at 1080p.
pub const CURSOR_POINTER_UNITS: [(f32, f32); 7] = [
    (0.0, 0.0),
    (0.0, 16.6),
    (4.3, 12.7),
    (6.8, 19.4),
    (9.3, 18.1),
    (7.0, 11.5),
    (11.0, 10.9),
];

/// Drop-shadow offset in reference units (≈2.0 px at 1080p) and its alpha:
/// enough to lift the white fill off a light background, far too little to
/// read as a second, dark body.
pub const CURSOR_SHADOW_OFFSET_UNITS: f32 = 1.3;
pub const CURSOR_SHADOW_ALPHA: f32 = 0.22;

/// Pixels per reference unit for a render `render_height` pixels tall, clamped
/// to [`CURSOR_MIN_SCALE`]..=[`CURSOR_MAX_SCALE`]. A missing/zero/NaN height
/// falls back to the reference scale so the overlay never degenerates into a
/// zero-size path.
pub fn cursor_render_scale(render_height: f32) -> f32 {
    if !render_height.is_finite() || render_height <= 0.0 {
        return CURSOR_UNIT_SCALE;
    }
    (render_height / CURSOR_REFERENCE_HEIGHT * CURSOR_UNIT_SCALE)
        .clamp(CURSOR_MIN_SCALE, CURSOR_MAX_SCALE)
}

/// The pointer silhouette scaled for a render `render_height` pixels tall, as
/// pixel offsets from the tip (which stays exactly on the automation target).
pub fn cursor_pointer_offsets(render_height: f32) -> [(f32, f32); 7] {
    let scale = cursor_render_scale(render_height);
    CURSOR_POINTER_UNITS.map(|(x, y)| (x * scale, y * scale))
}

/// How long the click ring is drawn for, measured in the emulator's own
/// `step_elapsed` (reset when a step fires) — never in wall-clock time, so a
/// headless export stays frame-exact.
pub const CURSOR_CLICK_PULSE: Duration = Duration::from_millis(280);

/// Radius (reference units) and alpha of the click ring `elapsed` after the
/// click fired, or `None` once the pulse is over (or before it starts). The
/// ring starts wide enough that its arc clears the pointer's body (a smaller
/// ring is swallowed by the arrow and reads as no feedback), expands while it
/// fades, and is fully transparent past the pulse so a resting click cannot
/// leave a hoop on screen.
pub fn cursor_click_ring(elapsed: Duration) -> Option<(f32, f32)> {
    let progress = elapsed.as_secs_f32() / CURSOR_CLICK_PULSE.as_secs_f32();
    if !(0.0..1.0).contains(&progress) {
        return None;
    }
    Some((9.0 + 15.0 * progress, 0.9 * (1.0 - progress)))
}

fn pointer_path(origin: Point, offsets: &[(f32, f32)]) -> canvas::Path {
    canvas::Path::new(|builder| {
        let mut points = offsets
            .iter()
            .map(|(x, y)| Point::new(origin.x + x, origin.y + y));
        if let Some(first) = points.next() {
            builder.move_to(first);
            for point in points {
                builder.line_to(point);
            }
            builder.close();
        }
    })
}

// --- Cursor overlay (canvas) ---
struct CursorOverlay {
    position: Point,
    /// How long ago the click at `position` fired. Drives the ring; the arrow
    /// itself never changes shape or colour on a click.
    click_elapsed: Option<Duration>,
    style: AutomationStyle,
}

impl<Message> canvas::Program<Message, Theme, Renderer> for CursorOverlay {
    type State = ();

    fn draw(
        &self,
        _state: &Self::State,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());

        let scale = cursor_render_scale(bounds.height);
        let offsets = cursor_pointer_offsets(bounds.height);
        let pointer = pointer_path(self.position, &offsets);

        // Soft drop shadow: the same silhouette offset down-right in a
        // translucent black, so the light fill keeps an edge on light themes
        // and gains depth on dark ones. One filled path, no blur. The offset
        // has to clear the outline width, or the shadow hides under it — but
        // it stays light, so it reads as depth rather than a second body.
        let shadow_offset = CURSOR_SHADOW_OFFSET_UNITS * scale;
        let shadow_path = pointer_path(
            Point::new(
                self.position.x + shadow_offset,
                self.position.y + shadow_offset,
            ),
            &offsets,
        );
        frame.fill(
            &shadow_path,
            Color::from_rgba(0.0, 0.0, 0.0, CURSOR_SHADOW_ALPHA),
        );

        // Click feedback as a separate ring at the tip, drawn behind the arrow
        // so the pointer itself stays untouched and the ring reads as a ripple
        // radiating out from it.
        if let Some((radius, alpha)) = self.click_elapsed.and_then(cursor_click_ring) {
            let ring_path = canvas::Path::circle(self.position, radius * scale);
            frame.stroke(
                &ring_path,
                canvas::Stroke::default()
                    .with_color(Color {
                        a: alpha,
                        ..self.style.click_color
                    })
                    .with_width(2.4 * scale),
            );
        }

        frame.fill(&pointer, self.style.cursor_color);
        frame.stroke(
            &pointer,
            canvas::Stroke::default()
                .with_color(self.style.outline_color)
                .with_width(self.style.cursor_outline_width * scale),
        );

        vec![frame.into_geometry()]
    }
}

// --- View wrapper ---
pub fn wrap_view<'a, Message>(
    base_view: Element<'a, Message, Theme, Renderer>,
    automation: &'a AutomationState<Message>,
) -> Element<'a, Message, Theme, Renderer>
where
    Message: Clone + 'a,
{
    let s = &automation.style;
    let mut layers: Vec<Element<'a, Message, Theme, Renderer>> = vec![base_view];

    if automation.completed {
        let completed_label = text(&s.completed_label)
            .size(22)
            .color(s.completed_text_color);
        let completed_bg = s.completed_bg;
        let completed_border = s.completed_border;
        let completed_border_w = s.completed_border_width;
        let completed_box = container(container(completed_label).padding([18, 36]).style(
            move |_theme| container::Style {
                background: Some(iced::Background::Color(completed_bg)),
                border: iced::Border {
                    color: completed_border,
                    width: completed_border_w,
                    radius: 8.0.into(),
                },
                ..container::Style::default()
            },
        ))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center);
        layers.push(completed_box.into());
    }

    if let Some((ref label, _)) = automation.active_keycap {
        let hud_bg = s.hud_bg;
        let hud_border = s.hud_border;
        let hud_border_w = s.hud_border_width;
        let hud = container(
            column![
                text(&s.active_label)
                    .size(11)
                    .color(Color::from_rgb(0.5, 0.5, 0.6)),
                text(label).size(16).color(Color::from_rgb(0.9, 0.9, 0.95)),
            ]
            .spacing(6),
        )
        .padding([12, 18])
        .style(move |_theme| container::Style {
            background: Some(iced::Background::Color(hud_bg)),
            border: iced::Border {
                color: hud_border,
                width: hud_border_w,
                radius: 8.0.into(),
            },
            shadow: Shadow {
                color: Color::from_rgba(0.0, 0.0, 0.0, 0.4),
                offset: Vector::new(0.0, 4.0),
                blur_radius: 12.0,
            },
            ..container::Style::default()
        });

        let cursor_in_br = automation.virtual_cursor.x > automation.window_width * 0.66
            && automation.virtual_cursor.y > automation.window_height * 0.66;
        let (align_x, align_y) = if cursor_in_br {
            (Alignment::Start, Alignment::End)
        } else {
            (Alignment::End, Alignment::End)
        };

        let positioned = container(hud)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(align_x)
            .align_y(align_y)
            .padding(24);

        layers.push(positioned.into());
    }

    let cursor = canvas(CursorOverlay {
        position: automation.virtual_cursor,
        click_elapsed: if automation.is_clicking {
            Some(automation.step_elapsed)
        } else {
            None
        },
        style: automation.style.clone(),
    })
    .width(Length::Fill)
    .height(Length::Fill);

    layers.push(cursor.into());
    stack(layers).into()
}

// --- JSON automation flow ---
#[derive(Debug, Deserialize, Clone)]
pub struct JsonAutomationFlow<Msg = serde_json::Value> {
    pub flow_name: String,
    pub steps: Vec<JsonAutomationStep<Msg>>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct JsonAutomationStep<Msg = serde_json::Value> {
    pub delay_ms: u64,
    pub target: JsonTarget,
    pub keycap_label: Option<String>,
    pub message: Msg,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(tag = "type", content = "value")]
pub enum JsonTarget {
    Coordinate { x: f32, y: f32 },
    Widget { id: String },
}

impl<Msg> JsonAutomationFlow<Msg>
where
    Msg: serde::de::DeserializeOwned,
{
    pub fn load_from_file<P: AsRef<Path>>(path: P) -> Result<Self, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(path)?;
        let flow = serde_json::from_reader(file)?;
        Ok(flow)
    }

    pub fn to_automation_steps<Message, MapTarget, MapMessage, MapKeycap>(
        &self,
        mut map_target: MapTarget,
        mut map_message: MapMessage,
        mut map_keycap: MapKeycap,
    ) -> Vec<AutomationStep<Message>>
    where
        MapTarget: FnMut(&JsonTarget) -> AutomationTarget,
        MapMessage: FnMut(&Msg) -> Option<Message>,
        MapKeycap: FnMut(&Msg, &Option<Message>) -> Option<String>,
    {
        self.steps
            .iter()
            .map(|step| {
                let target = map_target(&step.target);
                let underlying_message = map_message(&step.message);
                let keycap_label = step
                    .keycap_label
                    .clone()
                    .or_else(|| map_keycap(&step.message, &underlying_message));

                AutomationStep {
                    execution_delay: Duration::from_millis(step.delay_ms),
                    target,
                    underlying_message,
                    keycap_label,
                }
            })
            .collect()
    }
}

pub fn try_tick<State, Message, Update, MapBounds>(
    automation_opt: &mut Option<AutomationState<Message>>,
    state: &mut State,
    instant: Instant,
    mut update: Update,
    map_bounds: MapBounds,
) -> Task<Message>
where
    Message: Clone + Send + 'static,
    Update: FnMut(&mut State, Message) -> Task<Message>,
    MapBounds: FnMut(Option<Rectangle>) -> Message + Send + 'static,
{
    if let Some(automation) = automation_opt
        && automation.real_time
        && let Some(result) = handle_automation_tick(automation, instant)
    {
        match result {
            AutomationTickResult::Message(msg) => {
                return update(state, msg);
            }
            AutomationTickResult::FindBounds(id) => {
                return find_bounds_task(id).map(map_bounds);
            }
        }
    }
    Task::none()
}
pub fn build_automation_steps<Message>(
    flow: &JsonAutomationFlow<Message>,
    mut resolve_widget_id: impl FnMut(&str) -> String,
    mut format_keycap: impl FnMut(&Message) -> String,
) -> Vec<AutomationStep<Message>>
where
    Message: Clone + serde::de::DeserializeOwned,
{
    flow.to_automation_steps(
        |target| match target {
            JsonTarget::Coordinate { x, y } => AutomationTarget::Pixel(iced::Point::new(*x, *y)),
            JsonTarget::Widget { id } => {
                let resolved_id = resolve_widget_id(id.as_str());
                AutomationTarget::Widget(static_widget_id(resolved_id))
            }
        },
        |msg| Some(msg.clone()),
        |_, underlying_message| underlying_message.as_ref().map(&mut format_keycap),
    )
}

/// Caches the `&'static str` form of `s` process-globally so callers don't
/// re-leak the same string per frame. Use from view code to assign `Id`s that
/// need to match the automation engine's bbox-query IDs.
pub fn static_widget_id(s: String) -> iced::advanced::widget::Id {
    static CACHE: OnceLock<Mutex<HashMap<String, &'static str>>> = OnceLock::new();
    let mut cache = CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .expect("Mutex lock should not be poisoned");
    let id: &'static str = match cache.get(&s) {
        Some(existing) => existing,
        None => {
            let leaked: &'static str = Box::leak(s.clone().into_boxed_str());
            cache.insert(s, leaked);
            leaked
        }
    };
    iced::advanced::widget::Id::new(id)
}

pub trait AutomationMessage {
    fn is_automation(&self) -> bool;
    fn as_bounds(&self) -> Option<iced::Rectangle>;
    fn as_virtual_tick(&self) -> Option<std::time::Duration>;
}

pub trait VirtualTickMessage: Sized {
    fn virtual_tick(delta: Duration) -> Self;
}

pub trait AutomationContext<Message> {
    fn poll_background(&mut self) -> Task<Message> {
        Task::none()
    }
}

pub trait AutomationStateTrait<Message> {
    fn automation(&self) -> &Option<AutomationState<Message>>;
    fn automation_mut(&mut self) -> &mut Option<AutomationState<Message>>;

    fn is_automation_completed(&self) -> bool {
        self.automation().as_ref().is_some_and(|a| a.completed)
    }
}

pub fn handle_automation_message<State, Message>(
    state: &mut State,
    message: &Message,
    map_bounds: impl Fn(Option<iced::Rectangle>) -> Message + Send + Sync + 'static,
) -> Option<Task<Message>>
where
    State: AutomationStateTrait<Message> + AutomationContext<Message>,
    Message: AutomationMessage + Clone + Send + 'static,
{
    if !message.is_automation() {
        return None;
    }

    if let Some(delta) = message.as_virtual_tick() {
        let bg_task = state.poll_background();

        let automation_task = if let Some(automation) = state.automation_mut()
            && let Some(result) = handle_automation_virtual_tick(automation, delta)
        {
            match result {
                AutomationTickResult::Message(msg) => Task::done(msg),
                AutomationTickResult::FindBounds(id) => find_bounds_task(id).map(map_bounds),
            }
        } else {
            Task::none()
        };

        if let Some(automation) = state.automation() {
            VIRTUAL_CURSOR.with(|cell| *cell.borrow_mut() = automation.virtual_cursor);
        }

        return Some(Task::batch(vec![bg_task, automation_task]));
    }

    if let Some(automation) = state.automation_mut() {
        handle_bounds_resolved(automation, message.as_bounds());
    }
    Some(Task::none())
}

pub fn try_tick_state<State, Message, Update>(
    state: &mut State,
    instant: std::time::Instant,
    update: Update,
    map_bounds: impl Fn(Option<iced::Rectangle>) -> Message + Send + 'static,
) -> Task<Message>
where
    State: AutomationStateTrait<Message>,
    Message: Clone + Send + 'static,
    Update: FnMut(&mut State, Message) -> Task<Message>,
{
    let mut automation = state.automation_mut().take();
    let task = try_tick(&mut automation, state, instant, update, map_bounds);
    *state.automation_mut() = automation;
    task
}
