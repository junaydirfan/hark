//! Drawing, in trill's look. Only what fits on screen is built each frame.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Clear, List, ListItem, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    app::{App, FIXED, Focus, Item, Kind, fmt_date, fmt_s},
    theme::{accent, grad, on_accent},
};

/// figlet "Slant", like trill's logo; shown above the sidebar when there's room.
const LOGO: [&str; 5] = [
    "    __               __  ",
    "   / /_  ____ ______/ /__",
    "  / __ \\/ __ `/ ___/ //_/",
    " / / / / /_/ / /  / ,<   ",
    "/_/ /_/\\__,_/_/  /_/|_|  ",
];

/// The logo with the theme's gradient across it, if it fits `width`.
fn logo(width: u16) -> Option<Vec<Line<'static>>> {
    if LOGO[0].chars().count() as u16 > width {
        return None;
    }
    Some(
        LOGO.iter()
            .map(|row| {
                let n = row.chars().count().max(2) as f32 - 1.0;
                Line::from(row.chars().enumerate().map(|(i, c)| Span::styled(c.to_string(), Style::new().fg(grad(i as f32 / n)).bold())).collect::<Vec<_>>()).centered()
            })
            .collect(),
    )
}

fn block(title: &str, focused: bool) -> Block<'_> {
    let b = Block::bordered().border_type(BorderType::Rounded).title(format!(" {title} "));
    if focused { b.border_style(Style::new().fg(accent())) } else { b.border_style(Style::new().dim()) }
}

fn highlight(focused: bool) -> Style {
    if focused { Style::new().fg(on_accent()).bg(accent()) } else { Style::new().add_modifier(Modifier::REVERSED) }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let [body, now, bottom] = Layout::vertical([Constraint::Min(5), Constraint::Length(2), Constraint::Length(1)]).areas(f.area());
    // Narrow terminals: the sidebar only takes room while it has focus (Tab shows it).
    let side_w = if f.area().width < 70 && app.focus != Focus::Side { 0 } else { (f.area().width / 4).clamp(20, 34) };
    let [side, main] = Layout::horizontal([Constraint::Length(side_w), Constraint::Min(20)]).areas(body);

    // the playing episode's cover under the library, when there's room (like trill)
    let rows = art_rows(app, side);
    app.art_want = rows.map(|r| (side.width, r));
    let (side, art_area) = match rows {
        Some(r) => {
            let [s, a] = Layout::vertical([Constraint::Min(6), Constraint::Length(r)]).areas(side);
            (s, Some(a))
        }
        None => (side, None),
    };
    let side = match logo(side.width).filter(|l| side.height >= l.len() as u16 + 12) {
        Some(l) => {
            let [a, s] = Layout::vertical([Constraint::Length(l.len() as u16), Constraint::Min(6)]).areas(side);
            f.render_widget(Paragraph::new(l), a);
            s
        }
        None => side,
    };
    let fixed = FIXED.iter().enumerate().map(|(i, name)| {
        let n = app.counts.get(i).copied().unwrap_or(0);
        let count = if n > 0 { Span::raw(format!("  {n}")).dim() } else { Span::raw("") };
        ListItem::new(Line::from(vec![Span::raw(*name).bold(), count]))
    });
    let shows = app.shows.iter().map(|s| {
        let mark = if s.loading { Span::raw(" …").dim() } else if s.error.is_some() { Span::raw(" !").fg(Color::Red) } else { Span::raw("") };
        ListItem::new(Line::from(vec![Span::raw(s.show.title.as_str()), mark]))
    });
    let mut items: Vec<ListItem> = fixed.collect();
    if !app.shows.is_empty() {
        items.push(ListItem::new(Line::from("─ Shows ─").dim()));
    }
    let n_fixed = items.len();
    items.extend(shows);
    // the "Shows" divider is a row of its own: map the selection around it
    let mut state = app.side_state;
    if let Some(sel) = state.selected().filter(|&s| s >= FIXED.len() && n_fixed > FIXED.len()) {
        state.select(Some(sel + 1));
    }
    let focused = app.focus == Focus::Side;
    let list = List::new(items).block(block("Library", focused)).highlight_style(highlight(focused));
    f.render_stateful_widget(list, side, &mut state);
    *app.side_state.offset_mut() = state.offset();
    app.side_area = side;

    app.main_area = main;
    if app.notes.is_some() {
        draw_notes(f, app, main);
    } else {
        draw_table(f, app, main);
    }
    draw_now(f, app, now);

    if app.focus == Focus::Input {
        let text = format!(" Search podcasts, or paste a feed URL or an OPML file's path: {}", app.input);
        let x = bottom.x + text.chars().count() as u16;
        f.render_widget(Paragraph::new(text).fg(accent()), bottom);
        f.set_cursor_position((x.min(bottom.right().saturating_sub(1)), bottom.y));
    } else if !app.status.is_empty() {
        f.render_widget(Paragraph::new(format!(" {}", app.status)).dim(), bottom);
    } else if let Some(tag) = &app.update {
        f.render_widget(Paragraph::new(format!(" hark {tag} is out: quit and run hark --update · ? keys")).fg(accent()), bottom);
    } else {
        f.render_widget(Paragraph::new(" ? keys · Enter play · Space pause · a queue · L notes · / search · f follow · q quit").dim(), bottom);
    }
    if app.help_open {
        draw_help(f);
    } else if let (Some(area), Some(proto)) = (art_area, app.art.as_ref().and_then(|a| a.proto.as_ref())) {
        // terminal graphics drop every image row anything else is drawn across, so a popup hides the cover
        let sz = proto.size();
        let x = area.x + area.width.saturating_sub(sz.width) / 2;
        f.render_widget(ratatui_image::Image::new(proto), Rect { x, y: area.y, width: sz.width.min(area.width), height: sz.height.min(area.height) });
    }
}

