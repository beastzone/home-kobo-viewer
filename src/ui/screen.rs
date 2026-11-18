use crate::config::{AppConfig, CardConfig, SceneItemConfig};
use crate::display::canvas::ui_size;
use crate::display::Canvas;
use crate::ha::models::EntityState;
use crate::ui::layout::{InteractiveTarget, Rect, UiAction};
use crate::ui::theme::Theme;
use crate::ui::widgets::climate_card::CLIMATE_WIDE;
use crate::ui::widgets::floorplan_card::floorplan_heights;
use crate::ui::widgets::header::{read_device_status, HeaderStatus, SIDE_PAD};
use crate::ui::widgets::remote_card::{REMOTE_MIN_H, REMOTE_NATURAL_H};
use crate::ui::widgets::scene_card::SCENES_TITLE_H;
use crate::ui::widgets::sensor_card::{sensor_grid_columns, sensor_grid_height, TILE_GAP};
use crate::ui::widgets::*;
use chrono::{DateTime, Utc};
use std::collections::HashMap;

const GAP: f32 = 18.0;
const BOTTOM_PAD: f32 = 28.0;
/// Space kept at the bottom for the sleep-mode note
const SLEEP_NOTE_H: f32 = 124.0;
/// Width of the left column in landscape (weather + sensor grid)
const LANDSCAPE_LEFT_W: f32 = 440.0;
/// Narrowest half-width card the right column may hold before the landscape
/// layout falls back to a single full-width column
const LANDSCAPE_MIN_HALF_W: f32 = 300.0;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Span {
    Full,
    Half,
}

fn span_of(card: &CardConfig) -> Span {
    match card {
        CardConfig::Weather { .. }
        | CardConfig::ScenesRow { .. }
        | CardConfig::SensorGrid { .. }
        | CardConfig::MediaPlayer { .. }
        | CardConfig::Remote { .. }
        | CardConfig::Floorplan { .. } => Span::Full,
        _ => Span::Half,
    }
}

/// (natural, minimum) height of a card rendered at width `w`.
fn heights(card: &CardConfig, w: f32) -> (f32, f32) {
    match card {
        CardConfig::Weather { .. } => {
            if w >= 640.0 {
                (312.0, 300.0)
            } else {
                (318.0, 306.0)
            }
        }
        CardConfig::ScenesRow { title, .. } => {
            let t = if title.is_some() { SCENES_TITLE_H } else { 0.0 };
            (t + 124.0, t + 78.0)
        }
        CardConfig::Light { show_brightness, .. } => {
            if *show_brightness {
                (178.0, 150.0)
            } else {
                (150.0, 118.0)
            }
        }
        CardConfig::Switch { .. } => (150.0, 118.0),
        CardConfig::Climate { .. } => {
            if w >= CLIMATE_WIDE {
                (160.0, 136.0)
            } else {
                (236.0, 206.0)
            }
        }
        CardConfig::Sensor { show_graph, .. } => {
            if *show_graph {
                (250.0, 214.0)
            } else {
                (176.0, 150.0)
            }
        }
        CardConfig::SensorGrid { title, sensors, .. } => {
            let nat = sensor_grid_height(w, sensors.len(), title.is_some());
            let rows = sensors.len().div_ceil(sensor_grid_columns(w, sensors.len())).max(1);
            (nat, nat - rows as f32 * 10.0)
        }
        CardConfig::MediaPlayer { .. } => (180.0, 140.0),
        CardConfig::Floorplan { title, cols, rows, .. } => floorplan_heights(w, *cols, *rows, title.is_some()),
        CardConfig::Lock { .. } => (150.0, 118.0),
        CardConfig::Remote { .. } => (REMOTE_NATURAL_H, REMOTE_MIN_H),
        CardConfig::Scene { .. } => (112.0, 86.0),
    }
}

/// Flow cards into rows of one full-width or two half-width cards, shrinking
/// rows (down to their minimum) to fit `avail_h`. A half card without a
/// partner is stretched to full width. Rows that still don't fit are dropped
/// rather than drawn on top of each other.
fn layout_zone(cards: &[&CardConfig], x: f32, y: f32, w: f32, avail_h: f32) -> Vec<(usize, Rect)> {
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut pending: Option<usize> = None;
    for (i, card) in cards.iter().enumerate() {
        match span_of(card) {
            Span::Full => {
                if let Some(p) = pending.take() {
                    rows.push(vec![p]);
                }
                rows.push(vec![i]);
            }
            Span::Half => match pending.take() {
                Some(p) => rows.push(vec![p, i]),
                None => pending = Some(i),
            },
        }
    }
    if let Some(p) = pending {
        rows.push(vec![p]);
    }

    let half_w = (w - GAP) / 2.0;
    let row_heights: Vec<(f32, f32)> = rows
        .iter()
        .map(|row| {
            let item_w = if row.len() == 2 { half_w } else { w };
            row.iter()
                .map(|&i| heights(cards[i], item_w))
                .fold((0.0f32, 0.0f32), |(n, m), (cn, cm)| (n.max(cn), m.max(cm)))
        })
        .collect();

    let gaps = GAP * rows.len().saturating_sub(1) as f32;
    let natural: f32 = row_heights.iter().map(|h| h.0).sum::<f32>() + gaps;
    let slack: f32 = row_heights.iter().map(|h| h.0 - h.1).sum();
    let k = if natural > avail_h && slack > 0.0 { ((natural - avail_h) / slack).min(1.0) } else { 0.0 };

    let mut out = Vec::new();
    let mut shown = 0;
    let mut cur_y = y;
    for (row, (nat, min)) in rows.iter().zip(row_heights) {
        let h = nat - k * (nat - min);
        if cur_y + h > y + avail_h + 1.0 {
            let hidden: usize = rows.iter().skip(shown).map(|r| r.len()).sum();
            tracing::debug!("Page content does not fit the screen; {} card(s) hidden", hidden);
            break;
        }
        shown += 1;
        if row.len() == 2 {
            out.push((row[0], Rect::new(x, cur_y, half_w, h)));
            out.push((row[1], Rect::new(x + half_w + GAP, cur_y, half_w, h)));
        } else {
            out.push((row[0], Rect::new(x, cur_y, w, h)));
        }
        cur_y += h + GAP;
    }
    out
}

