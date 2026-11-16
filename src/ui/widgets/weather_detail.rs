use crate::display::{Canvas, TextAlign};
use crate::ha::models::{EntityState, ForecastItem};
use crate::i18n;
use crate::ui::layout::{InteractiveTarget, Rect, UiAction};
use crate::ui::theme::Theme;
use crate::ui::widgets::{draw_card, draw_round_button, type_scale, PAD};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, Timelike};

/// Attribute holding the hour-by-hour forecast fetched for a weather entity
pub const HOURLY_ATTR: &str = "forecast_hourly";
const TOP_H: f32 = 96.0;
const MAX_COLUMNS: usize = 3;

/// A forecast time in the dashboard's own time zone. Home Assistant sends
/// them with an offset (usually UTC), and the Kobo's clock is UTC too, so the
/// date printed in the string is not necessarily the local day.
fn local_time(datetime: &str, offset: FixedOffset) -> Option<DateTime<FixedOffset>> {
    DateTime::parse_from_rfc3339(datetime).ok().map(|t| t.with_timezone(&offset))
}

pub fn local_date(datetime: &str, offset: FixedOffset) -> Option<NaiveDate> {
    local_time(datetime, offset)
        .map(|t| t.date_naive())
        .or_else(|| datetime.get(..10).and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok()))
}

fn hour_label(t: &DateTime<FixedOffset>, twelve_hour: bool) -> String {
    if twelve_hour {
        let (pm, h) = t.hour12();
        format!("{} {}", h, if pm { "PM" } else { "AM" })
    } else {
        format!("{:02}:00", t.hour())
    }
}

/// The hours of one day out of an hourly forecast, in order.
pub fn hours_of_day(state: Option<&EntityState>, day: NaiveDate, offset: FixedOffset) -> Vec<(DateTime<FixedOffset>, ForecastItem)> {
    let items: Vec<ForecastItem> =
        state.and_then(|s| s.attributes.get(HOURLY_ATTR)).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    let mut hours: Vec<_> = items
        .into_iter()
        .filter_map(|item| local_time(&item.datetime, offset).map(|t| (t, item)))
        .filter(|(t, _)| t.date_naive() == day)
        .collect();
    hours.sort_by_key(|(t, _)| *t);
    hours
}

