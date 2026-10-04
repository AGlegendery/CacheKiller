use std::sync::atomic::Ordering;
use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Gauge, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Table, Wrap,
};

use crate::app::{App, LARGE_THRESHOLD, Mode, SizeView};
use crate::locations::Category;
use crate::platform::{ADMIN, LOCK_MARK, ROOT_LABEL, SPINNER};
use crate::util::{fmt_age, fmt_count, fmt_size, now_secs, trunc_left, trunc_right};

const CYAN: Color = Color::Cyan;
const GRAY: Color = Color::Indexed(245);
const DIM: Color = Color::Indexed(240);
const GREEN: Color = Color::Indexed(120);
const RED: Color = Color::Indexed(203);
const YELLOW: Color = Color::Indexed(221);
const ACCENT: Color = Color::Indexed(68);

fn cat_color(c: Category) -> Color {
    match c {
        Category::Temp => Color::Indexed(180),
        Category::Cache => Color::Indexed(117),
        Category::Browser => Color::Indexed(75),
        Category::App => Color::Indexed(141),
        Category::Dev => Color::Indexed(150),
        Category::Game => Color::Indexed(213),
        Category::Package => Color::Indexed(179),
        Category::Log => Color::Indexed(250),
        Category::Crash => Color::Indexed(210),
        Category::Trash => Color::Indexed(174),
    }
}

/// "CacheKiller": green "Cache", red→pink "Killer" (256-colour gradient).
fn brand() -> Vec<Span<'static>> {
    "CacheKiller"
        .chars()
        .zip([120u8, 121, 122, 123, 159, 196, 197, 198, 199, 200, 201])
        .map(|(c, col)| Span::styled(c.to_string(), Style::new().fg(Color::Indexed(col)).bold()))
        .collect()
}

pub fn draw(f: &mut Frame, app: &mut App) {
    app.tick = app.tick.wrapping_add(1);
    let area = f.area();
    if area.width < 72 || area.height < 22 {
        let msg = Paragraph::new(vec![
            Line::from(brand()),
            Line::from(format!("Terminal too small: {}x{} — need at least 72x22", area.width, area.height)).fg(YELLOW),
        ])
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
        f.render_widget(msg, center(area, area.width, 3));
        return;
    }

    let [head, stats, table, details, logs, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(2),
        Constraint::Min(6),
        Constraint::Length(5),
        Constraint::Length(4),
        Constraint::Length(1),
    ])
    .areas(area);

    draw_header(f, app, head);
    draw_stats(f, app, stats);
    draw_table(f, app, table);
    draw_details(f, app, details);
    draw_logs(f, app, logs);
    draw_footer(f, app, footer);

    match app.mode {
        Mode::Scanning => draw_scanning(f, app, area),
        Mode::Deleting => draw_deleting(f, app, area),
        Mode::Confirm => draw_confirm(f, app, area),
        Mode::Help => draw_help(f, area),
        Mode::Browse => {}
    }
}