/// Header height when page tabs are shown (see `render_header`)
pub const HEADER_WITH_TABS: f32 = 206.0;
pub const HEADER_WITHOUT_TABS: f32 = 122.0;

fn zones_for<'a>(cards: &'a [CardConfig], cw: f32, ch: f32, top: f32) -> Vec<(&'a CardConfig, Rect)> {
    let avail_h = ch - top - BOTTOM_PAD;
    let content_w = cw - 2.0 * SIDE_PAD;
    let all: Vec<&CardConfig> = cards.iter().collect();
    if cw > ch {
        let (left, right): (Vec<&CardConfig>, Vec<&CardConfig>) =
            cards.iter().partition(|c| matches!(c, CardConfig::Weather { .. } | CardConfig::SensorGrid { .. }));
        let right_w = content_w - LANDSCAPE_LEFT_W - GAP;
        // On small landscape screens (Nia, Glo, Aura: 1024 px) the right
        // column would squeeze two cards per row into ~240 px; use the full
        // width instead (issue #4).
        if !left.is_empty() && !right.is_empty() && (right_w - GAP) / 2.0 >= LANDSCAPE_MIN_HALF_W {
            let right_x = SIDE_PAD + LANDSCAPE_LEFT_W + GAP;
            let mut v: Vec<(&CardConfig, Rect)> = layout_zone(&left, SIDE_PAD, top, LANDSCAPE_LEFT_W, avail_h)
                .into_iter()
                .map(|(i, r)| (left[i], r))
                .collect();
            v.extend(layout_zone(&right, right_x, top, right_w, avail_h).into_iter().map(|(i, r)| (right[i], r)));
            return v;
        }
    }
    layout_zone(&all, SIDE_PAD, top, content_w, avail_h)
        .into_iter()
        .map(|(i, r)| (all[i], r))
        .collect()
}

/// How many of `cards` would not fit on a `w`x`h` screen.
pub fn hidden_card_count(cards: &[CardConfig], w: u32, h: u32, with_tabs: bool) -> usize {
    let top = if with_tabs { HEADER_WITH_TABS } else { HEADER_WITHOUT_TABS };
    let (w, h) = ui_size(w, h);
    cards.len() - zones_for(cards, w, h, top).len()
}

pub struct ScreenRenderer {
    pub theme: Theme,
    pub targets: Vec<InteractiveTarget>,
    /// Cards on the last rendered page that did not fit
    pub hidden_cards: usize,
    /// Time of the last successful Home Assistant refresh
    pub last_update: Option<DateTime<Utc>>,
    /// Frontlight state for the header button (None hides the button)
    pub frontlight: Option<bool>,
    /// While in sleep mode: the time of the next timed refresh, shown in a
    /// note at the bottom of the screen
    pub sleep_until: Option<String>,
    /// The owner's words for the sleep note, if any, and whether the note
    /// names the next refresh
    pub sleep_message: Option<String>,
    pub sleep_hide_next: bool,
    /// Draw the "battery empty" screen instead of the dashboard
    pub battery_empty: bool,
    /// Weather day open as a full page: (weather entity, that day's forecast time)
    pub weather_detail: Option<(String, String)>,
    /// Lock waiting for its second, confirming tap before it unlocks
    pub unlock_armed: Option<String>,
    /// Pixels per UI unit on the last rendered canvas
    scale: f32,
}

impl ScreenRenderer {
    pub fn new(config: &AppConfig) -> Self {
        Self {
            theme: Theme::from_config(&config.theme),
            targets: Vec::new(),
            hidden_cards: 0,
            last_update: None,
            frontlight: None,
            sleep_until: None,
            sleep_message: config.device.sleep_message.as_deref().map(str::trim).filter(|m| !m.is_empty()).map(str::to_string),
            sleep_hide_next: config.device.sleep_hide_next,
            battery_empty: false,
            unlock_armed: None,
            weather_detail: None,
            scale: 1.0,
        }
    }

