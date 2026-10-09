//! Rendering. Layout, from top to bottom: title bar, tables sidebar beside the
//! data grid, status line, then key help.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Widget};
use ratatui_cheese::fieldset::{Fieldset, FieldsetFill};
use ratatui_cheese::help::{Binding, Help, HelpStyles};
use ratatui_cheese::input::Input;
use ratatui_cheese::paginator::{Paginator, PaginatorMode, PaginatorState, PaginatorStyles};
use ratatui_cheese::theme::Palette;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::{App, EditorKind, Focus, Popup, StatusKind, choice_label};
use crate::db::{ColKind, Column};
use crate::favorites::{self, MAX_FAVORITES};
use crate::fuzzy;

const STAR: Color = Color::Rgb(0xFF, 0xD7, 0x5F);
const SIDEBAR_WIDTH: u16 = 32;
const COL_GAP: u16 = 2;
const MAX_COL_WIDTH: usize = 40;
const MAX_DROPDOWN_ROWS: usize = 8;

pub fn draw(f: &mut Frame, app: &mut App) {
    let p = app.palette.clone();
    let area = f.area().inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 0,
    });
    let help = help(app, &p);
    let help_h = if app.show_full_help {
        help.required_height().max(1)
    } else {
        1
    };
    let [title, _, body, status, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
        Constraint::Length(help_h),
    ])
    .areas(area);

    draw_title(f.buffer_mut(), title, app, &p);

    let side_w = SIDEBAR_WIDTH.min(body.width / 3).max(16);
    let [side, _, grid] =
        Layout::horizontal([Constraint::Length(side_w), Constraint::Length(2), Constraint::Fill(1)]).areas(body);
    draw_sidebar(f, side, app, &p);
    draw_grid(f.buffer_mut(), grid, app, &p);
    draw_status(f.buffer_mut(), status, app, &p);
    f.render_widget(&help, footer);

    match &app.popup {
        Popup::Editor(_) => draw_editor(f, app, &p),
        Popup::Favorites(_) => {
            let screen = f.area();
            draw_favorites(f.buffer_mut(), screen, app, &p);
        }
        Popup::None => {}
    }
}

// -------------------------------------------------------------------------
// Title bar
// -------------------------------------------------------------------------