fn draw_header(f: &mut Frame, app: &App, area: Rect) {
    let ctx = &app.ctx;
    let mut l1 = vec![Span::raw(" ")];
    l1.extend(brand());
    let os = if cfg!(windows) { " for Windows " } else { " for Linux " };
    l1.push(Span::styled(os, Style::new().fg(CYAN).bold()));
    l1.push(Span::styled(concat!("v", env!("CARGO_PKG_VERSION")), Style::new().fg(GRAY)));
    l1.push(Span::styled("  by AGlegend", Style::new().fg(DIM)));

    let who = if ctx.elevated_for_user {
        Span::styled(format!("{ROOT_LABEL} → cleaning for {}", ctx.user), Style::new().fg(GREEN))
    } else if ctx.is_admin {
        Span::styled(ROOT_LABEL.to_string(), Style::new().fg(GREEN))
    } else {
        Span::styled(format!("{} (press E for {ADMIN}: system items locked)", ctx.user), Style::new().fg(YELLOW))
    };
    let right = Line::from(vec![who, Span::raw(" ")]).alignment(Alignment::Right);

    let l2 = Line::from(vec![
        Span::styled(" Some junk files are hidden from your eyes!! But not after ", Style::new().fg(ACCENT)),
    ]
    .into_iter()
    .chain(brand())
    .collect::<Vec<_>>());

    let [r1, r2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    f.render_widget(Line::from(l1), r1);
    f.render_widget(right, r1);
    f.render_widget(l2, r2);
}

fn draw_stats(f: &mut Frame, app: &App, area: Rect) {
    let total: u64 = app.items.iter().map(|i| i.stats.reclaimable()).sum();
    let locked: u64 = app.items.iter().map(|i| i.stats.locked).sum();
    let shared: u64 = app.items.iter().map(|i| i.stats.shared).sum();
    let (nsel, ssel) = app.selected();
    fn label(s: impl Into<String>) -> Span<'static> {
        Span::styled(s.into(), Style::new().fg(GRAY))
    }
    fn val(s: String) -> Span<'static> {
        Span::styled(s, Style::new().fg(GREEN).bold())
    }

    let mut l1 = vec![
        label(" Reclaimable "),
        val(fmt_size(total)),
        label("   Selected "),
        val(format!("{nsel}")),
        label(format!(" / {} ", app.items.len())),
        val(format!("({})", fmt_size(ssel))),
        label("   Freed this session "),
        val(fmt_size(app.freed_session)),
    ];
    if locked > 0 {
        l1.push(label("   Locked "));
        l1.push(Span::styled(fmt_size(locked), Style::new().fg(YELLOW)));
        if !app.ctx.is_admin {
            l1.push(label(format!(" (needs {ADMIN})")));
        }
    }

    let l2 = match (&app.flash, &app.last_scan) {
        (Some((msg, at)), _) if at.elapsed() < Duration::from_secs(4) => {
            Line::from(Span::styled(format!(" {msg}"), Style::new().fg(YELLOW)))
        }
        (_, Some(s)) => {
            let rate = s.files as f64 / s.elapsed.as_secs_f64().max(0.001);
            let mut parts = vec![format!(
                " Scanned {} files in {:.2}s ({}/s)",
                fmt_count(s.files),
                s.elapsed.as_secs_f64(),
                fmt_count(rate as u64)
            )];
            if shared > 0 {
                parts.push(format!("{} hard-linked (not counted)", fmt_size(shared)));
            }
            if s.hidden_tmp > 0 {
                parts.push(format!(
                    "{} temp entries newer than {} kept",
                    s.hidden_tmp,
                    fmt_age(app.ctx.tmp_min_age_secs)
                ));
            }
            Line::from(Span::styled(parts.join(" · "), Style::new().fg(DIM)))
        }
        _ => Line::from(""),
    };
    let [r1, r2] = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(area);
    f.render_widget(Line::from(l1), r1);
    f.render_widget(l2, r2);
}