    pub fn render(
        &mut self,
        canvas: &mut Canvas,
        config: &AppConfig,
        active_page_idx: usize,
        states: &HashMap<String, EntityState>,
        is_live: bool,
    ) {
        self.targets.clear();
        self.hidden_cards = 0;
        self.scale = canvas.scale;
        canvas.clear(self.theme.bg);

        if self.battery_empty {
            self.render_battery_empty(canvas);
            return;
        }

        let (battery, charging, wifi_up) = read_device_status();
        let wifi_up = crate::ui::widgets::header::shown_wifi(wifi_up, self.sleep_until.is_some(), crate::power::wifi_switched_off());
        let status = HeaderStatus {
            is_live,
            is_demo: config.home_assistant.token.trim().is_empty(),
            last_update: self.last_update,
            battery,
            charging,
            wifi_up,
            asleep: self.sleep_until.is_some(),
            frontlight: self.frontlight,
        };

        let header_bottom = render_header(
            canvas,
            &self.theme,
            &config.theme,
            &config.pages,
            active_page_idx,
            &status,
            &mut self.targets,
        );

        let Some(page) = config.pages.get(active_page_idx) else { return };

        // The sleep note replaces the page tabs (useless while asleep); with a
        // single page there are no tabs, so room is made at the bottom instead.
        let has_tabs = config.pages.len() > 1;
        let reserve = if self.sleep_until.is_some() && !has_tabs { SLEEP_NOTE_H } else { 0.0 };
        let (w, h) = canvas.ui_size();
        let placements = zones_for(&page.cards, w, h - reserve, header_bottom);
        self.hidden_cards = page.cards.len() - placements.len();

        if let Some((entity, day)) = self.weather_detail.clone() {
            // A detail page takes the place of the cards; header and tabs stay.
            self.hidden_cards = 0;
            let now = crate::ui::widgets::header::now_local(&config.theme);
            let area = Rect::new(SIDE_PAD, header_bottom, w - 2.0 * SIDE_PAD, (h - reserve - header_bottom - SIDE_PAD).max(120.0));
            render_weather_detail(canvas, &self.theme, area, states.get(&entity), &day, *now.offset(), config.theme.time_format == "12h", &mut self.targets);
        } else {
            for (card, rect) in placements {
                self.render_card(canvas, card, rect, states);
            }
        }

        if let Some(next) = self.sleep_until.clone() {
            let bar = if has_tabs {
                // Same place and size as the tab row drawn by the header
                Rect::new(SIDE_PAD, HEADER_WITH_TABS - 84.0, w - 2.0 * SIDE_PAD, 64.0)
            } else {
                Rect::new(SIDE_PAD, h - SLEEP_NOTE_H + 4.0, w - 2.0 * SIDE_PAD, SLEEP_NOTE_H - 28.0)
            };
            self.render_sleep_note(canvas, bar, &next);
            // Nothing is tappable while the device sleeps.
            self.targets.clear();
        }
    }

    /// The whole screen, not a note on the dashboard: it can stay up for days
    /// and old values shouldn't look current. E-ink keeps it without power.
    fn render_battery_empty(&self, canvas: &mut Canvas) {
        use crate::display::TextAlign;
        use crate::i18n;
        let theme = &self.theme;
        let (w, h) = canvas.ui_size();
        let icon = 220.0;
        let top = h / 2.0 - icon / 2.0 - 90.0;
        let max_w = w - 2.0 * SIDE_PAD;
        canvas.draw_icon("battery_empty", w / 2.0 - icon / 2.0, top, icon, theme.text, 3.0);
        canvas.draw_text_fit(&i18n::t("sleep.battery_empty"), w / 2.0, top + icon + 24.0, 60.0, true, theme.text, TextAlign::Center, max_w);
        canvas.draw_text_fit(&i18n::t("sleep.charge_hint"), w / 2.0, top + icon + 108.0, 32.0, false, theme.text, TextAlign::Center, max_w);
    }

    fn render_sleep_note(&self, canvas: &mut Canvas, bar: Rect, next: &str) {
        use crate::display::TextAlign;
        use crate::i18n;
        let theme = &self.theme;
        canvas.fill_rounded_rect(bar.x, bar.y, bar.w, bar.h, bar.h / 2.0, theme.text);
        let icon = (bar.h * 0.5).min(40.0);
        let ix = bar.x + bar.h * 0.4;
        canvas.draw_icon("moon", ix, bar.y + (bar.h - icon) / 2.0, icon, theme.surface, 2.2);
        let tx = ix + icon + 18.0;
        let max_w = bar.x + bar.w - bar.h * 0.4 - tx;
        // The owner's own message takes the place of "Sleep mode" and of the
        // wake hint; the time of the next refresh can be left out as well.
        let custom = self.sleep_message.as_deref();
        let time = i18n::tf("sleep.next", &[("time", next)]);
        if bar.h < 80.0 {
            // One line: "Sleep mode · Next update 22:56" then the hint, muted
            let (line, size) = match (custom, self.sleep_hide_next) {
                (None, false) => sleep_line(canvas, next, max_w, 24.0),
                (None, true) => (i18n::t("sleep.title"), 24.0),
                (Some(message), true) => (message.to_string(), 24.0),
                (Some(message), false) => {
                    // The time gives way before the message is cut short
                    let both = format!("{} · {}", message, time);
                    let fits = canvas.measure_text(&both, 24.0, true).0 <= max_w;
                    (if fits { both } else { message.to_string() }, 24.0)
                }
            };
            let tw = canvas.draw_text_fit(&line, tx, bar.y + bar.h / 2.0 - size / 2.0, size, true, theme.surface, TextAlign::Left, max_w);
            let hint_x = tx + tw + 18.0;
            if custom.is_none() && hint_x + 120.0 < tx + max_w {
                let muted = theme.surface.mix(theme.text, 0.35);
                canvas.draw_text_fit(&i18n::t("sleep.wake_hint"), hint_x, bar.y + bar.h / 2.0 - 11.0, 22.0, false, muted, TextAlign::Left, tx + max_w - hint_x);
            }
        } else {
            let (line, size, second) = match (custom, self.sleep_hide_next) {
                (None, false) => {
                    let (line, size) = sleep_line(canvas, next, max_w, 26.0);
                    (line, size, Some(i18n::t("sleep.wake_hint")))
                }
                (None, true) => (i18n::t("sleep.title"), 26.0, Some(i18n::t("sleep.wake_hint"))),
                (Some(message), false) => (message.to_string(), 26.0, Some(time)),
                (Some(message), true) => (message.to_string(), 26.0, None),
            };
            match second {
                Some(second) => {
                    canvas.draw_text_fit(&line, tx, bar.y + 16.0 + (26.0 - size) / 2.0, size, true, theme.surface, TextAlign::Left, max_w);
                    canvas.draw_text_fit(&second, tx, bar.y + 52.0, 22.0, false, theme.surface, TextAlign::Left, max_w);
                }
                None => {
                    canvas.draw_text_fit(&line, tx, bar.y + bar.h / 2.0 - size / 2.0, size, true, theme.surface, TextAlign::Left, max_w);
                }
            }
        }
    }

