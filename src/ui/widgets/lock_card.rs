use crate::display::{Canvas, TextAlign};
use crate::ha::models::EntityState;
use crate::i18n;
use crate::ui::layout::{InteractiveTarget, Rect, UiAction};
use crate::ui::theme::Theme;
use crate::ui::widgets::{draw_card, draw_chip, draw_icon_badge, type_scale, PAD};

/// Door lock. A tap locks at once; unlocking takes a second tap while the
/// card shows its "tap again" prompt (`armed`), so a stray touch on a wall
/// panel never opens the door.
pub fn render_lock_card(
    canvas: &mut Canvas,
    theme: &Theme,
    rect: Rect,
    entity_id: &str,
    state: Option<&EntityState>,
    custom_title: Option<&str>,
    armed: bool,
    targets: &mut Vec<InteractiveTarget>,
) {
    let raw = state.map(|s| s.state.as_str()).unwrap_or("unavailable");
    let unavailable = matches!(raw, "unavailable" | "unknown");
    let open = is_unlocked(raw);
    let title = custom_title
        .map(|s| s.to_string())
        .or_else(|| state.map(|s| s.friendly_name()))
        .unwrap_or_else(|| i18n::t("lock.default_title"));

    // An unlocked door is the state worth noticing, so that is the active look.
    let tone = theme.warm;
    let active = !unavailable && (open || armed);
    draw_card(canvas, theme, rect, active.then_some(tone));

    let badge = 68.0;
    let bx = rect.x + PAD;
    let by = rect.y + (rect.h - badge) / 2.0;
    draw_icon_badge(canvas, theme, bx, by, badge, if open { "unlock" } else { "lock" }, if active { tone } else { theme.idle }, active);

    let status = i18n::t(match raw {
        _ if unavailable => "state.unavailable",
        _ if armed => "lock.confirm",
        "locking" => "lock.locking",
        "unlocking" => "lock.unlocking",
        "jammed" => "lock.jammed",
        _ if open => "lock.unlocked",
        _ => "lock.locked",
    });

    let mut right = rect.x + rect.w - PAD;
    if !unavailable {
        // The action chip is a hint, not the button (the whole card is):
        // half-width cards on a 6" screen keep the room for the name.
        if rect.w >= 420.0 {
            let label = i18n::t(if open { "lock.action_lock" } else { "lock.action_unlock" });
            let chip_w = draw_chip(canvas, theme, right, by + (badge - 38.0) / 2.0, &label, if active { tone } else { theme.idle }, armed);
            right -= chip_w + 14.0;
        }
        targets.push(InteractiveTarget { rect, action: UiAction::LockTap(entity_id.to_string()) });
    }

    let tx = bx + badge + 18.0;
    let text_w = (right - tx).max(10.0);
    let title_size = if canvas.measure_text(&title, type_scale::TITLE, true).0 <= text_w { type_scale::TITLE } else { 24.0 };
    canvas.draw_text_fit(&title, tx, by + 2.0 + (type_scale::TITLE - title_size) * 0.6, title_size, true, theme.text, TextAlign::Left, text_w);
    let sub_color = if active { theme.text } else { theme.text_muted };
    let status_size = if canvas.measure_text(&status, type_scale::LABEL, armed).0 <= text_w { type_scale::LABEL } else { 19.0 };
    canvas.draw_text_fit(&status, tx, by + 40.0 + (type_scale::LABEL - status_size) * 0.6, status_size, armed, sub_color, TextAlign::Left, text_w);
}

/// States in which the bolt is not thrown.
pub fn is_unlocked(state: &str) -> bool {
    matches!(state, "unlocked" | "open" | "opening")
}