fn draw_table(f: &mut Frame, app: &mut App, area: Rect) {
    let filter = app.filter.map(|c| c.name()).unwrap_or("all");
    let title = Line::from(vec![
        Span::styled(" Items ", Style::new().fg(CYAN).bold()),
        Span::styled(
            format!("· filter {filter} · sort {} · size {} ", app.sort.name(), app.size_view.name()),
            Style::new().fg(GRAY),
        ),
    ]);
    let count = Line::from(Span::styled(
        format!(" {}/{} ", if app.view.is_empty() { 0 } else { app.cursor + 1 }, app.view.len()),
        Style::new().fg(GRAY),
    ))
    .alignment(Alignment::Right);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM))
        .title(title)
        .title(count);
    let inner = block.inner(area);
    f.render_widget(block, area);

    app.page = inner.height.saturating_sub(1) as usize;
    // Re-clamp scrolling for the real page size.
    if app.cursor >= app.offset + app.page.max(1) {
        app.offset = app.cursor + 1 - app.page.max(1);
    }

    if app.view.is_empty() {
        let msg = if app.mode == Mode::Scanning { "Scanning…" } else { "Nothing to clean here." };
        f.render_widget(Paragraph::new(msg).fg(GRAY).alignment(Alignment::Center), center(inner, inner.width, 1));
        return;
    }

    let size_hdr = match app.size_view {
        SizeView::Reclaimable => "SIZE",
        SizeView::Disk => "DISK",
        SizeView::Apparent => "APPARENT",
    };
    let header = Row::new(["", "", "TYPE", size_hdr, "CATEGORY", "AGE", "PATH"]).style(Style::new().fg(GRAY).bold());
    let widths = [
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Length(9),
        Constraint::Length(8),
        Constraint::Length(4),
        Constraint::Fill(1),
    ];
    let path_w = inner.width.saturating_sub(1 + 3 + 4 + 9 + 8 + 4 + 7) as usize;
    let now = now_secs();

    let end = (app.offset + app.page).min(app.view.len());
    let rows: Vec<Row> = (app.offset..end)
        .map(|vi| {
            let it = &app.items[app.view[vi]];
            let is_cur = vi == app.cursor;
            let locked = !it.selectable();
            let size = app.size_view.of(it);
            let mark = if it.selected {
                Span::styled("[x]", Style::new().fg(GREEN).bold())
            } else if locked {
                Span::styled(LOCK_MARK, Style::new().fg(DIM))
            } else {
                Span::styled("[ ]", Style::new().fg(GRAY))
            };
            let size_style = if locked {
                Style::new().fg(DIM)
            } else if size > LARGE_THRESHOLD {
                Style::new().fg(RED).bold()
            } else if size > 20 * 1024 * 1024 {
                Style::new().fg(YELLOW)
            } else {
                Style::new()
            };
            let path = app.label(it);
            let mut row = Row::new(vec![
                Cell::from(if is_cur { "›" } else { " " }).fg(GREEN),
                Cell::from(mark),
                Cell::from(it.kind.name()).fg(GRAY),
                Cell::from(Line::from(fmt_size(size)).alignment(Alignment::Right)).style(size_style),
                Cell::from(it.category.name()).fg(if locked { DIM } else { cat_color(it.category) }),
                Cell::from(fmt_age(now - it.stats.newest)).fg(GRAY),
                Cell::from(trunc_left(&path, path_w)).fg(if locked { DIM } else { Color::Reset }),
            ]);
            if is_cur {
                row = row.style(Style::new().bg(Color::Indexed(236)).add_modifier(Modifier::BOLD));
            }
            row
        })
        .collect();

    f.render_widget(Table::new(rows, widths).header(header).column_spacing(1), inner);

    if app.view.len() > app.page {
        let mut sb = ScrollbarState::new(app.view.len().saturating_sub(app.page)).position(app.offset);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None),
            area.inner(ratatui::layout::Margin { vertical: 1, horizontal: 0 }),
            &mut sb,
        );
    }
}