/// Rows for a square cover as wide as the sidebar, if images are on and there's room for it.
fn art_rows(app: &App, side: Rect) -> Option<u16> {
    let picker = app.img_picker.as_ref()?;
    if !app.prefs.art || app.now.as_ref()?.art.is_none() {
        return None;
    }
    let fs = picker.font_size();
    let rows = (side.width as u32 * fs.width as u32 / fs.height.max(1) as u32) as u16;
    (rows >= 4 && side.height >= rows + 10).then_some(rows)
}

fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Main;
    let title = app.view.title.clone();
    if app.shows.is_empty() && matches!(app.view.kind, Kind::Inbox | Kind::InProgress | Kind::Queue) {
        let msg = "\n No shows yet.\n\n Press / and search for a podcast, or browse Discover in the sidebar.\n You can also paste a feed URL, or the path to an OPML file exported from another podcast app.";
        f.render_widget(Paragraph::new(msg).block(block("Welcome", focused)), area);
        return;
    }
    if app.view.rows.is_empty() {
        let msg = match app.view.kind {
            Kind::Inbox => "\n Nothing new in the last 30 days. Episodes from your shows land here as they come out.",
            Kind::InProgress => "\n Nothing in progress. Episodes you start and don't finish wait here.",
            Kind::Queue => "\n The queue is empty. Press a on an episode to add it, A to play it next.\n When an episode ends, the next one in the queue starts.",
            Kind::Discover if app.cards_pending() => "\n Loading charts from Apple Podcasts…",
            Kind::Discover => "\n Apple Podcasts sent nothing for this chart. o / O tries another genre.",
            Kind::Search(_) if app.cards_pending() => "\n Searching…",
            Kind::Search(_) => "\n No podcasts found. Try other words, or paste the show's feed URL.",
            Kind::Show(_) => "\n Loading…",
        };
        f.render_widget(Paragraph::new(msg).dim().block(block(&title, focused)), area);
        return;
    }
    if matches!(app.view.rows.first(), Some(Item::Show(_))) {
        return draw_cards(f, app, area, &title, focused);
    }
    let many_shows = !matches!(app.view.kind, Kind::Show(_));

    // only the rows that fit: some shows have thousands of episodes
    let (off, end, sel) = window(&mut app.view.state, app.view.rows.len(), area);
    let playing = app.now.as_ref().map(|n| n.guid.as_str());

    let rows: Vec<Row> = app.view.rows[off..end]
        .iter()
        .filter_map(|it| app.episode(it))
        .map(|(s, e)| {
            let l = app.state.get(&*e.guid).copied().unwrap_or_default();
            let queued = app.queue.iter().any(|(u, g)| *u == s.url && **g == *e.guid);
            let now = Some(&*e.guid) == playing;
            // the length the player measured beats the feed's own figure
            let dur = if l.dur > 0.0 { l.dur } else { e.duration_s as f64 };
            let (mark, len) = if now {
                ("▶", fmt_s(dur))
            } else if l.played {
                ("✓", fmt_s(dur))
            } else if l.pos > 0.0 {
                (progress_glyph(l.pos, dur), if dur > 0.0 { format!("{} left", fmt_s(dur - l.pos)) } else { fmt_s(l.pos) })
            } else {
                (" ", if dur > 0.0 { fmt_s(dur) } else { String::new() })
            };
            let mut title = vec![Span::raw(&*e.title)];
            if queued {
                title.insert(0, Span::raw("≡ ").fg(accent()));
            }
            let mut cells = vec![Cell::from(mark).fg(accent()), Cell::from(Line::from(title))];
            if many_shows {
                cells.push(Cell::from(s.show.title.as_str()).dim());
            }
            cells.push(Cell::from(fmt_date(e.published)).dim());
            cells.push(Cell::from(Line::from(len).right_aligned()));
            let row = Row::new(cells);
            if now {
                row.style(Style::new().fg(accent()).bold())
            } else if l.played {
                row.style(Style::new().dim())
            } else {
                row
            }
        })
        .collect();
    let mut widths = vec![Constraint::Length(1), Constraint::Fill(3)];
    let mut header = vec!["", "Episode"];
    if many_shows {
        widths.push(Constraint::Fill(1));
        header.push("Show");
    }
    widths.extend([Constraint::Length(10), Constraint::Length(13)]);
    header.extend(["Date", "Length"]);
    let table = Table::new(rows, widths)
        .header(Row::new(header).dim())
        .block(block(&title, focused))
        .row_highlight_style(highlight(focused))
        .column_spacing(1);
    let mut window = TableState::default().with_selected(Some(sel - off));
    f.render_stateful_widget(table, area, &mut window);
}