fn draw_title(buf: &mut Buffer, area: Rect, app: &App, p: &Palette) {
    let left = Line::from(vec![
        Span::styled(
            " qbench ",
            Style::new()
                .fg(p.on_highlight)
                .bg(p.primary)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled("⛁ ", Style::new().fg(p.secondary)),
        Span::styled(app.conn_label.clone(), Style::new().fg(p.muted)),
    ]);
    let left_w = left.width() as u16;
    left.render(area, buf);

    // Favorite shortcuts on the right, as many as fit.
    let mut spans: Vec<Span> = Vec::new();
    let room = area.width.saturating_sub(left_w + 4) as usize;
    let mut used = 0;
    for (i, t) in app.favorites.list().iter().enumerate() {
        let name = t.short();
        let w = name.width() + 4;
        if used + w > room {
            break;
        }
        used += w;
        spans.push(Span::styled(
            format!("{}", favorites::shortcut(i)),
            Style::new().fg(STAR).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {name}  "), Style::new().fg(p.faint)));
    }
    Paragraph::new(Line::from(spans))
        .alignment(Alignment::Right)
        .render(area, buf);
}

// -------------------------------------------------------------------------
// Sidebar
// -------------------------------------------------------------------------

fn section_title_style(p: &Palette, focused: bool) -> Style {
    if focused {
        Style::new().fg(p.primary).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(p.muted)
    }
}

fn draw_sidebar(f: &mut Frame, area: Rect, app: &mut App, p: &Palette) {
    let focused = app.focus == Focus::Tables && matches!(app.popup, Popup::None);
    let bottom = format!(
        "{} rel · ★ {}/{MAX_FAVORITES}",
        app.tables.len(),
        app.favorites.list().len()
    );
    let fs = Fieldset::new()
        .title("Tables")
        .title_bottom(&bottom)
        .bottom_alignment(Alignment::Right)
        .fill(FieldsetFill::Dash)
        .palette(p)
        .title_style(section_title_style(p, focused));
    let inner = fs.inner(area);
    f.render_widget(&fs, area);
    if inner.height < 3 {
        return;
    }

    let filter_area = Rect { height: 1, ..inner };
    let placeholder = if app.filter_active {
        "fuzzy filter…"
    } else {
        "press / to filter"
    };
    let input = Input::new("").prompt("/").placeholder(placeholder).palette(p);
    f.render_stateful_widget(&input, filter_area, &mut app.filter);

    let list = Rect {
        y: inner.y + 2,
        height: inner.height - 2,
        ..inner
    };
    let buf = f.buffer_mut();
    if app.visible_tables.is_empty() {
        let msg = if app.tables_loaded {
            Span::styled("  no matches", Style::new().fg(p.faint))
        } else {
            Span::styled(
                format!("  {} loading…", app.spinner.frame_str()),
                Style::new().fg(p.muted),
            )
        };
        buf.set_span(list.x, list.y, &msg, list.width);
        return;
    }

    let h = list.height as usize;
    if app.table_sel < app.table_scroll {
        app.table_scroll = app.table_sel;
    } else if app.table_sel >= app.table_scroll + h {
        app.table_scroll = app.table_sel + 1 - h;
    }
    let query = app.filter.value().to_string();
    for (row, &ti) in app.visible_tables.iter().enumerate().skip(app.table_scroll).take(h) {
        let info = &app.tables[ti];
        let y = list.y + (row - app.table_scroll) as u16;
        let selected = row == app.table_sel;
        let is_open = app.data.as_ref().is_some_and(|d| d.info.table == info.table);

        let base = match (selected, focused) {
            (true, true) => Style::new().fg(p.primary).add_modifier(Modifier::BOLD),
            (true, false) => Style::new().fg(p.foreground),
            _ if is_open => Style::new().fg(p.secondary),
            _ => Style::new().fg(p.muted),
        };
        let mut spans = vec![
            if selected {
                Span::styled("│ ", Style::new().fg(if focused { p.primary } else { p.muted }))
            } else {
                Span::raw("  ")
            },
            if app.favorites.contains(&info.table) {
                Span::styled("★ ", Style::new().fg(STAR))
            } else {
                Span::raw("  ")
            },
        ];
        let name = info.table.short();
        let hits = fuzzy::positions(&query, &name);
        let dot = if info.table.schema == "public" {
            0
        } else {
            info.table.schema.chars().count() + 1
        };
        spans.extend(name.chars().enumerate().map(|(i, c)| {
            let mut st = if i < dot && !selected {
                Style::new().fg(p.faint)
            } else {
                base
            };
            if hits.contains(&i) {
                st = st.fg(p.highlight).add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            Span::styled(c.to_string(), st)
        }));
        let kind = info.kind.label();
        if !kind.is_empty() {
            spans.push(Span::styled(
                format!(" {kind}"),
                Style::new().fg(p.faint).add_modifier(Modifier::ITALIC),
            ));
        }
        buf.set_line(list.x, y, &Line::from(spans), list.width);
    }
}

// -------------------------------------------------------------------------
// Grid
// -------------------------------------------------------------------------

/// Display form of a cell: control characters folded so a value stays on one line.
fn display_value(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' => '↵',
            '\t' => ' ',
            c if c.is_control() => '·',
            c => c,
        })
        .collect()
}

fn truncate(s: &str, width: usize) -> String {
    if s.width() <= width {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

fn is_numeric(c: &Column) -> bool {
    let t = c.type_name.as_str();
    [
        "smallint",
        "integer",
        "bigint",
        "numeric",
        "real",
        "double precision",
        "money",
        "oid",
    ]
    .iter()
    .any(|n| t.starts_with(n))
}

fn column_marker(c: &Column) -> &'static str {
    match c.kind {
        ColKind::Enum(_) | ColKind::Bool => " ▾",
        ColKind::Other => "",
    }
}

fn draw_grid(buf: &mut Buffer, area: Rect, app: &mut App, p: &Palette) {
    app.cell_anchor = None;
    let focused = app.focus == Focus::Grid;
    let Some(d) = &app.data else {
        let fs = Fieldset::new().fill(FieldsetFill::Dash).palette(p);
        let inner = fs.inner(area);
        fs.render(area, buf);
        let lines = match &app.loading {
            Some(t) => vec![
                Line::styled(app.spinner.frame_str().to_string(), Style::new().fg(p.primary)),
                Line::raw(""),
                Line::styled(format!("loading {}…", t.full()), Style::new().fg(p.muted)),
            ],
            None => vec![
                Line::styled("◆", Style::new().fg(p.primary)),
                Line::raw(""),
                Line::styled("pick a table to start editing", Style::new().fg(p.foreground)),
                Line::styled(
                    "enter open · / filter · f favorite · F favorites",
                    Style::new().fg(p.faint),
                ),
            ],
        };
        let y = inner.y + inner.height.saturating_sub(lines.len() as u16) / 2;
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .render(Rect { y, height: 4, ..inner }, buf);
        return;
    };

    let first = d.page * d.page_size;
    let title = format!(
        "{}{}",
        d.info.table.full(),
        if d.info.kind.editable() { "" } else { " (read-only)" }
    );
    let bottom = if d.rows.is_empty() {
        format!("empty · {} rows", d.total)
    } else {
        format!("rows {}–{} of {}", first + 1, first + d.rows.len(), d.total)
    };
    let fs = Fieldset::new()
        .title(&title)
        .title_bottom(&bottom)
        .bottom_alignment(Alignment::Right)
        .fill(FieldsetFill::Dash)
        .palette(p)
        .title_style(section_title_style(p, focused));
    let inner = fs.inner(area);
    fs.render(area, buf);
    if inner.height < 4 || d.columns.is_empty() {
        return;
    }

    // Column widths from header, type and the values on this page.
    let widths: Vec<usize> = d
        .columns
        .iter()
        .enumerate()
        .map(|(ci, c)| {
            let head = c.name.width() + if c.is_pk { 2 } else { 0 };
            let ty = (c.type_name.width() + column_marker(c).width()).min(18);
            let vals = d
                .rows
                .iter()
                .map(|r| r.values[ci].as_deref().map_or(4, |v| display_value(v).width()))
                .max()
                .unwrap_or(0);
            head.max(ty).max(vals).clamp(3, MAX_COL_WIDTH)
        })
        .collect();

    let gutter = (first + d.rows.len()).max(1).to_string().len() as u16 + 1;
    let avail = inner.width.saturating_sub(gutter + 1) as usize;
    let span_width =
        |from: usize, to: usize| -> usize { widths[from..=to].iter().sum::<usize>() + (to - from) * COL_GAP as usize };
    if app.cur_col < app.col_scroll {
        app.col_scroll = app.cur_col;
    }
    while app.col_scroll < app.cur_col && span_width(app.col_scroll, app.cur_col) > avail {
        app.col_scroll += 1;
    }

    let rows_h = (inner.height - 3) as usize;
    app.grid_rows_visible = rows_h;
    if app.cur_row < app.row_scroll {
        app.row_scroll = app.cur_row;
    } else if app.cur_row >= app.row_scroll + rows_h {
        app.row_scroll = app.cur_row + 1 - rows_h;
    }

    // Visible columns with their x positions; the last one may be clipped.
    let x0 = inner.x + gutter + 1;
    let right = inner.right();
    let mut cols: Vec<(usize, u16, usize)> = Vec::new();
    let mut x = x0;
    for (ci, &w) in widths.iter().enumerate().skip(app.col_scroll) {
        if x >= right {
            break;
        }
        let w = w.min((right - x) as usize);
        cols.push((ci, x, w));
        x += w as u16 + COL_GAP;
    }
    let more_left = app.col_scroll > 0;
    let more_right = cols
        .last()
        .is_some_and(|&(ci, _, w)| ci + 1 < widths.len() || w < widths[ci]);

    // Header: name line, type line, rule.
    let (hy, ty, ry) = (inner.y, inner.y + 1, inner.y + 2);
    for &(ci, x, w) in &cols {
        let c = &d.columns[ci];
        let current = ci == app.cur_col;
        let mut name_style = Style::new().fg(p.secondary).add_modifier(Modifier::BOLD);
        if current && focused {
            name_style = name_style.fg(p.primary).add_modifier(Modifier::UNDERLINED);
        }
        let mut spans = Vec::new();
        if c.is_pk {
            spans.push(Span::styled("◆ ", Style::new().fg(STAR)));
        }
        spans.push(Span::styled(c.name.clone(), name_style));
        buf.set_line(x, hy, &Line::from(spans), w as u16);
        let ty_text = truncate(&format!("{}{}", c.type_name, column_marker(c)), w);
        buf.set_string(x, ty, ty_text, Style::new().fg(p.faint).add_modifier(Modifier::ITALIC));
    }
    buf.set_string(inner.x, ry, "─".repeat(inner.width as usize), Style::new().fg(p.border));
    if more_left {
        buf.set_string(inner.x, hy, "‹", Style::new().fg(p.primary));
    }
    if more_right {
        buf.set_string(right - 1, hy, "›", Style::new().fg(p.primary));
    }

    // Rows.
    for (ri, row) in d.rows.iter().enumerate().skip(app.row_scroll).take(rows_h) {
        let y = inner.y + 3 + (ri - app.row_scroll) as u16;
        let row_selected = ri == app.cur_row;
        if row_selected {
            buf.set_style(
                Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: 1,
                },
                Style::new().bg(p.surface),
            );
        }
        let num = format!("{:>width$}", first + ri + 1, width = gutter as usize - 1);
        let num_style = if row_selected {
            Style::new().fg(p.primary)
        } else {
            Style::new().fg(p.faint)
        };
        buf.set_string(inner.x, y, num, num_style);

        for &(ci, x, w) in &cols {
            let c = &d.columns[ci];
            let cell_selected = row_selected && ci == app.cur_col;
            let (text, mut style) = match &row.values[ci] {
                None => (
                    "NULL".to_string(),
                    Style::new().fg(p.faint).add_modifier(Modifier::ITALIC),
                ),
                Some(v) => {
                    let fg = match (&c.kind, v.as_str()) {
                        (ColKind::Bool, "true") => p.success,
                        (ColKind::Bool, _) => p.error,
                        (ColKind::Enum(_), _) => p.secondary,
                        _ => p.foreground,
                    };
                    (display_value(v), Style::new().fg(fg))
                }
            };
            let text = truncate(&text, w);
            let pad = if is_numeric(c) {
                w.saturating_sub(text.width())
            } else {
                0
            };
            if cell_selected {
                let cell = Rect {
                    x: x.saturating_sub(1),
                    y,
                    width: (w as u16 + 2).min(right - x + 1),
                    height: 1,
                };
                let hl = if focused {
                    Style::new()
                        .bg(p.highlight)
                        .fg(p.on_highlight)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::new().bg(p.border)
                };
                buf.set_style(cell, hl);
                style = style.patch(hl);
                app.cell_anchor = Some(cell);
            }
            buf.set_stringn(x + pad as u16, y, text, w, style);
        }
    }
}

// -------------------------------------------------------------------------
// Status line + help
// -------------------------------------------------------------------------

fn draw_status(buf: &mut Buffer, area: Rect, app: &App, p: &Palette) {
    let left = if app.pending > 0 {
        Line::from(vec![
            Span::styled(app.spinner.frame_str().to_string(), Style::new().fg(p.primary)),
            Span::styled(" working…", Style::new().fg(p.muted)),
        ])
    } else if let Some(s) = &app.status {
        let (icon, color) = match s.kind {
            StatusKind::Success => ("✓", p.success),
            StatusKind::Error => ("✗", p.error),
            StatusKind::Info => ("•", p.secondary),
        };
        Line::from(vec![
            Span::styled(format!("{icon} "), Style::new().fg(color).add_modifier(Modifier::BOLD)),
            Span::styled(
                s.text.clone(),
                Style::new().fg(if s.kind == StatusKind::Error { p.error } else { p.muted }),
            ),
        ])
    } else if let (Focus::Grid, Some(d)) = (app.focus, &app.data)
        && let Some(c) = d.columns.get(app.cur_col)
    {
        let mut spans = vec![
            Span::styled(c.name.clone(), Style::new().fg(p.secondary)),
            Span::styled(format!(" {}", c.type_name), Style::new().fg(p.faint)),
        ];
        if !c.nullable {
            spans.push(Span::styled(" not null", Style::new().fg(p.faint)));
        }
        if let ColKind::Enum(l) = &c.kind {
            spans.push(Span::styled(format!(" · {} values", l.len()), Style::new().fg(p.faint)));
        }
        if !d.has_pk() && d.info.kind.editable() {
            spans.push(Span::styled(
                " · no primary key, rows addressed by ctid",
                Style::new().fg(p.faint),
            ));
        }
        Line::from(spans)
    } else {
        Line::default()
    };
    left.render(area, buf);

    let Some(d) = &app.data else { return };
    let pos = format!(
        "  row {}/{}  col {}/{}",
        d.page * d.page_size + app.cur_row + 1,
        d.total,
        app.cur_col + 1,
        d.columns.len()
    );
    let pos_w = pos.width() as u16;
    let mut state = PaginatorState::new(d.total.max(0) as usize, d.page_size);
    for _ in 0..d.page {
        state.next_page();
    }
    let pages = state.total_pages();
    let (mode, pw) = if pages <= 20 {
        (PaginatorMode::Dots, pages as u16)
    } else {
        (
            PaginatorMode::Arabic,
            format!("{}/{}", d.page + 1, pages).width() as u16,
        )
    };
    let x = area.right().saturating_sub(pos_w + if pages > 1 { pw } else { 0 });
    if pages > 1 {
        let pager = Paginator::default().mode(mode).styles(PaginatorStyles::from_palette(p));
        ratatui::widgets::StatefulWidget::render(
            &pager,
            Rect {
                x,
                y: area.y,
                width: pw,
                height: 1,
            },
            buf,
            &mut state,
        );
    }
    buf.set_string(
        area.right().saturating_sub(pos_w),
        area.y,
        pos,
        Style::new().fg(p.muted),
    );
}

fn help(app: &App, p: &Palette) -> Help {
    let short: Vec<Binding> = match (&app.popup, app.focus) {
        (Popup::Editor(e), _) => match e.kind {
            EditorKind::Choice { .. } => vec![
                Binding::new("type", "fuzzy find"),
                Binding::new("↑/↓", "select"),
                Binding::new("enter", "apply"),
                Binding::new("esc", "cancel"),
            ],
            EditorKind::Text => vec![
                Binding::new("enter", "save"),
                Binding::new("ctrl+n", "set NULL"),
                Binding::new("ctrl+u", "clear"),
                Binding::new("esc", "cancel"),
            ],
        },
        (Popup::Favorites(_), _) => vec![
            Binding::new("1-0", "jump"),
            Binding::new("enter", "open"),
            Binding::new("d", "remove"),
            Binding::new("esc", "close"),
        ],
        (Popup::None, _) if app.filter_active => vec![
            Binding::new("type", "filter"),
            Binding::new("↑/↓", "select"),
            Binding::new("enter", "open"),
            Binding::new("esc", "clear"),
        ],
        (Popup::None, Focus::Tables) => vec![
            Binding::new("↑/↓", "move"),
            Binding::new("enter", "open"),
            Binding::new("/", "filter"),
            Binding::new("f", "favorite"),
            Binding::new("F", "favorites"),
            Binding::new("1-0", "jump"),
            Binding::new("?", "more"),
            Binding::new("q", "quit"),
        ],
        (Popup::None, Focus::Grid) => vec![
            Binding::new("←↑↓→", "move"),
            Binding::new("enter", "edit"),
            Binding::new("[/]", "page"),
            Binding::new("f", "favorite"),
            Binding::new("F", "favorites"),
            Binding::new("r", "refresh"),
            Binding::new("esc", "tables"),
            Binding::new("?", "more"),
        ],
    };
    let groups = vec![
        vec![
            Binding::new("←↑↓→/hjkl", "move"),
            Binding::new("g/G", "first/last row"),
            Binding::new("^/$", "first/last col"),
            Binding::new("pgup/pgdn", "scroll"),
            Binding::new("[/]", "prev/next page"),
        ],
        vec![
            Binding::new("enter/e", "edit cell"),
            Binding::new("ctrl+n", "NULL (text)"),
            Binding::new("r", "refresh"),
            Binding::new("tab", "switch pane"),
            Binding::new("/", "filter tables"),
        ],
        vec![
            Binding::new("f", "toggle favorite"),
            Binding::new("F", "favorites list"),
            Binding::new("1-9, 0", "open favorite"),
            Binding::new("alt+1-0", "open (anywhere)"),
        ],
        vec![
            Binding::new("?", "close help"),
            Binding::new("q", "quit"),
            Binding::new("ctrl+c", "quit"),
        ],
    ];
    Help::default()
        .bindings(short)
        .binding_groups(groups)
        .show_all(app.show_full_help)
        .short_separator(" · ")
        .styles(HelpStyles::from_palette(p))
}

// -------------------------------------------------------------------------
// Popups
// -------------------------------------------------------------------------

fn popup_block<'a>(p: &Palette, title: Line<'a>, hint: &'a str) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(p.primary))
        .title(title)
        .title_bottom(Line::styled(format!(" {hint} "), Style::new().fg(p.faint)).right_aligned())
}

/// Places a `w`×`h` box just below `anchor` (or above it if there is no room), inside `screen`.
fn place_near(anchor: Option<Rect>, w: u16, h: u16, screen: Rect) -> Rect {
    let w = w.min(screen.width);
    let h = h.min(screen.height);
    let Some(a) = anchor else {
        return Rect {
            x: screen.x + (screen.width - w) / 2,
            y: screen.y + (screen.height - h) / 2,
            width: w,
            height: h,
        };
    };
    let x = a.x.min(screen.right() - w);
    let y = if a.bottom() + h <= screen.bottom() {
        a.bottom()
    } else {
        a.y.saturating_sub(h).max(screen.y)
    };
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

fn draw_editor(f: &mut Frame, app: &mut App, p: &Palette) {
    let screen = f.area();
    let anchor = app.cell_anchor;
    let Popup::Editor(ed) = &mut app.popup else { return };
    let Some(col) = app.data.as_ref().and_then(|d| d.columns.get(ed.col)) else {
        return;
    };
    let title = Line::from(vec![
        Span::raw(" "),
        Span::styled(
            col.name.clone(),
            Style::new().fg(p.secondary).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {} ", col.type_name), Style::new().fg(p.faint)),
    ]);
    let anchor_w = anchor.map_or(0, |a| a.width);

    match &ed.kind {
        EditorKind::Choice { options, filtered, sel } => {
            let label_w = options.iter().map(|o| choice_label(o).width()).max().unwrap_or(4) + 12;
            let w = (label_w.max(title.width() + 4).max(30) as u16).max(anchor_w);
            let shown = filtered.len().clamp(1, MAX_DROPDOWN_ROWS);
            let h = shown as u16 + 4;
            let rect = place_near(anchor, w, h, screen);
            let block = popup_block(p, title, "↑↓ · enter · esc");
            let inner = block.inner(rect);
            f.render_widget(Clear, rect);
            f.render_widget(block, rect);
            let input = Input::new("").prompt("›").placeholder("fuzzy search…").palette(p);
            f.render_stateful_widget(&input, Rect { height: 1, ..inner }, &mut ed.input);

            let buf = f.buffer_mut();
            let count = format!("{}/{}", filtered.len(), options.len());
            buf.set_string(
                inner.right().saturating_sub(count.width() as u16),
                inner.y,
                &count,
                Style::new().fg(p.faint),
            );
            buf.set_string(
                inner.x,
                inner.y + 1,
                "─".repeat(inner.width as usize),
                Style::new().fg(p.border),
            );

            let list = Rect {
                y: inner.y + 2,
                height: inner.height.saturating_sub(2),
                ..inner
            };
            if filtered.is_empty() {
                buf.set_string(
                    list.x + 2,
                    list.y,
                    "no match",
                    Style::new().fg(p.faint).add_modifier(Modifier::ITALIC),
                );
                return;
            }
            let h = list.height as usize;
            let scroll = sel.saturating_sub(h.saturating_sub(1));
            let query = ed.input.value().to_string();
            for (row, &oi) in filtered.iter().enumerate().skip(scroll).take(h) {
                let y = list.y + (row - scroll) as u16;
                let selected = row == *sel;
                let opt = &options[oi];
                let label = choice_label(opt);
                let mut base = match opt.as_deref() {
                    None => Style::new().fg(p.faint).add_modifier(Modifier::ITALIC),
                    Some("true") if matches!(col.kind, ColKind::Bool) => Style::new().fg(p.success),
                    Some("false") if matches!(col.kind, ColKind::Bool) => Style::new().fg(p.error),
                    Some(_) => Style::new().fg(p.foreground),
                };
                if selected {
                    buf.set_style(Rect { y, height: 1, ..list }, Style::new().bg(p.surface));
                    base = base.add_modifier(Modifier::BOLD);
                    if opt.is_some() && matches!(col.kind, ColKind::Enum(_)) {
                        base = base.fg(p.primary);
                    }
                }
                let hits = if opt.is_some() {
                    fuzzy::positions(&query, label)
                } else {
                    Vec::new()
                };
                let mut spans = vec![if selected {
                    Span::styled("▸ ", Style::new().fg(p.primary))
                } else {
                    Span::raw("  ")
                }];
                spans.extend(label.chars().enumerate().map(|(i, c)| {
                    let st = if hits.contains(&i) {
                        base.fg(p.highlight).add_modifier(Modifier::UNDERLINED)
                    } else {
                        base
                    };
                    Span::styled(c.to_string(), st)
                }));
                if *opt == ed.current {
                    spans.push(Span::styled("  ● current", Style::new().fg(p.faint)));
                }
                buf.set_line(list.x, y, &Line::from(spans), list.width);
            }
        }
        EditorKind::Text => {
            let w = (ed.input.value().width() as u16 + 6)
                .clamp(44, screen.width.saturating_sub(4).max(44))
                .max(anchor_w);
            let w = w.min(80.max(anchor_w));
            let hint = if col.nullable {
                "enter save · ctrl+n NULL · esc"
            } else {
                "enter save · esc"
            };
            let rect = place_near(anchor, w, 3, screen);
            let block = popup_block(p, title, hint);
            let inner = block.inner(rect);
            f.render_widget(Clear, rect);
            f.render_widget(block, rect);
            let placeholder = if ed.current.is_none() { "NULL" } else { "empty string" };
            let input = Input::new("").prompt("›").placeholder(placeholder).palette(p);
            f.render_stateful_widget(&input, inner, &mut ed.input);
        }
    }
}

fn draw_favorites(buf: &mut Buffer, screen: Rect, app: &App, p: &Palette) {
    let Popup::Favorites(picker) = &app.popup else { return };
    let favs = app.favorites.list();
    let w = favs.iter().map(|t| t.full().width()).max().unwrap_or(0).max(36) as u16 + 14;
    let h = favs.len().max(1) as u16 + 4;
    let rect = place_near(None, w, h, screen);
    let title = Line::from(vec![
        Span::styled(" ★ ", Style::new().fg(STAR)),
        Span::styled("Favorites ", Style::new().fg(p.secondary).add_modifier(Modifier::BOLD)),
        Span::styled(format!("{}/{MAX_FAVORITES} ", favs.len()), Style::new().fg(p.faint)),
    ]);
    let block = popup_block(p, title, "1-0 jump · enter · d remove · esc");
    let inner = block.inner(rect).inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 1,
    });
    Clear.render(rect, buf);
    block.render(rect, buf);

    if favs.is_empty() {
        let lines = vec![
            Line::styled("no favorites yet", Style::new().fg(p.muted)),
            Line::styled("press f on a table to add one", Style::new().fg(p.faint)),
        ];
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .render(Rect { height: 2, ..inner }, buf);
        return;
    }
    for (i, t) in favs.iter().enumerate().take(inner.height as usize) {
        let y = inner.y + i as u16;
        let selected = i == picker.sel;
        if selected {
            buf.set_style(Rect { y, height: 1, ..inner }, Style::new().bg(p.surface));
        }
        let exists = !app.tables_loaded || app.tables.iter().any(|x| &x.table == t);
        let name_style = match (selected, exists) {
            (_, false) => Style::new().fg(p.faint).add_modifier(Modifier::CROSSED_OUT),
            (true, _) => Style::new().fg(p.primary).add_modifier(Modifier::BOLD),
            (false, _) => Style::new().fg(p.foreground),
        };
        let mut spans = vec![
            Span::styled(if selected { "▸ " } else { "  " }, Style::new().fg(p.primary)),
            Span::styled(
                format!(" {} ", favorites::shortcut(i)),
                Style::new()
                    .fg(p.on_highlight)
                    .bg(if selected { p.primary } else { p.secondary })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
        ];
        if t.schema != "public" {
            spans.push(Span::styled(format!("{}.", t.schema), Style::new().fg(p.faint)));
        }
        spans.push(Span::styled(t.name.clone(), name_style));
        if !exists {
            spans.push(Span::styled("  missing", Style::new().fg(p.error)));
        }
        buf.set_line(inner.x, y, &Line::from(spans), inner.width);
    }
}