fn draw_details(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM))
        .title(Span::styled(" Details ", Style::new().fg(CYAN).bold()));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(it) = app.current() else { return };
    let s = &it.stats;
    let k = |t: &'static str| Span::styled(t, Style::new().fg(GRAY));
    let v = |t: String| Span::styled(t, Style::new().fg(Color::Reset).bold());

    let w = inner.width as usize;
    let l1 = Line::from(vec![
        Span::styled(it.category.name(), Style::new().fg(cat_color(it.category)).bold()),
        Span::raw("  "),
        Span::raw(trunc_left(&it.path.display().to_string(), w.saturating_sub(it.category.name().len() + 2))),
    ]);
    let mut l2 = vec![
        k("Reclaimable "),
        Span::styled(fmt_size(s.reclaimable()), Style::new().fg(GREEN).bold()),
        k("  Disk "),
        v(fmt_size(s.disk)),
        k("  Apparent "),
        v(fmt_size(s.apparent)),
    ];
    if s.shared > 0 {
        l2.push(k("  Hard-linked "));
        l2.push(Span::styled(fmt_size(s.shared), Style::new().fg(YELLOW)));
    }
    if s.locked > 0 {
        l2.push(k("  Locked "));
        l2.push(Span::styled(fmt_size(s.locked), Style::new().fg(YELLOW)));
    }
    let mut l3 = vec![
        v(fmt_count(s.files)),
        k(" files  "),
        v(fmt_count(s.dirs)),
        k(" dirs  "),
        k("last change "),
        v(fmt_age(now_secs() - s.newest)),
        k(" ago  "),
        v(fmt_count(it.targets.len() as u64)),
        k(if it.targets.len() == 1 { " entry to delete" } else { " entries to delete" }),
    ];
    if s.errors > 0 {
        l3.push(k("  unreadable "));
        l3.push(Span::styled(fmt_count(s.errors), Style::new().fg(RED)));
    }
    if s.mounts > 0 {
        l3.push(k("  mounts skipped "));
        l3.push(Span::styled(fmt_count(s.mounts), Style::new().fg(YELLOW)));
    }
    f.render_widget(Paragraph::new(vec![l1, Line::from(l2), Line::from(l3)]), inner);
}

fn draw_logs(f: &mut Frame, app: &App, area: Rect) {
    let [left, right] = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
    let panel = |title: &'static str, color: Color, lines: &std::collections::VecDeque<String>, empty: &'static str, w: u16| {
        let w = w.saturating_sub(2) as usize;
        let body: Vec<Line> = if lines.is_empty() {
            vec![Line::from(Span::styled(empty, Style::new().fg(DIM)))]
        } else {
            lines.iter().take(2).map(|l| Line::from(trunc_right(&format!("[{}", l.get(12..).unwrap_or(l)), w)).fg(if color == RED { RED } else { GRAY })).collect()
        };
        Paragraph::new(body).block(
            Block::new()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(DIM))
                .title(Span::styled(title, Style::new().fg(color).bold())),
        )
    };
    f.render_widget(panel(" Recently deleted ", CYAN, &app.logger.ok, "(none yet)", left.width), left);
    f.render_widget(panel(" Failed ", RED, &app.logger.failed, "(none)", right.width), right);
}

fn draw_footer(f: &mut Frame, app: &App, area: Rect) {
    let keys: &[(&str, &str)] = match app.mode {
        Mode::Confirm => &[("y/Enter", "delete"), ("n/Esc", "cancel")],
        Mode::Scanning | Mode::Deleting => &[("q", "quit")],
        _ if !app.ctx.is_admin => &[
            ("↑↓", "move"),
            ("Space", "select"),
            ("Enter", "delete"),
            ("A", "all"),
            ("U", "none"),
            ("L", ">200MB"),
            ("Tab", "filter"),
            ("S", "sort"),
            ("R", "rescan"),
            ("E", ADMIN),
            ("?", "help"),
            ("q", "quit"),
        ],
        _ => &[
            ("↑↓", "move"),
            ("Space", "select"),
            ("Enter", "delete"),
            ("A", "all"),
            ("U", "none"),
            ("L", ">200MB"),
            ("C", "category"),
            ("Tab", "filter"),
            ("S", "sort"),
            ("B", "size"),
            ("R", "rescan"),
            ("?", "help"),
            ("q", "quit"),
        ],
    };
    let mut spans = vec![Span::raw(" ")];
    for (k, d) in keys {
        spans.push(Span::styled(*k, Style::new().fg(Color::Black).bg(Color::Indexed(109))));
        spans.push(Span::styled(format!(" {d}  "), Style::new().fg(GRAY)));
    }
    f.render_widget(Line::from(spans), area);
}

fn center(area: Rect, w: u16, h: u16) -> Rect {
    let [v] = Layout::vertical([Constraint::Length(h.min(area.height))]).flex(Flex::Center).areas(area);
    let [r] = Layout::horizontal([Constraint::Length(w.min(area.width))]).flex(Flex::Center).areas(v);
    r
}