/// The rows that fit, keeping the selection in view; returns (first, end, selected) indexes.
fn window(state: &mut TableState, n: usize, area: Rect) -> (usize, usize, usize) {
    let visible = area.height.saturating_sub(3).max(1) as usize;
    let sel = state.selected().unwrap_or(0).min(n.saturating_sub(1));
    let mut off = state.offset().min(n.saturating_sub(1));
    if sel < off {
        off = sel;
    } else if sel >= off + visible {
        off = sel + 1 - visible;
    }
    *state.offset_mut() = off;
    (off, (off + visible).min(n), sel)
}

/// Discover's genres in a line, the current one lit, scrolled so it stays in view.
fn genre_strip(app: &App, width: u16) -> Line<'static> {
    let names: Vec<&str> = std::iter::once("All").chain(crate::discover::GENRES.iter().map(|g| g.0)).collect();
    let cur = app.genre;
    // start far enough left that the current genre fits
    let mut start = cur;
    let mut used: usize = names[cur].chars().count() + 3;
    while start > 0 && used + names[start - 1].chars().count() + 3 <= width as usize / 2 {
        start -= 1;
        used += names[start].chars().count() + 3;
    }
    let mut spans = vec![Span::raw(if start > 0 { " ‹ " } else { " " }).dim()];
    for (i, n) in names.iter().enumerate().skip(start) {
        spans.push(if i == cur { Span::styled(format!(" {n} "), Style::new().fg(on_accent()).bg(accent()).bold()) } else { Span::raw(format!(" {n} ")).dim() });
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// Discover and Search: shows, with ✓ on the ones you follow.
fn draw_cards(f: &mut Frame, app: &mut App, area: Rect, title: &str, focused: bool) {
    let area = if app.view.kind == Kind::Discover {
        let [strip, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(3)]).areas(area);
        f.render_widget(Paragraph::new(genre_strip(app, strip.width)), strip);
        rest
    } else {
        area
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let (off, end, sel) = window(&mut app.view.state, app.view.rows.len(), area);
    let rows: Vec<Row> = app.view.rows[off..end]
        .iter()
        .filter_map(|it| if let Item::Show(c) = it { Some(c) } else { None })
        .map(|c| {
            let followed = app.follows(c).is_some();
            let tag = if c.tag == "for you" { Span::raw("for you").fg(accent()) } else { Span::raw(c.tag.as_str()).dim() };
            let mut row = Row::new(vec![
                Cell::from(if followed { "✓" } else { " " }).fg(accent()),
                Cell::from(tag),
                Cell::from(Line::from(vec![Span::raw(c.title.as_str()), Span::raw(if c.explicit { " [E]" } else { "" }).dim()])),
                Cell::from(c.author.as_str()).dim(),
                Cell::from(c.genre.as_str()),
                Cell::from(Line::from(if c.episodes > 0 { c.episodes.to_string() } else { String::new() }).right_aligned()).dim(),
                Cell::from(Line::from(crate::discover::ago(now, c.updated)).right_aligned()).dim(),
            ]);
            if followed {
                row = row.style(Style::new().dim());
            }
            row
        })
        .collect();
    let widths = [Constraint::Length(1), Constraint::Length(7), Constraint::Fill(3), Constraint::Fill(2), Constraint::Length(18), Constraint::Length(8), Constraint::Length(10)];
    let table = Table::new(rows, widths)
        .header(Row::new(vec!["", "", "Show", "By", "Genre", "Episodes", "Latest"]).dim())
        .block(block(title, focused))
        .row_highlight_style(highlight(focused))
        .column_spacing(1);
    let mut w = TableState::default().with_selected(Some(sel - off));
    f.render_stateful_widget(table, area, &mut w);
}

/// `L`: show notes in place of the table, scrolled with j/k.
fn draw_notes(f: &mut Frame, app: &App, area: Rect) {
    let Some(n) = &app.notes else { return };
    let blk = block(&n.title, true).title_bottom(Line::from(" L or Esc closes · j/k scroll ").dim().right_aligned());
    let mut lines: Vec<Line> = n.lines.iter().skip(n.scroll).map(|l| Line::raw(l.as_str())).collect();
    if !n.links.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::styled("Links", Style::new().fg(accent()).bold()));
        lines.extend(n.links.iter().map(|u| Line::raw(u.as_str()).dim()));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(blk), area);
}

