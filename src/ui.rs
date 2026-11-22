//! Drawing. Only what fits on screen is built each frame.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, List, ListItem, Paragraph, Row, Table, TableState},
};

use crate::app::{App, Focus, fmt_date, fmt_s};

const ACCENT: Color = Color::Rgb(255, 176, 59);
const ON_ACCENT: Color = Color::Black;

fn block(title: &str, focused: bool) -> Block<'_> {
    let b = Block::bordered().border_type(BorderType::Rounded).title(format!(" {title} "));
    if focused { b.border_style(Style::new().fg(ACCENT)) } else { b.border_style(Style::new().dim()) }
}

fn highlight(focused: bool) -> Style {
    if focused { Style::new().fg(ON_ACCENT).bg(ACCENT) } else { Style::new().add_modifier(Modifier::REVERSED) }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [body, now, bottom] = Layout::vertical([Constraint::Min(5), Constraint::Length(2), Constraint::Length(1)]).areas(f.area());
    let side_w = (f.area().width / 4).clamp(18, 36);
    let [side, main] = Layout::horizontal([Constraint::Length(side_w), Constraint::Min(20)]).areas(body);

    let items: Vec<ListItem> = app
        .shows
        .iter()
        .map(|s| {
            let mark = if s.loading { Span::raw(" …").dim() } else if s.error.is_some() { Span::raw(" !").fg(Color::Red) } else { Span::raw("") };
            ListItem::new(Line::from(vec![Span::raw(s.show.title.as_str()), mark]))
        })
        .collect();
    let shows_focused = app.focus == Focus::Shows;
    let list = List::new(items).block(block("Shows", shows_focused)).highlight_style(highlight(shows_focused));
    f.render_stateful_widget(list, side, &mut app.show_state);

    draw_episodes(f, app, main);
    draw_now(f, app, now);

    if app.focus == Focus::Input {
        let text = format!(" Add: {}", app.input);
        let x = bottom.x + text.chars().count() as u16;
        f.render_widget(Paragraph::new(text).fg(ACCENT), bottom);
        f.set_cursor_position((x.min(bottom.right().saturating_sub(1)), bottom.y));
    } else if !app.status.is_empty() {
        f.render_widget(Paragraph::new(format!(" {}", app.status)).dim(), bottom);
    } else {
        let hint = " Enter play · Space pause · ←/→ −15/+30 s · [ ] speed · +/− volume · m played · a add · R refresh · D remove · q quit";
        f.render_widget(Paragraph::new(hint).dim(), bottom);
    }
}