fn popup(f: &mut Frame, area: Rect, w: u16, h: u16, title: Line<'static>, border: Color) -> Rect {
    let r = center(area, w, h);
    f.render_widget(Clear, r);
    let block = Block::new()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    inner
}

fn progress_box(f: &mut Frame, app: &App, area: Rect, title: &str, ratio: f64, gauge_label: String, lines: Vec<Line>) {
    let spin = SPINNER[app.tick / 2 % SPINNER.len()];
    let w = (area.width * 3 / 4).clamp(60, 100);
    let inner = popup(
        f,
        area,
        w,
        8,
        Line::from(vec![
            Span::styled(format!(" {spin} "), Style::new().fg(GREEN)),
            Span::styled(format!("{title} "), Style::new().fg(CYAN).bold()),
        ]),
        CYAN,
    );
    let [g, rest] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
    f.render_widget(
        Gauge::default()
            .gauge_style(Style::new().fg(GREEN).bg(Color::Indexed(236)))
            .ratio(ratio.clamp(0.0, 1.0))
            .label(gauge_label),
        Rect { height: 1, ..g },
    );
    f.render_widget(Paragraph::new(lines), rest);
}

fn draw_scanning(f: &mut Frame, app: &App, area: Rect) {
    let Some((_, p, started)) = &app.scan_job else { return };
    let done = p.done.load(Ordering::Relaxed);
    let total = p.total.load(Ordering::Relaxed);
    let files = p.files.load(Ordering::Relaxed);
    let bytes = p.bytes.load(Ordering::Relaxed);
    let secs = started.elapsed().as_secs_f64().max(0.001);
    let cur = p.current.lock().unwrap().clone();
    let w = (area.width * 3 / 4).clamp(60, 100) as usize - 14;
    let ratio = if total == 0 { 0.0 } else { done as f64 / total as f64 };
    progress_box(
        f,
        app,
        area,
        "Scanning cache locations",
        ratio,
        format!("{done}/{total} locations"),
        vec![
            Line::from(vec![
                Span::styled(fmt_count(files), Style::new().bold()),
                Span::styled(" files  ", Style::new().fg(GRAY)),
                Span::styled(fmt_size(bytes), Style::new().fg(GREEN).bold()),
                Span::styled("  ", Style::new()),
                Span::styled(format!("{}/s", fmt_count((files as f64 / secs) as u64)), Style::new().fg(GRAY)),
                Span::styled(format!("  {secs:.1}s"), Style::new().fg(GRAY)),
            ]),
            Line::from(vec![
                Span::styled("Examining: ", Style::new().fg(GRAY)),
                Span::raw(trunc_left(&cur, w)),
            ]),
        ],
    );
}

fn draw_deleting(f: &mut Frame, app: &App, area: Rect) {
    let Some((_, p, started)) = &app.del_job else { return };
    let done = p.done.load(Ordering::Relaxed);
    let total = p.total.load(Ordering::Relaxed);
    let cur = p.current.lock().unwrap().clone();
    let w = (area.width * 3 / 4).clamp(60, 100) as usize - 14;
    let ratio = if total == 0 { 0.0 } else { done as f64 / total as f64 };
    progress_box(
        f,
        app,
        area,
        "Deleting",
        ratio,
        format!("{done}/{total} items"),
        vec![
            Line::from(vec![
                Span::styled(fmt_count(p.removed.load(Ordering::Relaxed)), Style::new().bold()),
                Span::styled(" entries removed  ", Style::new().fg(GRAY)),
                Span::styled(fmt_size(p.freed.load(Ordering::Relaxed)), Style::new().fg(GREEN).bold()),
                Span::styled(" freed", Style::new().fg(GRAY)),
                Span::styled(format!("  {:.1}s", started.elapsed().as_secs_f64()), Style::new().fg(GRAY)),
            ]),
            Line::from(vec![Span::styled("Removing: ", Style::new().fg(GRAY)), Span::raw(trunc_left(&cur, w))]),
        ],
    );
}

