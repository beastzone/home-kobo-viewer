use crate::display::{Canvas, TextAlign};
use crate::ha::models::EntityState;
use crate::i18n;
use crate::ui::layout::{InteractiveTarget, Rect, UiAction};
use crate::ui::theme::Theme;
use crate::ui::widgets::{draw_card, draw_chip, draw_round_button, type_scale, PAD};

const HEADER_H: f32 = 52.0;
/// Side of the D-pad and of the 3x3 key grid at full size
const BLOCK: f32 = 268.0;
pub const REMOTE_NATURAL_H: f32 = PAD + HEADER_H + BLOCK + PAD;
pub const REMOTE_MIN_H: f32 = PAD + HEADER_H + 204.0 + PAD;

/// TV remote for a Home Assistant `remote` entity: a D-pad on the left and a
/// grid of keys on the right. Key names are the Android TV / Google TV ones
/// that the "Android TV Remote" integration accepts.
pub fn render_remote_card(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    entity_id: &str,
    state: Option<&EntityState>,
    custom_title: Option<&str>,
    targets: &mut Vec<InteractiveTarget>,
) {
    let raw = state.map(|s| s.state.as_str()).unwrap_or("unavailable");
    let unavailable = matches!(raw, "unavailable" | "unknown");
    let is_on = raw == "on";
    let title = custom_title
        .map(|s| s.to_string())
        .or_else(|| state.map(|s| s.friendly_name()))
        .unwrap_or_else(|| i18n::t("remote.default_title"));
    let tone = theme.media;

    draw_card(canvas, theme, rect, is_on.then_some(tone));

    let status = i18n::t(if unavailable {
        "state.unavailable"
    } else if is_on {
        "state.on"
    } else {
        "state.off"
    });
    let chip_w = draw_chip(canvas, theme, rect.x + rect.w - PAD, rect.y + PAD, &status, if is_on { tone } else { theme.idle }, is_on);
    canvas.draw_icon("tv", rect.x + PAD, rect.y + PAD + 2.0, 34.0, if is_on { tone.solid } else { theme.text_muted }, 2.2);
    canvas.draw_text_fit(
        &title,
        rect.x + PAD + 48.0,
        rect.y + PAD + 3.0,
        type_scale::TITLE,
        true,
        theme.text,
        TextAlign::Left,
        rect.w - 2.0 * PAD - 48.0 - chip_w - 14.0,
    );
    if unavailable {
        return;
    }

    // Both blocks shrink together to fit a short or narrow card.
    let body_y = rect.y + PAD + HEADER_H;
    let avail_h = rect.y + rect.h - PAD - body_y;
    let gap = 28.0;
    let block = BLOCK.min(avail_h).min((rect.w - 2.0 * PAD - gap) / 2.0).max(120.0);
    let k = block / BLOCK;
    let spare = (rect.w - 2.0 * PAD - 2.0 * block - gap).max(0.0);
    let pad_x = rect.x + PAD + spare / 3.0;
    let grid_x = pad_x + block + gap + spare / 3.0;
    let top = body_y + (avail_h - block).max(0.0) / 2.0;

    let key = |command: &str| UiAction::Service {
        domain: "remote".to_string(),
        service: "send_command".to_string(),
        data: serde_json::json!({ "entity_id": entity_id, "command": command }),
    };

    // D-pad: four arrows around OK
    let (cx, cy) = (pad_x + block / 2.0, top + block / 2.0);
    let arrow = 76.0 * k;
    let ok = 92.0 * k;
    let reach = (block - arrow) / 2.0;
    for (dx, dy, icon, command) in [
        (0.0, -1.0, "chevron_up", "DPAD_UP"),
        (0.0, 1.0, "chevron_down", "DPAD_DOWN"),
        (-1.0, 0.0, "chevron_left", "DPAD_LEFT"),
        (1.0, 0.0, "chevron_right", "DPAD_RIGHT"),
    ] {
        let r = Rect::new(cx + dx * reach - arrow / 2.0, cy + dy * reach - arrow / 2.0, arrow, arrow);
        draw_round_button(canvas, theme, r, icon, tone, false);
        targets.push(InteractiveTarget { rect: r.inset(-4.0), action: key(command) });
    }
    canvas.fill_circle(cx, cy, ok / 2.0, tone.solid);
    let ok_size = 30.0 * k;
    canvas.draw_text("OK", cx, cy - ok_size * 0.58, ok_size, true, tone.on_solid, TextAlign::Center);
    targets.push(InteractiveTarget { rect: Rect::new(cx - ok / 2.0, cy - ok / 2.0, ok, ok), action: key("DPAD_CENTER") });

    // Keys: power / home / back, volume, playback
    let power = UiAction::Service {
        domain: "remote".to_string(),
        service: "toggle".to_string(),
        data: serde_json::json!({ "entity_id": entity_id }),
    };
    let keys = [
        ("power", power, is_on),
        ("home", key("HOME"), false),
        ("back", key("BACK"), false),
        ("volume_down", key("VOLUME_DOWN"), false),
        ("volume_mute", key("VOLUME_MUTE"), false),
        ("volume_up", key("VOLUME_UP"), false),
        ("skip_previous", key("MEDIA_PREVIOUS"), false),
        ("play_pause", key("MEDIA_PLAY_PAUSE"), false),
        ("skip_next", key("MEDIA_NEXT"), false),
    ];
    let d = 76.0 * k;
    let step = (block - d) / 2.0;
    for (i, (icon, action, primary)) in keys.into_iter().enumerate() {
        let r = Rect::new(grid_x + (i % 3) as f32 * step, top + (i / 3) as f32 * step, d, d);
        draw_round_button(canvas, theme, r, icon, tone, primary);
        targets.push(InteractiveTarget { rect: r.inset(-4.0), action });
    }
}
