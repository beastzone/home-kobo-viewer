use crate::config::{PlanDevice, PlanRoom};
use crate::display::{Canvas, TextAlign};
use crate::ha::models::EntityState;
use crate::i18n;
use crate::ui::layout::{InteractiveTarget, Rect, UiAction};
use crate::ui::theme::Theme;
use crate::ui::widgets::lock_card::is_unlocked;
use crate::ui::widgets::{draw_icon_badge, format_number, type_scale};
use std::collections::HashMap;

pub const PLAN_TITLE_H: f32 = 38.0;
/// Tallest a plan is drawn however square its grid
const PLAN_MAX_H: f32 = 760.0;
const PLAN_MIN_H: f32 = 240.0;
const WALL: f32 = 3.0;

/// (natural, minimum) height of a plan drawn `w` wide.
pub fn floorplan_heights(w: f32, cols: u32, rows: u32, has_title: bool) -> (f32, f32) {
    let t = if has_title { PLAN_TITLE_H } else { 0.0 };
    let natural = (w * rows.max(1) as f32 / cols.max(1) as f32).clamp(PLAN_MIN_H, PLAN_MAX_H);
    (t + natural, t + PLAN_MIN_H)
}

/// Domains a room tap switches. Deliberately short: a tap on a room must
/// never unlock a door, start a vacuum or change the heating.
pub fn room_switchable(entity_id: &str) -> bool {
    matches!(entity_id.split('.').next(), Some("light" | "switch" | "fan" | "input_boolean"))
}

fn in_room(room: &PlanRoom, d: &PlanDevice) -> bool {
    d.x >= room.x && d.x < room.x + room.w && d.y >= room.y && d.y < room.y + room.h
}

fn default_icon(entity_id: &str, state: &str) -> &'static str {
    match entity_id.split('.').next().unwrap_or("") {
        "light" => "lightbulb",
        "switch" | "input_boolean" => "plug",
        "fan" => "fan",
        "lock" => {
            if is_unlocked(state) {
                "unlock"
            } else {
                "lock"
            }
        }
        "media_player" | "remote" => "tv",
        "climate" => "thermometer",
        "cover" => "door",
        "scene" | "script" | "button" | "input_button" => "play",
        _ => "power",
    }
}

/// What a tap on a device does, by the kind of thing it is.
fn device_action(entity_id: &str) -> Option<UiAction> {
    let id = entity_id.to_string();
    match entity_id.split('.').next().unwrap_or("") {
        "light" | "switch" | "fan" | "input_boolean" | "automation" | "media_player" | "remote" => Some(UiAction::ToggleEntity(id)),
        "lock" => Some(UiAction::LockTap(id)),
        "scene" | "script" | "button" | "input_button" => Some(UiAction::ActivateScene(id)),
        _ => None,
    }
}