fn draw_confirm(f: &mut Frame, app: &App, area: Rect) {
    let (n, size) = app.selected();
    let sel: Vec<_> = {
        let mut v: Vec<_> = app.items.iter().filter(|i| i.selected).collect();
        v.sort_by_key(|i| std::cmp::Reverse(i.stats.reclaimable()));
        v
    };
    let shown = sel.len().min(6);
    let w = (area.width * 3 / 4).clamp(60, 100);
    let mut lines = vec![
        Line::from(vec![
            Span::raw("Permanently delete "),
            Span::styled(format!("{n} item{}", if n == 1 { "" } else { "s" }), Style::new().bold()),
            Span::raw(" and free about "),
            Span::styled(fmt_size(size), Style::new().fg(GREEN).bold()),
            Span::raw("?"),
        ]),
        Line::from(""),
    ];
    for it in sel.iter().take(shown) {
        lines.push(Line::from(vec![
            Span::styled(format!("{:>9}  ", fmt_size(it.stats.reclaimable())), Style::new().fg(GRAY)),
            Span::raw(trunc_left(&app.label(it), w as usize - 16)),
        ]));
    }
    if sel.len() > shown {
        lines.push(Line::from(Span::styled(format!("           … and {} more", sel.len() - shown), Style::new().fg(GRAY))));
    }
    if sel.iter().any(|i| matches!(i.category, Category::Browser | Category::App)) {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Tip: close browsers / apps first so they don't rewrite their cache.",
            Style::new().fg(YELLOW),
        )));
    }
    if sel.iter().any(|i| i.category == Category::Trash) {
        lines.push(Line::from(Span::styled("Trash will be emptied — this cannot be undone.", Style::new().fg(RED))));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(vec![
        Span::styled(" y ", Style::new().fg(Color::Black).bg(GREEN).bold()),
        Span::raw(" delete    "),
        Span::styled(" n ", Style::new().fg(Color::Black).bg(Color::Indexed(245)).bold()),
        Span::raw(" cancel"),
    ]));
    let h = lines.len() as u16 + 2;
    let inner = popup(f, area, w, h, Line::from(Span::styled(" Confirm delete ", Style::new().fg(RED).bold())), RED);
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_help(f: &mut Frame, area: Rect) {
    let rows: &[(&str, &str)] = &[
        ("↑ ↓  j k", "Move cursor"),
        ("PgUp PgDn  g G", "Page / jump to top, bottom"),
        ("Space", "Toggle selection (and move down)"),
        ("Enter / Del", "Delete selected (asks to confirm)"),
        ("A / U", "Select all visible / unselect everything"),
        ("L", "Select items larger than 200 MB"),
        ("C", "Select every item in the current item's category"),
        ("Tab", "Filter by category (cycles, then back to all)"),
        ("S", "Sort: size → age → category → path"),
        ("B", "Size column: reclaimable → disk → apparent"),
        ("R", "Rescan"),
        ("E", if cfg!(windows) { "Restart as Administrator (UAC) to unlock system caches" } else { "Restart with sudo to unlock system caches" }),
        ("q / Esc", "Quit"),
        ("", ""),
        ("SIZE", "Disk space you'd actually get back (size on disk)."),
        ("", "Hard-linked & permission-locked bytes are excluded."),
        (LOCK_MARK.trim(), "Locked: no permission — press E to unlock."),
        ("AGE", "Time since anything inside last changed."),
    ];
    let lines: Vec<Line> = rows
        .iter()
        .map(|(k, d)| Line::from(vec![Span::styled(format!("{k:>16}  "), Style::new().fg(CYAN).bold()), Span::raw(*d)]))
        .collect();
    let h = lines.len() as u16 + 2;
    let inner = popup(f, area, 74, h, Line::from(Span::styled(" Help — any key to close ", Style::new().fg(CYAN).bold())), CYAN);
    f.render_widget(Paragraph::new(lines), inner);
}