/// ◔ ◑ ◕: how far into an episode you are, at a glance.
fn progress_glyph(pos: f64, dur: f64) -> &'static str {
    match if dur > 0.0 { pos / dur } else { 0.0 } {
        f if f < 0.375 => "◔",
        f if f < 0.625 => "◑",
        _ => "◕",
    }
}

/// The right side of the now-playing line: what has a value, then the speed as a pill that's
/// lit when it isn't 1×, so the state is visible at a glance (like trill's flags).
fn now_flags(app: &App) -> Line<'static> {
    let info = Style::new().dim();
    let mut s = Vec::new();
    if app.now.as_ref().is_some_and(|n| n.buffering) {
        s.push(Span::styled("buffering…  ", Style::new().fg(accent())));
    }
    if let Some(z) = app.sleep_label() {
        s.push(Span::styled(format!("{z}  "), Style::new().fg(accent())));
    }
    // what's left in real time at your speed: what a podcast listener plans around
    if let Some(n) = app.now.as_ref().filter(|n| n.duration > 0.0) {
        s.push(Span::styled(format!("{} left  ", fmt_s((n.duration - n.pos).max(0.0) / app.speed as f64)), info));
    }
    if !app.queue.is_empty() {
        s.push(Span::styled(format!("queue {}  ", app.queue.len()), info));
    }
    s.push(Span::styled(format!("vol {}%  ", app.prefs.volume), info));
    let lit = (app.speed - 1.0).abs() > 0.01;
    let style = if lit { Style::new().fg(Color::Black).bg(accent()).bold() } else { Style::new().fg(Color::DarkGray) };
    s.push(Span::styled(format!(" {:.1}× ", app.speed), style));
    Line::from(s)
}

fn draw_now(f: &mut Frame, app: &mut App, area: Rect) {
    let [line1, line2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    let flags = now_flags(app);
    app.bar_area = Rect::default();
    let Some(n) = &app.now else {
        f.render_widget(Paragraph::new(" ■ Nothing playing").dim(), line1);
        f.render_widget(Paragraph::new(flags).right_aligned(), line1);
        return;
    };
    let [l, r] = Layout::horizontal([Constraint::Min(10), Constraint::Length((flags.width() as u16).min(line1.width / 2))]).areas(line1);
    let icon = if n.playing { "▶" } else { "⏸" };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {icon} "), Style::new().fg(accent()).bold()),
            Span::styled(n.title.as_str(), Style::new().fg(accent()).bold()),
            Span::raw("  "),
            Span::styled(n.show.as_str(), Style::new().dim()),
            Span::styled(n.chapter().map(|(i, t)| format!("  · {} {t}", i + 1)).unwrap_or_default(), Style::new().fg(accent())),
        ])),
        l,
    );
    f.render_widget(Paragraph::new(flags).right_aligned(), r);

    // " 12:03 ██████▌░░░░░ 41:10 ": solid bar, 1/8-cell precision, gradient fill (like trill)
    let ratio = if n.duration > 0.0 { (n.pos / n.duration).clamp(0.0, 1.0) } else { 0.0 };
    let (ta, tb) = (format!(" {} ", fmt_s(n.pos)), format!(" {} ", if n.duration > 0.0 { fmt_s(n.duration) } else { "--:--".into() }));
    let bar = (line2.width as usize).saturating_sub(ta.len() + tb.len());
    app.bar_area = Rect { x: line2.x + ta.len() as u16, width: bar as u16, ..line2 };
    let eighths = (ratio * (bar * 8) as f64) as usize;
    let track = Color::Rgb(55, 55, 60);
    let last = bar.max(2) as f32 - 1.0;
    let fill = |i: usize| if n.playing { grad(i as f32 / last) } else { Color::Rgb(120, 120, 120) };
    let mut spans = vec![Span::styled(ta, Style::new().dim())];
    spans.extend((0..bar).map(|i| match (eighths / 8).cmp(&i) {
        std::cmp::Ordering::Greater => Span::styled("█", Style::new().fg(fill(i))),
        std::cmp::Ordering::Equal if !eighths.is_multiple_of(8) => Span::styled(["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"][eighths % 8], Style::new().fg(fill(i)).bg(track)),
        _ => Span::styled(" ", Style::new().bg(track)),
    }));
    spans.push(Span::styled(tb, Style::new().dim()));
    f.render_widget(Paragraph::new(Line::from(spans)), line2);
}