/// Overhead plan of the home: rooms as rectangles on a grid, devices placed
/// inside them. A tap on a device works that device; a tap on the room around
/// it switches every light, switch and fan in the room (all off if any is on,
/// otherwise all on).
#[allow(clippy::too_many_arguments)]
pub fn render_floorplan_card(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    custom_title: Option<&str>,
    cols: u32,
    rows: u32,
    rooms: &[PlanRoom],
    devices: &[PlanDevice],
    states: &HashMap<String, EntityState>,
    unlock_armed: Option<&str>,
    targets: &mut Vec<InteractiveTarget>,
) {
    let mut top = rect.y;
    if let Some(title) = custom_title {
        canvas.draw_text(title, rect.x + 4.0, top + 2.0, type_scale::BODY, true, theme.text_muted, TextAlign::Left);
        top += PLAN_TITLE_H;
    }
    let (cols, rows) = (cols.max(1) as f32, rows.max(1) as f32);
    let avail_h = (rect.y + rect.h - top).max(1.0);
    // Square cells, the whole grid centred in the space it was given
    let cell = (rect.w / cols).min(avail_h / rows);
    let ox = rect.x + (rect.w - cell * cols) / 2.0;
    let oy = top + (avail_h - cell * rows) / 2.0;
    let state_of = |id: &str| states.get(id).map(|s| s.state.as_str()).unwrap_or("unavailable");
    let is_on = |id: &str| states.get(id).map(|s| s.is_on()).unwrap_or(false);

    // Rooms first, so that devices are drawn over them and win a tap.
    let tone = theme.warm;
    for room in rooms {
        let r = Rect::new(ox + room.x * cell, oy + room.y * cell, room.w * cell, room.h * cell);
        let inside: Vec<&str> = devices
            .iter()
            .filter(|d| in_room(room, d) && room_switchable(&d.entity_id) && state_of(&d.entity_id) != "unavailable")
            .map(|d| d.entity_id.as_str())
            .collect();
        let lit = inside.iter().any(|id| is_on(id));
        canvas.fill_rect(r.x, r.y, r.w, r.h, if lit { tone.tint } else { theme.surface });
        if !inside.is_empty() {
            targets.push(InteractiveTarget {
                rect: r,
                action: UiAction::Service {
                    domain: "homeassistant".to_string(),
                    service: if lit { "turn_off" } else { "turn_on" }.to_string(),
                    data: serde_json::json!({ "entity_id": inside }),
                },
            });
        }
    }
    // Walls after every fill: neighbours share one line instead of half each.
    for room in rooms {
        let r = Rect::new(ox + room.x * cell, oy + room.y * cell, room.w * cell, room.h * cell);
        canvas.stroke_rounded_rect(r.x, r.y, r.w, r.h, 1.0, WALL, theme.text);
        if !room.name.is_empty() {
            let size = if r.w >= 150.0 { type_scale::LABEL } else { 18.0 };
            canvas.draw_text_fit(&room.name, r.x + 10.0, r.y + 8.0, size, true, theme.text, TextAlign::Left, (r.w - 20.0).max(10.0));
        }
    }

    let d = (cell * 1.7).clamp(40.0, 60.0);
    for dev in devices {
        let (cx, cy) = (ox + dev.x * cell, oy + dev.y * cell);
        let state = state_of(&dev.entity_id);
        let domain = dev.entity_id.split('.').next().unwrap_or("");
        let unavailable = matches!(state, "unavailable" | "unknown") && !matches!(domain, "scene" | "script" | "button" | "input_button");

        if matches!(domain, "sensor" | "binary_sensor") && dev.icon.is_none() {
            // A reading rather than a button: the value itself, on a small plate
            let unit = states.get(&dev.entity_id).and_then(|s| s.get_attr_str("unit_of_measurement")).unwrap_or("");
            let text = format!("{}{}", format_number(state), if unit.starts_with('°') { "°" } else { unit });
            let size = 22.0;
            let w = canvas.measure_text(&text, size, true).0 + 20.0;
            canvas.fill_rounded_rect(cx - w / 2.0, cy - 17.0, w, 34.0, 17.0, theme.surface);
            canvas.stroke_rounded_rect(cx - w / 2.0 + 1.0, cy - 16.0, w - 2.0, 32.0, 16.0, 2.0, theme.border_strong);
            canvas.draw_text(&text, cx, cy - size * 0.58, size, true, theme.text, TextAlign::Center);
            continue;
        }

        let armed = unlock_armed == Some(dev.entity_id.as_str());
        let active = !unavailable
            && match domain {
                "lock" => armed || is_unlocked(state),
                _ => is_on(&dev.entity_id),
            };
        let icon = dev.icon.as_deref().unwrap_or_else(|| default_icon(&dev.entity_id, state));
        // An opaque disc under the badge keeps it legible on a lit room
        canvas.fill_circle(cx, cy, d / 2.0 + 2.0, theme.surface);
        draw_icon_badge(canvas, theme, cx - d / 2.0, cy - d / 2.0, d, icon, if active { tone } else { theme.idle }, active);
        if unavailable {
            // Struck through: there, but not answering
            canvas.draw_line(cx - d * 0.3, cy + d * 0.3, cx + d * 0.3, cy - d * 0.3, 3.0, theme.text_muted);
            continue;
        }
        if armed {
            let text = i18n::t("lock.confirm");
            let size = 18.0;
            let w = canvas.measure_text(&text, size, true).0 + 16.0;
            let lx = (cx - w / 2.0).clamp(rect.x, (rect.x + rect.w - w).max(rect.x));
            canvas.fill_rounded_rect(lx, cy + d / 2.0 + 4.0, w, 28.0, 14.0, theme.text);
            canvas.draw_text(&text, lx + w / 2.0, cy + d / 2.0 + 8.0, size, true, theme.surface, TextAlign::Center);
        }
        if let Some(action) = device_action(&dev.entity_id) {
            targets.push(InteractiveTarget { rect: Rect::new(cx - d / 2.0, cy - d / 2.0, d, d).inset(-5.0), action });
        }
    }
}