    fn render_card(&mut self, canvas: &mut Canvas, card: &CardConfig, rect: Rect, states: &HashMap<String, EntityState>) {
        let theme = &self.theme;
        let targets = &mut self.targets;
        match card {
            CardConfig::Weather { entity_id, title, forecast_days } => {
                render_weather_card(canvas, theme, rect, entity_id, states.get(entity_id), title.as_deref(), *forecast_days, targets)
            }
            CardConfig::ScenesRow { title, scenes } => render_scenes_row(canvas, theme, rect, title.as_deref(), scenes, targets),
            CardConfig::SensorGrid { title, sensors, large_value } => {
                render_sensor_grid(canvas, theme, rect, title.as_deref(), sensors, *large_value, states, targets)
            }
            CardConfig::MediaPlayer { entity_id, title } => {
                render_media_card(canvas, theme, rect, entity_id, states.get(entity_id), title.as_deref(), targets)
            }
            CardConfig::Climate { entity_id, title, step } => {
                render_climate_card(canvas, theme, rect, entity_id, states.get(entity_id), title.as_deref(), *step, targets)
            }
            CardConfig::Light { entity_id, title, subtitle, icon, show_brightness } => render_light_card(
                canvas,
                theme,
                rect,
                entity_id,
                states.get(entity_id),
                title.as_deref(),
                subtitle.as_deref(),
                icon.as_deref(),
                *show_brightness,
                targets,
            ),
            CardConfig::Switch { entity_id, title, subtitle, icon } => render_switch_card(
                canvas,
                theme,
                rect,
                entity_id,
                states.get(entity_id),
                title.as_deref(),
                subtitle.as_deref(),
                icon.as_deref(),
                targets,
            ),
            CardConfig::Sensor { entity_id, title, unit, icon, show_graph, large_value } => render_sensor_card(
                canvas,
                theme,
                rect,
                entity_id,
                states.get(entity_id),
                title.as_deref(),
                unit.as_deref(),
                icon.as_deref(),
                *show_graph,
                *large_value,
                targets,
            ),
            CardConfig::Floorplan { title, cols, rows, rooms, devices } => render_floorplan_card(
                canvas,
                theme,
                rect,
                title.as_deref(),
                *cols,
                *rows,
                rooms,
                devices,
                states,
                self.unlock_armed.as_deref(),
                targets,
            ),
            CardConfig::Lock { entity_id, title } => {
                let armed = self.unlock_armed.as_deref() == Some(entity_id.as_str());
                render_lock_card(canvas, theme, rect, entity_id, states.get(entity_id), title.as_deref(), armed, targets)
            }
            CardConfig::Remote { entity_id, title } => {
                render_remote_card(canvas, theme, rect, entity_id, states.get(entity_id), title.as_deref(), targets)
            }
            CardConfig::Scene { entity_id, title, icon } => {
                let item = [SceneItemConfig {
                    entity_id: entity_id.clone(),
                    title: title.clone(),
                    icon: icon.clone().unwrap_or_else(|| "play".to_string()),
                }];
                render_scenes_row(canvas, theme, rect, None, &item, targets)
            }
        }
    }

    /// `x`, `y` in screen pixels.
    pub fn handle_touch(&self, x: f32, y: f32) -> Option<UiAction> {
        let (x, y) = (x / self.scale, y / self.scale);
        // Exact hit first (topmost wins), then the nearest target within a
        // small radius to forgive slightly-off finger taps.
        if let Some(t) = self.targets.iter().rev().find(|t| t.rect.contains(x, y)) {
            return Some(t.action.clone());
        }
        const SLOP: f32 = 24.0;
        self.targets
            .iter()
            .filter_map(|t| {
                let dx = (t.rect.x - x).max(x - (t.rect.x + t.rect.w)).max(0.0);
                let dy = (t.rect.y - y).max(y - (t.rect.y + t.rect.h)).max(0.0);
                let d = (dx * dx + dy * dy).sqrt();
                (d <= SLOP).then_some((d, t))
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, t)| t.action.clone())
    }
}