const HELP: [(&str, &[(&str, &str)]); 4] = [
    ("Move", &[
        ("↑↓ jk", "move · PgUp/PgDn · g/G top/bottom"),
        ("Tab h l", "switch pane"),
        ("Enter", "open · play"),
        ("Esc", "back"),
        ("w", "jump to the playing episode"),
    ]),
    ("Listen", &[
        ("Space", "play / pause"),
        ("← →  , .", "back 15 s / forward 30 s"),
        ("p", "back to the start"),
        ("[ ]", "speed −/+ 0.1× (pitch stays)"),
        ("e", "this show keeps this speed"),
        ("I E", "skip this show's intro / outro"),
        ("z", "sleep timer"),
        ("c C", "next chapter / back one"),
        ("+ -", "volume ±5%"),
    ]),
    ("Queue", &[
        ("a", "add to the queue (again: take out)"),
        ("A", "play next"),
        ("n", "next in the queue"),
        ("m", "mark played / unplayed"),
    ]),
    ("Shows", &[
        ("/", "search · paste a feed URL / OPML"),
        ("f", "follow the highlighted show"),
        ("o O", "Discover: next / previous genre"),
        ("x", "For you: not interested"),
        ("M", "more from this show's maker"),
        ("s", "sort a show: date / length"),
        ("L", "show notes"),
        ("R", "refresh every show now"),
        ("D D", "unsubscribe"),
        ("t  i", "next colour theme · cover on/off"),
        ("?  q", "this help · quit (also Ctrl-C)"),
    ]),
];

fn help_lines(groups: &[(&str, &[(&str, &str)])]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (i, (name, keys)) in groups.iter().enumerate() {
        if i > 0 {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(name.to_string(), Style::new().fg(accent()).bold())));
        for (k, what) in keys.iter() {
            lines.push(Line::from(vec![Span::styled(format!(" {k:<10}"), Style::new().bold()), Span::raw(what.to_string())]));
        }
    }
    lines
}

/// `?`: every key in a centred pane, two columns when there's room.
fn draw_help(f: &mut Frame) {
    let area = f.area();
    let two = area.width >= 100;
    let (left, right) = if two { (help_lines(&HELP[..2]), help_lines(&HELP[2..])) } else { (help_lines(&HELP), Vec::new()) };
    let w = if two { 92 } else { 50 }.min(area.width.saturating_sub(2));
    let h = (left.len().max(right.len()) as u16 + 3).min(area.height.saturating_sub(2));
    let rect = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, rect);
    let blk = block("Keys · any key closes", true);
    let inner = blk.inner(rect);
    f.render_widget(blk, rect);
    if two {
        let [l, r] = Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(inner);
        f.render_widget(Paragraph::new(left), l);
        f.render_widget(Paragraph::new(right), r);
    } else {
        f.render_widget(Paragraph::new(left), inner);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn help_shows_every_key() {
        for (w, h) in [(120, 40), (60, 40)] {
            let mut t = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            t.draw(super::draw_help).unwrap();
            let screen: String = t.backend().buffer().content().iter().map(|c| c.symbol()).collect();
            for (_, keys) in super::HELP {
                for (_, what) in keys {
                    assert!(screen.contains(what), "{w}x{h} is missing {what}");
                }
            }
        }
    }

    #[test]
    fn progress_glyphs() {
        assert_eq!(super::progress_glyph(100.0, 1000.0), "◔");
        assert_eq!(super::progress_glyph(500.0, 1000.0), "◑");
        assert_eq!(super::progress_glyph(900.0, 1000.0), "◕");
        assert_eq!(super::progress_glyph(100.0, 0.0), "◔", "unknown length");
    }

    #[test]
    fn logo_rows_line_up() {
        let w = super::LOGO[0].chars().count();
        assert!(super::LOGO.iter().all(|r| r.chars().count() == w));
    }
}