fn draw_episodes(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Episodes;
    let Some(show) = app.show_state.selected().and_then(|i| app.shows.get(i)) else {
        let msg = "\n No shows yet.\n\n Press a, then paste a podcast's feed URL,\n or the path to an OPML file exported from another podcast app.";
        f.render_widget(Paragraph::new(msg).block(block("Episodes", focused)), area);
        return;
    };
    let eps = &show.show.episodes;
    let mut title = format!("{} · {} episodes", show.show.title, eps.len());
    if show.loading {
        title.push_str(" (refreshing…)");
    }
    if let Some(e) = &show.error {
        title.push_str(&format!(" (couldn't refresh: {e})"));
    }
    if eps.is_empty() {
        let msg = if show.loading { "\n Loading…" } else { "\n No episodes." };
        f.render_widget(Paragraph::new(msg).block(block(&title, focused)), area);
        return;
    }

    // only the rows that fit: some shows have thousands of episodes
    let visible = area.height.saturating_sub(3).max(1) as usize;
    let sel = app.ep_state.selected().unwrap_or(0).min(eps.len() - 1);
    let mut off = app.ep_state.offset().min(eps.len() - 1);
    if sel < off {
        off = sel;
    } else if sel >= off + visible {
        off = sel + 1 - visible;
    }
    *app.ep_state.offset_mut() = off;
    let end = (off + visible).min(eps.len());
    let playing = app.now.as_ref().map(|n| n.guid.as_str());

    let rows = eps[off..end].iter().map(|e| {
        let l = app.state.get(&e.guid).copied().unwrap_or_default();
        let (mark, len) = if Some(e.guid.as_str()) == playing {
            ("▶", fmt_s(e.duration_s as f64))
        } else if l.played {
            ("✓", fmt_s(e.duration_s as f64))
        } else if l.pos > 0.0 {
            ("◐", if e.duration_s > 0 { format!("{} left", fmt_s(e.duration_s as f64 - l.pos)) } else { fmt_s(l.pos) })
        } else {
            (" ", if e.duration_s > 0 { fmt_s(e.duration_s as f64) } else { String::new() })
        };
        let mut row = Row::new(vec![Cell::from(mark).fg(ACCENT), Cell::from(e.title.as_str()), Cell::from(fmt_date(e.published)).dim(), Cell::from(Line::from(len).right_aligned())]);
        if l.played && Some(e.guid.as_str()) != playing {
            row = row.style(Style::new().dim());
        } else if Some(e.guid.as_str()) == playing {
            row = row.style(Style::new().fg(ACCENT).bold());
        }
        row
    });
    let widths = [Constraint::Length(1), Constraint::Fill(1), Constraint::Length(10), Constraint::Length(13)];
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["", "Episode", "Date", "Length"]).dim())
        .block(block(&title, focused))
        .row_highlight_style(highlight(focused))
        .column_spacing(1);
    let mut window = TableState::default().with_selected(Some(sel - off));
    f.render_stateful_widget(table, area, &mut window);
}

fn draw_now(f: &mut Frame, app: &App, area: Rect) {
    let [line1, line2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let mut right = Vec::new();
    if let Some(n) = &app.now
        && n.buffering
    {
        right.push(Span::raw("buffering…  ").fg(ACCENT));
    }
    right.push(Span::raw(format!("{:.1}×  vol {}% ", app.prefs.speed, app.prefs.volume)).dim());
    let right = Line::from(right);
    let Some(n) = &app.now else {
        f.render_widget(Paragraph::new(" ■ Nothing playing").dim(), line1);
        f.render_widget(Paragraph::new(right).right_aligned(), line1);
        return;
    };
    let [l, r] = Layout::horizontal([Constraint::Min(10), Constraint::Length(right.width() as u16)]).areas(line1);
    let icon = if n.playing { "▶" } else { "⏸" };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(format!(" {icon} ")).fg(ACCENT).bold(),
            Span::raw(n.title.as_str()).fg(ACCENT).bold(),
            Span::raw("  "),
            Span::raw(n.show.as_str()).dim(),
        ])),
        l,
    );
    f.render_widget(Paragraph::new(right).right_aligned(), r);

    // " 12:03 ██████▌░░░░░ 41:10 ": 1/8-cell precision
    let ratio = if n.duration > 0.0 { (n.pos / n.duration).clamp(0.0, 1.0) } else { 0.0 };
    let (ta, tb) = (format!(" {} ", fmt_s(n.pos)), format!(" {} ", if n.duration > 0.0 { fmt_s(n.duration) } else { "--:--".into() }));
    let bar = (line2.width as usize).saturating_sub(ta.len() + tb.len());
    let eighths = (ratio * (bar * 8) as f64) as usize;
    let track = Color::Rgb(55, 55, 60);
    let fill = if n.playing { ACCENT } else { Color::Rgb(120, 120, 120) };
    let mut spans = vec![Span::raw(ta).dim()];
    spans.extend((0..bar).map(|i| match (eighths / 8).cmp(&i) {
        std::cmp::Ordering::Greater => Span::styled("█", Style::new().fg(fill)),
        std::cmp::Ordering::Equal if !eighths.is_multiple_of(8) => Span::styled(["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][eighths % 8], Style::new().fg(fill).bg(track)),
        _ => Span::styled(" ", Style::new().bg(track)),
    }));
    spans.push(Span::raw(tb).dim());
    f.render_widget(Paragraph::new(Line::from(spans)), line2);
}