/// The one-line sleep note: "Sleep mode · Next update 22:56", smaller if
/// needed, and on narrow screens (Kobo Aura, Glo) just "Next update 22:56";
/// the moon icon already says it's sleeping. The time is never cut off.
fn sleep_line(canvas: &Canvas, time: &str, max_w: f32, size: f32) -> (String, f32) {
    use crate::i18n;
    let small = size * 0.875;
    let next = i18n::tf("sleep.next", &[("time", time)]);
    let full = format!("{} · {}", i18n::t("sleep.title"), next);
    for (text, size) in [(&full, size), (&full, small), (&next, size), (&next, small)] {
        if canvas.measure_text(text, size, true).0 <= max_w {
            return (text.clone(), size);
        }
    }
    // Still too wide: shorten the words but keep the time, "Next upd… 22:56"
    let Some(at) = next.find(time) else { return (next, small) };
    let (label, rest) = next.split_at(at);
    let mut chars: Vec<char> = label.trim_end().chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let text = format!("{}… {}", chars.iter().collect::<String>().trim_end(), rest);
        if canvas.measure_text(&text, small, true).0 <= max_w {
            return (text, small);
        }
    }
    (rest.to_string(), small)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_note_shows_the_next_update_time_on_narrow_screens() {
        // Glo/Aura width with page tabs: "Modalità sospensione · Prossimo
        // aggiorname…" lost the time (issue #2).
        let _guard = crate::i18n::TEST_LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let canvas = Canvas::new(758, 200);
        // Same geometry as render_sleep_note: the one-line bar in place of the
        // tabs, and the two-line bar at the bottom of a single page
        let bar_w = 758.0 - 2.0 * SIDE_PAD;
        for (bar_h, size) in [(64.0f32, 24.0), (SLEEP_NOTE_H - 28.0, 26.0)] {
            let icon = (bar_h * 0.5).min(40.0);
            let max_w = bar_w - bar_h * 0.4 - (bar_h * 0.4 + icon + 18.0);
            for (code, _) in crate::i18n::BUILTIN {
                crate::i18n::set_language(code);
                let (line, size) = sleep_line(&canvas, "22:56", max_w, size);
                assert!(line.contains("22:56"), "{}: {}", code, line);
                assert!(canvas.measure_text(&line, size, true).0 <= max_w, "{}: {}", code, line);
            }
        }
        crate::i18n::set_language("en");
        // Wide screens keep the full line
        assert_eq!(sleep_line(&canvas, "22:56", 1000.0, 24.0), ("Sleep mode · Next update 22:56".to_string(), 24.0));
        // Very narrow: the words are shortened, the time stays
        let (line, _) = sleep_line(&canvas, "22:56", 150.0, 24.0);
        assert!(line.ends_with("… 22:56"), "{}", line);
    }

    #[test]
    fn battery_empty_screen_replaces_the_dashboard() {
        let _guard = crate::i18n::TEST_LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let config = AppConfig::default();
        let mut canvas = Canvas::new(758, 1024);
        let mut renderer = ScreenRenderer::new(&config);
        renderer.battery_empty = true;
        renderer.render(&mut canvas, &config, 0, &HashMap::new(), false);
        assert!(renderer.targets.is_empty(), "nothing is tappable");
        // The narrowest screen fits both lines whole in every language
        for (code, _) in crate::i18n::BUILTIN {
            crate::i18n::set_language(code);
            let max_w = 758.0 - 2.0 * SIDE_PAD;
            for (key, size, bold) in [("sleep.battery_empty", 60.0, true), ("sleep.charge_hint", 32.0, false)] {
                let text = crate::i18n::t(key);
                assert!(canvas.measure_text(&text, size, bold).0 <= max_w, "{}: {}", code, text);
            }
        }
        crate::i18n::set_language("en");
    }

    #[test]
    fn smallest_screens_scale_the_layout_down() {
        // Kobo Touch (issue #7): 6" like the Glo, the same page has to fit
        let cards = AppConfig::default().pages[0].cards.clone();
        for ((w, h), (glo_w, glo_h)) in [((600, 800), (758, 1024)), ((800, 600), (1024, 758))] {
            assert_eq!(hidden_card_count(&cards, w, h, true), hidden_card_count(&cards, glo_w, glo_h, true));
            assert_eq!(ui_size(w, h).0.max(ui_size(w, h).1), 1024.0);
        }
        // Larger screens stay at one pixel per unit
        assert_eq!(ui_size(758, 1024), (758.0, 1024.0));
        assert_eq!(ui_size(1072, 1448), (1072.0, 1448.0));
    }

    /// A page holding the lock and the TV remote, as on a Kobo Aura.
    fn lock_and_remote_page() -> AppConfig {
        let mut config = AppConfig::default();
        config.theme.mode = "high_contrast_bw".to_string();
        config.pages = vec![crate::config::PageConfig {
            id: "home".to_string(),
            title: "Home".to_string(),
            icon: "home".to_string(),
            cards: vec![
                CardConfig::Lock { entity_id: "lock.front_door".to_string(), title: None },
                CardConfig::Switch { entity_id: "switch.heater".to_string(), title: Some("Heater".to_string()), subtitle: None, icon: None },
                CardConfig::Remote { entity_id: "remote.living_room_tv".to_string(), title: None },
            ],
        }];
        config
    }

    #[test]
    fn lock_and_remote_cards_fit_the_aura_both_ways_up() {
        let _guard = crate::i18n::TEST_LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::i18n::set_language("en");
        let config = lock_and_remote_page();
        let states = crate::ha::mock::generate_mock_states();
        for (w, h, landscape) in [(758, 1024, false), (1024, 758, true)] {
            let mut canvas = Canvas::for_screen(w, h);
            let mut renderer = ScreenRenderer::new(&config);
            renderer.render(&mut canvas, &config, 0, &states, landscape);
            assert_eq!(renderer.hidden_cards, 0, "{}x{}", w, h);
            let keys: Vec<&Rect> = renderer
                .targets
                .iter()
                .filter(|t| matches!(&t.action, UiAction::Service { domain, .. } if domain == "remote"))
                .map(|t| &t.rect)
                .collect();
            assert_eq!(keys.len(), 14, "five D-pad keys and nine others");
            // No key overlaps another, so a tap can only mean one of them
            for (i, a) in keys.iter().enumerate() {
                for b in &keys[i + 1..] {
                    let apart = a.x + a.w <= b.x || b.x + b.w <= a.x || a.y + a.h <= b.y || b.y + b.h <= a.y;
                    assert!(apart, "{:?} overlaps {:?} at {}x{}", a, b, w, h);
                }
            }
            let (uw, uh) = canvas.ui_size();
            for t in &renderer.targets {
                assert!(t.rect.x >= -0.5 && t.rect.y >= -0.5 && t.rect.x + t.rect.w <= uw + 0.5 && t.rect.y + t.rect.h <= uh + 0.5, "{:?}", t.rect);
            }
            assert!(renderer.targets.iter().any(|t| matches!(&t.action, UiAction::LockTap(id) if id == "lock.front_door")));
            if let Ok(dir) = std::env::var("KOBO_PREVIEW_DIR") {
                renderer.unlock_armed = Some("lock.front_door".to_string());
                renderer.render(&mut canvas, &config, 0, &states, landscape);
                std::fs::write(format!("{}/aura-{}x{}.png", dir, w, h), canvas.to_grayscale_png_bytes().unwrap()).unwrap();
            }
        }
    }

    /// Two rooms side by side: a lit kitchen with a second switch, and a hall
    /// holding only the front door lock.
    fn plan_page() -> AppConfig {
        use crate::config::{PlanDevice, PlanRoom};
        let dev = |id: &str, x: f32, y: f32| PlanDevice { entity_id: id.to_string(), x, y, icon: None };
        let mut config = AppConfig::default();
        config.theme.mode = "high_contrast_bw".to_string();
        config.pages = vec![crate::config::PageConfig {
            id: "plan".to_string(),
            title: "Plan".to_string(),
            icon: "home".to_string(),
            cards: vec![CardConfig::Floorplan {
                title: None,
                cols: 24,
                rows: 16,
                rooms: vec![
                    PlanRoom { name: "Kitchen".to_string(), x: 0.0, y: 0.0, w: 14.0, h: 10.0 },
                    PlanRoom { name: "Hall".to_string(), x: 14.0, y: 0.0, w: 10.0, h: 10.0 },
                    PlanRoom { name: "Living room".to_string(), x: 0.0, y: 10.0, w: 24.0, h: 6.0 },
                ],
                devices: vec![
                    dev("light.ceiling", 4.0, 5.0),
                    dev("switch.heater", 10.0, 5.0),
                    dev("lock.front_door", 19.0, 5.0),
                    dev("sensor.living_room_temperature", 20.0, 13.0),
                    dev("remote.living_room_tv", 5.0, 13.0),
                ],
            }],
        }];
        config
    }

    fn room_tap<'a>(renderer: &'a ScreenRenderer, rooms: usize) -> Vec<(&'a str, Vec<String>)> {
        let found: Vec<_> = renderer
            .targets
            .iter()
            .filter_map(|t| match &t.action {
                UiAction::Service { domain, service, data } if domain == "homeassistant" => {
                    let ids = data["entity_id"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
                    Some((service.as_str(), ids))
                }
                _ => None,
            })
            .collect();
        assert_eq!(found.len(), rooms);
        found
    }

    #[test]
    fn a_room_tap_switches_its_lights_and_never_its_lock() {
        let _guard = crate::i18n::TEST_LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::i18n::set_language("en");
        let config = plan_page();
        let mut states = crate::ha::mock::generate_mock_states();
        states.get_mut("light.ceiling").unwrap().state = "on".to_string();
        states.insert("switch.heater".to_string(), EntityState { entity_id: "switch.heater".to_string(), state: "off".to_string(), attributes: Default::default(), last_changed: None, last_updated: None });
        let mut canvas = Canvas::for_screen(758, 1024);
        let mut renderer = ScreenRenderer::new(&config);
        renderer.render(&mut canvas, &config, 0, &states, false);
        assert_eq!(renderer.hidden_cards, 0);

        // Only the kitchen can be tapped as a room: the hall holds nothing a
        // room tap may switch, and the living room only a reading and a remote.
        let rooms = room_tap(&renderer, 1);
        assert_eq!(rooms[0], ("turn_off", vec!["light.ceiling".to_string(), "switch.heater".to_string()]), "one light is on, so the room goes off");

        // A tap on a device reaches the device, not the room under it
        let hit = |id: &str| {
            let t = renderer.targets.iter().rev().find(|t| match &t.action {
                UiAction::ToggleEntity(e) | UiAction::LockTap(e) => e == id,
                _ => false,
            });
            let r = t.unwrap_or_else(|| panic!("{} has no target", id)).rect;
            renderer.handle_touch((r.x + r.w / 2.0) * canvas.scale, (r.y + r.h / 2.0) * canvas.scale)
        };
        assert!(matches!(hit("light.ceiling"), Some(UiAction::ToggleEntity(id)) if id == "light.ceiling"));
        assert!(matches!(hit("lock.front_door"), Some(UiAction::LockTap(id)) if id == "lock.front_door"));
        assert!(matches!(hit("remote.living_room_tv"), Some(UiAction::ToggleEntity(_))));

        // With everything off, the same room tap turns the room on
        states.get_mut("light.ceiling").unwrap().state = "off".to_string();
        renderer.render(&mut canvas, &config, 0, &states, false);
        assert_eq!(room_tap(&renderer, 1)[0].0, "turn_on");

        if let Ok(dir) = std::env::var("KOBO_PREVIEW_DIR") {
            states.get_mut("light.ceiling").unwrap().state = "on".to_string();
            for (w, h, landscape) in [(758, 1024, false), (1024, 758, true)] {
                let mut canvas = Canvas::for_screen(w, h);
                renderer.render(&mut canvas, &config, 0, &states, landscape);
                std::fs::write(format!("{}/plan-{}x{}.png", dir, w, h), canvas.to_grayscale_png_bytes().unwrap()).unwrap();
            }
        }
    }

    #[test]
    fn a_floorplan_round_trips_through_yaml() {
        let config = plan_page();
        let yaml = serde_yaml::to_string(&config.pages[0].cards).unwrap();
        let back: Vec<CardConfig> = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(serde_yaml::to_string(&back).unwrap(), yaml);
        // A bare card gets a usable grid
        let bare: Vec<CardConfig> = serde_yaml::from_str("- type: floorplan\n").unwrap();
        assert!(matches!(&bare[0], CardConfig::Floorplan { cols: 24, rows: 16, rooms, devices, .. } if rooms.is_empty() && devices.is_empty()));
    }

    #[test]
    fn tapping_a_forecast_day_opens_its_hours_and_back_returns() {
        let _guard = crate::i18n::TEST_LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::i18n::set_language("en");
        let mut config = AppConfig::default();
        config.theme.mode = "high_contrast_bw".to_string();
        config.theme.utc_offset = Some("+00:00".to_string());
        let states = crate::ha::mock::generate_mock_states();
        let weather = config.pages[0].cards.iter().find_map(|c| match c {
            CardConfig::Weather { entity_id, .. } => Some(entity_id.clone()),
            _ => None,
        });
        let weather = weather.expect("the demo page has a weather card");
        for (w, h, landscape) in [(758, 1024, false), (1024, 758, true)] {
            let mut canvas = Canvas::for_screen(w, h);
            let mut renderer = ScreenRenderer::new(&config);
            renderer.render(&mut canvas, &config, 0, &states, landscape);
            // One target per forecast day, each naming a different day
            let days: Vec<(Rect, String)> = renderer
                .targets
                .iter()
                .filter_map(|t| match &t.action {
                    UiAction::OpenWeatherDay(e, day) if *e == weather => Some((t.rect, day.clone())),
                    _ => None,
                })
                .collect();
            assert!(days.len() >= 3, "{}x{}: {}", w, h, days.len());
            let mut distinct: Vec<&String> = days.iter().map(|(_, d)| d).collect();
            distinct.dedup();
            assert_eq!(distinct.len(), days.len());
            // A tap on tomorrow's column means tomorrow, not the card's refresh
            let (r, tomorrow) = &days[1];
            let tapped = renderer.handle_touch((r.x + r.w / 2.0) * canvas.scale, (r.y + r.h / 2.0) * canvas.scale);
            assert!(matches!(&tapped, Some(UiAction::OpenWeatherDay(_, d)) if d == tomorrow));

            renderer.weather_detail = Some((weather.clone(), tomorrow.clone()));
            renderer.render(&mut canvas, &config, 0, &states, landscape);
            assert!(renderer.targets.iter().any(|t| matches!(t.action, UiAction::CloseDetail)), "a way back");
            assert!(!renderer.targets.iter().any(|t| matches!(t.action, UiAction::ToggleEntity(_) | UiAction::OpenWeatherDay(..))), "the cards are not under the page");
            if let Ok(dir) = std::env::var("KOBO_PREVIEW_DIR") {
                std::fs::write(format!("{}/weather-{}x{}.png", dir, w, h), canvas.to_grayscale_png_bytes().unwrap()).unwrap();
            }
        }
    }

    #[test]
    fn a_day_page_lists_that_local_days_hours_only() {
        use crate::ui::widgets::weather_detail::{hours_of_day, local_date, HOURLY_ATTR};
        use chrono::Timelike;
        // 03:00 UTC on the 17th is still the 16th in New York (UTC-5)
        let ny = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
        let utc = chrono::FixedOffset::east_opt(0).unwrap();
        let hours: Vec<serde_json::Value> = (0..30)
            .map(|h| serde_json::json!({ "datetime": format!("2026-11-{:02}T{:02}:00:00+00:00", 16 + h / 24, h % 24), "condition": "sunny", "temperature": 10.0 }))
            .collect();
        let mut state = crate::ha::mock::generate_mock_states().remove("weather.home").unwrap();
        state.attributes.insert(HOURLY_ATTR.to_string(), serde_json::json!(hours));
        let day = |d| chrono::NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        assert_eq!(local_date("2026-11-17T03:00:00+00:00", ny), Some(day(16)));
        assert_eq!(local_date("2026-11-17T03:00:00+00:00", utc), Some(day(17)));
        assert_eq!(hours_of_day(Some(&state), day(16), utc).len(), 24);
        assert_eq!(hours_of_day(Some(&state), day(17), utc).len(), 6);
        // In New York the same data starts at 19:00 on the 15th
        let ny_16 = hours_of_day(Some(&state), day(16), ny);
        assert_eq!((ny_16.len(), ny_16[0].0.hour()), (24, 0));
        assert_eq!(hours_of_day(Some(&state), day(15), ny).len(), 5);
        assert!(hours_of_day(Some(&state), day(20), utc).is_empty(), "beyond the hourly forecast");
        assert!(hours_of_day(None, day(16), utc).is_empty());
    }

    #[test]
    fn the_sleep_note_can_carry_the_owners_message() {
        let _guard = crate::i18n::TEST_LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        crate::i18n::set_language("en");
        let states = crate::ha::mock::generate_mock_states();
        let frame = |message: Option<&str>, hide_next: bool, pages: usize| {
            let mut config = AppConfig::default();
            config.theme.mode = "high_contrast_bw".to_string();
            config.pages.truncate(pages);
            config.device.sleep_message = message.map(str::to_string);
            config.device.sleep_hide_next = hide_next;
            let mut canvas = Canvas::for_screen(758, 1024);
            let mut renderer = ScreenRenderer::new(&config);
            renderer.sleep_until = Some("22:56".to_string());
            renderer.render(&mut canvas, &config, 0, &states, true);
            assert!(renderer.targets.is_empty(), "nothing is tappable while asleep");
            (canvas.to_grayscale_png_bytes().unwrap(), renderer.sleep_message.clone())
        };
        // With tabs the note is one line in their place; with a single page
        // it is a taller bar at the bottom. Both draw each variant differently.
        for pages in [2, 1] {
            let (standard, none) = frame(None, false, pages);
            let (custom, kept) = frame(Some("  Tap power to wake  "), false, pages);
            let (custom_no_time, _) = frame(Some("Tap power to wake"), true, pages);
            let (blank, blank_kept) = frame(Some("   "), false, pages);
            assert_eq!((none, kept.as_deref(), blank_kept), (None, Some("Tap power to wake"), None));
            assert!(standard != custom && custom != custom_no_time, "pages: {}", pages);
            assert!(standard == blank, "an empty message is no message");
            if let Ok(dir) = std::env::var("KOBO_PREVIEW_DIR") {
                std::fs::write(format!("{}/sleep-{}.png", dir, pages), &custom).unwrap();
            }
        }
    }

    #[test]
    fn an_offline_lock_cannot_be_tapped() {
        let _guard = crate::i18n::TEST_LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let config = lock_and_remote_page();
        let mut canvas = Canvas::for_screen(758, 1024);
        let mut renderer = ScreenRenderer::new(&config);
        // No states at all: the lock and the remote are unavailable
        renderer.render(&mut canvas, &config, 0, &HashMap::new(), false);
        assert!(!renderer.targets.iter().any(|t| matches!(t.action, UiAction::LockTap(_) | UiAction::Service { .. })));
    }

    #[test]
    fn lock_and_remote_cards_round_trip_through_yaml() {
        let yaml = "- type: lock\n  entity_id: lock.front_door\n- type: remote\n  entity_id: remote.tv\n  title: TV\n";
        let cards: Vec<CardConfig> = serde_yaml::from_str(yaml).unwrap();
        assert!(matches!(&cards[0], CardConfig::Lock { entity_id, title: None } if entity_id == "lock.front_door"));
        assert!(matches!(&cards[1], CardConfig::Remote { title: Some(t), .. } if t == "TV"));
        assert_eq!(serde_yaml::to_string(&cards).unwrap(), yaml);
    }

    #[test]
    fn taps_land_on_scaled_targets() {
        let _guard = crate::i18n::TEST_LANGUAGE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let config = AppConfig::default();
        let mut canvas = Canvas::for_screen(600, 800);
        let mut renderer = ScreenRenderer::new(&config);
        renderer.render(&mut canvas, &config, 0, &HashMap::new(), false);
        let (w, h) = canvas.ui_size();
        // Everything tappable is on screen, and a tap in the middle of a
        // target's pixels hits that target rather than a neighbour.
        let s = canvas.scale;
        for t in &renderer.targets {
            assert!(t.rect.x + t.rect.w <= w + 0.5 && t.rect.y + t.rect.h <= h + 0.5, "{:?}", t.rect);
            let hit = renderer.handle_touch((t.rect.x + t.rect.w / 2.0) * s, (t.rect.y + t.rect.h / 2.0) * s);
            assert_eq!(format!("{:?}", hit), format!("{:?}", Some(t.action.clone())));
        }
    }

    #[test]
    fn small_landscape_uses_one_column() {
        // Nia/Glo/Aura in landscape: two columns left ~240 px cards (issue #4)
        let cards = AppConfig::default().pages[0].cards.clone();
        for (_, r) in zones_for(&cards, 1024.0, 758.0, HEADER_WITH_TABS) {
            assert!(r.w >= LANDSCAPE_MIN_HALF_W, "{:?}", r);
        }
        // Larger screens keep the weather column
        let placed = zones_for(&cards, 1448.0, 1072.0, HEADER_WITH_TABS);
        assert!(placed.iter().any(|(_, r)| r.w == LANDSCAPE_LEFT_W));
    }

    fn overlaps(a: &Rect, b: &Rect) -> bool {
        a.x < b.x + b.w - 0.5 && b.x < a.x + a.w - 0.5 && a.y < b.y + b.h - 0.5 && b.y < a.y + a.h - 0.5
    }

    #[test]
    fn demo_pages_fit_without_overlap() {
        let config = AppConfig::default();
        for landscape in [false, true] {
            let (w, h) = if landscape { (1448, 1072) } else { (1072, 1448) };
            let mut canvas = Canvas::new(w, h);
            let mut renderer = ScreenRenderer::new(&config);
            let states = crate::ha::mock::generate_mock_states();
            for page in 0..config.pages.len() {
                renderer.render(&mut canvas, &config, page, &states, false);
                let rects: Vec<Rect> = renderer.targets.iter().map(|t| t.rect).collect();
                for r in &rects {
                    assert!(r.y + r.h <= h as f32 + 12.0, "target below screen: {:?}", r);
                    assert!(r.x + r.w <= w as f32 + 12.0, "target right of screen: {:?}", r);
                }
            }
            if landscape {
                continue; // landscape splits into two zones; covered by the render pass above
            }
            let all: Vec<&CardConfig> = config.pages[0].cards.iter().collect();
            let placed = layout_zone(&all, 32.0, 200.0, w as f32 - 64.0, h as f32 - 230.0);
            assert_eq!(placed.len(), all.len(), "a card was dropped (landscape={})", landscape);
            for (i, (_, a)) in placed.iter().enumerate() {
                for (_, b) in placed.iter().skip(i + 1) {
                    assert!(!overlaps(a, b), "{:?} overlaps {:?}", a, b);
                }
            }
        }
    }
}