/// One day of weather, hour by hour, filling the page under the header.
/// Opened by tapping a day in the weather card; the back button returns.
#[allow(clippy::too_many_arguments)]
pub fn render_weather_detail(
    canvas: &mut Canvas,
    theme: &Theme,
    area: Rect,
    state: Option<&EntityState>,
    day_datetime: &str,
    offset: FixedOffset,
    twelve_hour: bool,
    targets: &mut Vec<InteractiveTarget>,
) {
    // Back button and the day's name
    let back = Rect::new(area.x, area.y + 4.0, 72.0, 72.0);
    draw_round_button(canvas, theme, back, "back", theme.accent, false);
    targets.push(InteractiveTarget { rect: back.inset(-8.0), action: UiAction::CloseDetail });

    let day = local_date(day_datetime, offset);
    let tx = back.x + back.w + 20.0;
    if let Some(date) = day {
        let weekday = i18n::nth("date.weekdays", date.weekday().num_days_from_monday() as usize);
        let long = i18n::tf("date.long", &[("day", &date.day().to_string()), ("month", &i18n::nth("date.months", date.month0() as usize))]);
        canvas.draw_text_fit(&weekday, tx, area.y + 2.0, 38.0, true, theme.text, TextAlign::Left, area.w * 0.5);
        canvas.draw_text_fit(&long, tx, area.y + 48.0, type_scale::BODY, false, theme.text_muted, TextAlign::Left, area.w * 0.5);
    }

    // The day's summary from the daily forecast, at the right
    let daily: Vec<ForecastItem> =
        state.and_then(|s| s.attributes.get("forecast")).and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
    if let Some(item) = daily.iter().find(|i| day.is_some() && local_date(&i.datetime, offset) == day) {
        let right = area.x + area.w;
        let hi = format!("{:.0}°", item.temperature);
        let lo = item.templow.map(|l| format!("{:.0}°", l)).unwrap_or_default();
        let lo_w = if lo.is_empty() { 0.0 } else { canvas.measure_text(&lo, 28.0, false).0 + 12.0 };
        let hi_w = canvas.measure_text(&hi, 44.0, true).0;
        canvas.draw_text(&lo, right, area.y + 22.0, 28.0, false, theme.text_muted, TextAlign::Right);
        canvas.draw_text(&hi, right - lo_w, area.y + 8.0, 44.0, true, theme.text, TextAlign::Right);
        canvas.draw_weather_art(&item.condition, right - lo_w - hi_w - 84.0, area.y + 4.0, 68.0, theme.weather);
    }

    let list = Rect::new(area.x, area.y + TOP_H, area.w, (area.h - TOP_H).max(60.0));
    draw_card(canvas, theme, list, None);
    let hours = day.map(|d| hours_of_day(state, d, offset)).unwrap_or_default();
    if hours.is_empty() {
        canvas.draw_text_fit(
            &i18n::t("weather.no_hourly"),
            list.x + list.w / 2.0,
            list.y + list.h / 2.0 - 14.0,
            type_scale::BODY,
            false,
            theme.text_muted,
            TextAlign::Center,
            list.w - 2.0 * PAD,
        );
        return;
    }

    // As many columns as the hours need at a readable row height; a very
    // short screen shows every second or third hour rather than tiny rows.
    let inner_h = list.h - 2.0 * 12.0;
    let max_rows = ((inner_h / 42.0).floor() as usize).max(1);
    let every = hours.len().div_ceil(max_rows * MAX_COLUMNS).max(1);
    let shown: Vec<&(DateTime<FixedOffset>, ForecastItem)> = hours.iter().step_by(every).collect();
    let cols = shown.len().div_ceil(max_rows).clamp(1, MAX_COLUMNS);
    let rows = shown.len().div_ceil(cols);
    let row_h = (inner_h / rows as f32).min(58.0);
    let col_w = list.w / cols as f32;
    let roomy = col_w >= 400.0;

    for (i, (time, item)) in shown.iter().enumerate() {
        let (col, row) = (i / rows, i % rows);
        let x = list.x + col as f32 * col_w + PAD;
        let y = list.y + 12.0 + row as f32 * row_h;
        let mid = y + row_h / 2.0;
        if row > 0 {
            canvas.draw_line(x, y, x + col_w - 2.0 * PAD, y, 1.0, theme.border);
        }
        if col > 0 && row == 0 {
            canvas.draw_line(x - PAD, list.y + 14.0, x - PAD, list.y + list.h - 14.0, 2.0, theme.border);
        }
        let art = (row_h - 10.0).min(38.0);
        canvas.draw_text(&hour_label(time, twelve_hour), x, mid - 14.0, 24.0, true, theme.text, TextAlign::Left);
        canvas.draw_weather_art(&item.condition, x + 88.0, mid - art / 2.0, art, theme.weather);
        canvas.draw_text(&format!("{:.0}°", item.temperature), x + 190.0, mid - 15.0, 26.0, true, theme.text, TextAlign::Right);
        if let Some(p) = item.precipitation_probability {
            canvas.draw_icon("droplet", x + 206.0, mid - 11.0, 22.0, theme.text_muted, 2.0);
            let text = i18n::tf("meta.percent", &[("value", &format!("{:.0}", p))]);
            canvas.draw_text(&text, x + 232.0, mid - 12.0, type_scale::LABEL, false, theme.text, TextAlign::Left);
        }
        if let (true, Some(w)) = (roomy, item.wind_speed) {
            canvas.draw_icon("wind", x + 310.0, mid - 11.0, 22.0, theme.text_muted, 2.0);
            canvas.draw_text(&format!("{:.0}", w), x + 338.0, mid - 12.0, type_scale::LABEL, false, theme.text, TextAlign::Left);
        }
    }
}
