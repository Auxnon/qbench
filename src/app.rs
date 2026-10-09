//! Application state and input handling.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui_cheese::input::InputState;
use ratatui_cheese::spinner::{SpinnerState, SpinnerType};
use ratatui_cheese::theme::Palette;

use crate::db::{CellUpdate, ColKind, Db, Response, TableData, TableInfo, TableRef};
use crate::favorites::{self, Favorites};
use crate::fuzzy;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Tables,
    Grid,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Success,
    Error,
}

pub struct Status {
    pub text: String,
    pub kind: StatusKind,
    at: Instant,
}

/// One entry in a choice dropdown; `None` stands for SQL NULL.
pub type Choice = Option<String>;

pub enum EditorKind {
    /// Enum / bool columns: fuzzy-filtered dropdown.
    Choice {
        options: Vec<Choice>,
        filtered: Vec<usize>,
        sel: usize,
    },
    /// Everything else: free text, cast server-side.
    Text,
}

pub struct Editor {
    pub row: usize,
    pub col: usize,
    pub input: InputState,
    pub kind: EditorKind,
    pub current: Option<String>,
}

impl Editor {
    fn refilter(&mut self) {
        let query = self.input.value().to_string();
        if let EditorKind::Choice { options, filtered, sel } = &mut self.kind {
            *filtered = fuzzy::filter(&query, options.iter().map(choice_label));
            *sel = 0;
        }
    }
}

pub fn choice_label(c: &Choice) -> &str {
    c.as_deref().unwrap_or("NULL")
}

pub struct FavPicker {
    pub sel: usize,
}

pub enum Popup {
    None,
    Editor(Editor),
    Favorites(FavPicker),
}

pub struct App {
    pub palette: Palette,
    pub conn_label: String,
    db: Db,
    pub page_size: usize,

    pub tables: Vec<TableInfo>,
    pub tables_loaded: bool,
    pub filter: InputState,
    pub filter_active: bool,
    pub visible_tables: Vec<usize>,
    pub table_sel: usize,
    pub table_scroll: usize,

    pub favorites: Favorites,

    pub data: Option<TableData>,
    /// Table whose data is being fetched while `data` is empty.
    pub loading: Option<TableRef>,
    pub cur_row: usize,
    pub cur_col: usize,
    pub row_scroll: usize,
    pub col_scroll: usize,
    /// Set by the renderer: data rows that fit on screen, and where the selected cell was drawn.
    pub grid_rows_visible: usize,
    pub cell_anchor: Option<Rect>,

    pub focus: Focus,
    pub popup: Popup,
    pub show_full_help: bool,
    pub status: Option<Status>,
    pub pending: usize,
    pub spinner: SpinnerState,
    seq: u64,
    pub quit: bool,
}

impl App {
    pub fn new(db: Db, palette: Palette, conn_label: String, fav_key: String, page_size: usize) -> Self {
        let mut filter = InputState::new();
        filter.set_focused(false);
        let mut app = Self {
            palette,
            conn_label,
            db,
            page_size,
            tables: Vec::new(),
            tables_loaded: false,
            filter,
            filter_active: false,
            visible_tables: Vec::new(),
            table_sel: 0,
            table_scroll: 0,
            favorites: Favorites::load(fav_key),
            data: None,
            loading: None,
            cur_row: 0,
            cur_col: 0,
            row_scroll: 0,
            col_scroll: 0,
            grid_rows_visible: 1,
            cell_anchor: None,
            focus: Focus::Tables,
            popup: Popup::None,
            show_full_help: false,
            status: None,
            pending: 0,
            spinner: SpinnerState::new(SpinnerType::Dot),
            seq: 0,
            quit: false,
        };
        app.pending += 1;
        app.db.load_tables();
        app
    }

    pub fn tick(&mut self, dt: Duration) {
        self.spinner.tick(dt);
        if let Some(s) = &self.status {
            let ttl = if s.kind == StatusKind::Error { 12 } else { 5 };
            if s.at.elapsed() > Duration::from_secs(ttl) {
                self.status = None;
            }
        }
    }

    fn set_status(&mut self, kind: StatusKind, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            kind,
            at: Instant::now(),
        });
    }

    // ---------------------------------------------------------------------
    // Database responses
    // ---------------------------------------------------------------------

    pub fn on_response(&mut self, resp: Response) {
        self.pending = self.pending.saturating_sub(1);
        match resp {
            Response::Tables(tables) => {
                let n = tables.len();
                self.tables = tables;
                self.tables_loaded = true;
                self.refilter_tables();
                self.set_status(StatusKind::Info, format!("{n} relations"));
            }
            Response::Data { seq, data } => {
                if seq != self.seq {
                    return;
                }
                self.loading = None;
                let same_table = self
                    .data
                    .as_ref()
                    .is_some_and(|d| d.info.table == data.info.table && d.page == data.page);
                if !same_table {
                    self.cur_row = 0;
                    self.row_scroll = 0;
                    if self.data.as_ref().is_none_or(|d| d.info.table != data.info.table) {
                        self.cur_col = 0;
                        self.col_scroll = 0;
                    }
                }
                self.data = Some(data);
                self.clamp_cursor();
            }
            Response::RowUpdated {
                table,
                row_idx,
                column,
                row,
            } => {
                if let Some(d) = &mut self.data
                    && d.info.table == table
                    && row_idx < d.rows.len()
                {
                    d.rows[row_idx] = row;
                }
                self.set_status(StatusKind::Success, format!("saved {}.{column}", table.short()));
            }
            Response::Error(e) => {
                if self.pending == 0 {
                    self.loading = None;
                }
                self.set_status(StatusKind::Error, e)
            }
        }
    }

    fn clamp_cursor(&mut self) {
        if let Some(d) = &self.data {
            self.cur_row = self.cur_row.min(d.rows.len().saturating_sub(1));
            self.cur_col = self.cur_col.min(d.columns.len().saturating_sub(1));
        }
    }

    // ---------------------------------------------------------------------
    // Tables
    // ---------------------------------------------------------------------

    fn refilter_tables(&mut self) {
        let names: Vec<String> = self.tables.iter().map(|t| t.table.short()).collect();
        self.visible_tables = fuzzy::filter(self.filter.value(), names.iter().map(String::as_str));
        self.table_sel = self.table_sel.min(self.visible_tables.len().saturating_sub(1));
        if !self.filter.value().is_empty() {
            self.table_sel = 0;
        }
    }

    fn selected_table(&self) -> Option<&TableInfo> {
        self.visible_tables.get(self.table_sel).map(|&i| &self.tables[i])
    }

    fn open_table(&mut self, info: TableInfo, page: usize) {
        // Drop the old grid when switching tables so no key can edit it while the new one loads.
        if self.data.as_ref().is_some_and(|d| d.info.table != info.table) {
            self.data = None;
        }
        self.loading = Some(info.table.clone());
        self.seq += 1;
        self.pending += 1;
        self.db.load_table(self.seq, info, page, self.page_size);
    }

    fn open_table_ref(&mut self, t: &TableRef) {
        let Some(info) = self.tables.iter().find(|i| &i.table == t).cloned() else {
            if self.tables_loaded {
                self.set_status(StatusKind::Error, format!("{} no longer exists", t.full()));
            }
            return;
        };
        // Reveal it in the sidebar too.
        if let Some(pos) = self.tables.iter().position(|i| &i.table == t) {
            if !self.visible_tables.contains(&pos) {
                self.filter.set_value(String::new());
                self.refilter_tables();
            }
            if let Some(v) = self.visible_tables.iter().position(|&i| i == pos) {
                self.table_sel = v;
            }
        }
        self.open_table(info, 0);
        self.focus = Focus::Grid;
    }

    fn open_favorite(&mut self, idx: usize) {
        match self.favorites.list().get(idx).cloned() {
            Some(t) => {
                self.popup = Popup::None;
                self.open_table_ref(&t);
            }
            None => self.set_status(StatusKind::Info, format!("no favorite on {}", favorites::shortcut(idx))),
        }
    }

    fn toggle_favorite(&mut self) {
        let target = match self.focus {
            Focus::Tables => self.selected_table().map(|t| t.table.clone()),
            Focus::Grid => self.data.as_ref().map(|d| d.info.table.clone()),
        };
        let Some(t) = target else { return };
        match self.favorites.toggle(&t) {
            Ok(true) => {
                let n = self.favorites.list().len() - 1;
                let key = favorites::shortcut(n);
                self.set_status(StatusKind::Success, format!("★ {} → press {key} to jump", t.short()));
            }
            Ok(false) => self.set_status(StatusKind::Info, format!("removed {} from favorites", t.short())),
            Err(e) => self.set_status(StatusKind::Error, e),
        }
    }

    fn reload(&mut self) {
        if let Some(d) = &self.data {
            let (info, page) = (d.info.clone(), d.page);
            self.open_table(info, page);
        }
        self.pending += 1;
        self.db.load_tables();
    }

    // ---------------------------------------------------------------------
    // Keys
    // ---------------------------------------------------------------------

    pub fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        // Alt+digit jumps to a favorite from anywhere, even mid-typing.
        if alt
            && let KeyCode::Char(c) = key.code
            && let Some(i) = favorites::index_for_digit(c)
        {
            self.open_favorite(i);
            return;
        }
        if alt {
            return;
        }
        match self.popup {
            Popup::Editor(_) => return self.on_editor_key(key),
            Popup::Favorites(_) => return self.on_favorites_key(key),
            Popup::None => {}
        }
        if self.focus == Focus::Tables && self.filter_active {
            return self.on_filter_key(key);
        }

        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => self.show_full_help = !self.show_full_help,
            KeyCode::Char('f') => self.toggle_favorite(),
            KeyCode::Char('F') => self.popup = Popup::Favorites(FavPicker { sel: 0 }),
            KeyCode::Char('r') => self.reload(),
            KeyCode::Char(c) if c.is_ascii_digit() => {
                if let Some(i) = favorites::index_for_digit(c) {
                    self.open_favorite(i);
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Tables if self.data.is_some() => Focus::Grid,
                    _ => Focus::Tables,
                }
            }
            _ => match self.focus {
                Focus::Tables => self.on_tables_key(key),
                Focus::Grid => self.on_grid_key(key),
            },
        }
    }

    fn on_tables_key(&mut self, key: KeyEvent) {
        let n = self.visible_tables.len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.table_sel = self.table_sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.table_sel = (self.table_sel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.table_sel = self.table_sel.saturating_sub(10),
            KeyCode::PageDown => self.table_sel = (self.table_sel + 10).min(n.saturating_sub(1)),
            KeyCode::Home | KeyCode::Char('g') => self.table_sel = 0,
            KeyCode::End | KeyCode::Char('G') => self.table_sel = n.saturating_sub(1),
            KeyCode::Char('/') => {
                self.filter_active = true;
                self.filter.set_focused(true);
            }
            KeyCode::Esc if !self.filter.value().is_empty() => {
                self.filter.set_value(String::new());
                self.refilter_tables();
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_selected_table(),
            _ => {}
        }
    }

    fn open_selected_table(&mut self) {
        if let Some(info) = self.selected_table().cloned() {
            self.open_table(info, 0);
            self.focus = Focus::Grid;
        }
    }

    fn on_filter_key(&mut self, key: KeyEvent) {
        let n = self.visible_tables.len();
        match key.code {
            KeyCode::Esc => {
                self.filter_active = false;
                self.filter.set_focused(false);
                self.filter.set_value(String::new());
                self.refilter_tables();
            }
            KeyCode::Enter => {
                self.filter_active = false;
                self.filter.set_focused(false);
                self.open_selected_table();
            }
            KeyCode::Up => self.table_sel = self.table_sel.saturating_sub(1),
            KeyCode::Down => self.table_sel = (self.table_sel + 1).min(n.saturating_sub(1)),
            _ => {
                if edit_input(&mut self.filter, key) {
                    self.refilter_tables();
                }
            }
        }
    }

    fn on_grid_key(&mut self, key: KeyEvent) {
        let Some(d) = &self.data else {
            if self.loading.is_none() || key.code == KeyCode::Esc {
                self.focus = Focus::Tables;
            }
            return;
        };
        let (rows, cols) = (d.rows.len(), d.columns.len());
        let last_page = (d.total.max(0) as usize).saturating_sub(1) / d.page_size.max(1);
        let page = d.page;
        let jump = self.grid_rows_visible.max(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.cur_row = self.cur_row.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.cur_row = (self.cur_row + 1).min(rows.saturating_sub(1)),
            KeyCode::Left | KeyCode::Char('h') => self.cur_col = self.cur_col.saturating_sub(1),
            KeyCode::Right | KeyCode::Char('l') => self.cur_col = (self.cur_col + 1).min(cols.saturating_sub(1)),
            KeyCode::PageUp => self.cur_row = self.cur_row.saturating_sub(jump),
            KeyCode::PageDown => self.cur_row = (self.cur_row + jump).min(rows.saturating_sub(1)),
            KeyCode::Char('g') => self.cur_row = 0,
            KeyCode::Char('G') => self.cur_row = rows.saturating_sub(1),
            KeyCode::Home | KeyCode::Char('^') => self.cur_col = 0,
            KeyCode::End | KeyCode::Char('$') => self.cur_col = cols.saturating_sub(1),
            KeyCode::Char(']') if page < last_page => {
                let info = d.info.clone();
                self.open_table(info, page + 1);
            }
            KeyCode::Char('[') if page > 0 => {
                let info = d.info.clone();
                self.open_table(info, page - 1);
            }
            KeyCode::Enter | KeyCode::Char('e') => self.open_editor(),
            KeyCode::Esc => self.focus = Focus::Tables,
            _ => {}
        }
    }

    // ---------------------------------------------------------------------
    // Cell editor
    // ---------------------------------------------------------------------

    fn open_editor(&mut self) {
        let Some(d) = &self.data else { return };
        if !d.info.kind.editable() {
            let kind = d.info.kind.label();
            return self.set_status(StatusKind::Error, format!("{kind}s are read-only"));
        }
        let (Some(row), Some(col)) = (d.rows.get(self.cur_row), d.columns.get(self.cur_col)) else {
            return;
        };
        let current = row.values[self.cur_col].clone();
        let mut input = InputState::new();
        input.set_focused(true);
        let mut options: Vec<Choice> = match &col.kind {
            ColKind::Enum(labels) => labels.iter().cloned().map(Some).collect(),
            ColKind::Bool => vec![Some("true".into()), Some("false".into())],
            ColKind::Other => Vec::new(),
        };
        let kind = if options.is_empty() {
            if let Some(v) = &current {
                input.set_value(v.clone());
                input.end();
            }
            EditorKind::Text
        } else {
            if col.nullable {
                options.push(None);
            }
            let filtered: Vec<usize> = (0..options.len()).collect();
            let sel = options.iter().position(|o| *o == current).unwrap_or(0);
            EditorKind::Choice { options, filtered, sel }
        };
        self.popup = Popup::Editor(Editor {
            row: self.cur_row,
            col: self.cur_col,
            input,
            kind,
            current,
        });
    }

    fn on_editor_key(&mut self, key: KeyEvent) {
        let Popup::Editor(ed) = &mut self.popup else { return };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let mut commit: Option<Choice> = None;
        match (&mut ed.kind, key.code) {
            (_, KeyCode::Esc) => {
                self.popup = Popup::None;
                return;
            }
            (EditorKind::Choice { filtered, sel, .. }, KeyCode::Up | KeyCode::BackTab) => {
                *sel = if *sel == 0 {
                    filtered.len().saturating_sub(1)
                } else {
                    *sel - 1
                };
            }
            (EditorKind::Choice { filtered, sel, .. }, KeyCode::Down | KeyCode::Tab) => {
                *sel = if *sel + 1 >= filtered.len() { 0 } else { *sel + 1 };
            }
            (EditorKind::Choice { filtered, sel, .. }, KeyCode::Char('p')) if ctrl => {
                *sel = sel.saturating_sub(1).min(filtered.len().saturating_sub(1));
            }
            (EditorKind::Choice { filtered, sel, .. }, KeyCode::Char('n')) if ctrl => {
                *sel = (*sel + 1).min(filtered.len().saturating_sub(1));
            }
            (EditorKind::Choice { options, filtered, sel }, KeyCode::Enter) => match filtered.get(*sel) {
                Some(&i) => commit = Some(options[i].clone()),
                None => return,
            },
            (EditorKind::Text, KeyCode::Enter) => commit = Some(Some(ed.input.value().to_string())),
            (EditorKind::Text, KeyCode::Char('n')) if ctrl => commit = Some(None),
            _ => {
                if edit_input(&mut ed.input, key) {
                    ed.refilter();
                }
            }
        }
        let Some(value) = commit else { return };
        let (row, col, current) = (ed.row, ed.col, ed.current.clone());
        self.popup = Popup::None;
        if value == current {
            return;
        }
        let Some(d) = &self.data else { return };
        let column = &d.columns[col];
        if value.is_none() && !column.nullable {
            return self.set_status(StatusKind::Error, format!("{} is NOT NULL", column.name));
        }
        self.pending += 1;
        self.db.update_cell(CellUpdate {
            info: d.info.clone(),
            columns: d.columns.clone(),
            row_idx: row,
            row: d.rows[row].clone(),
            col,
            value,
        });
    }

    // ---------------------------------------------------------------------
    // Favorites popup
    // ---------------------------------------------------------------------

    fn on_favorites_key(&mut self, key: KeyEvent) {
        let Popup::Favorites(p) = &mut self.popup else { return };
        let n = self.favorites.list().len();
        match key.code {
            KeyCode::Esc | KeyCode::Char('F') | KeyCode::Char('q') => self.popup = Popup::None,
            KeyCode::Up | KeyCode::Char('k') => p.sel = p.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => p.sel = (p.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Enter => {
                let i = p.sel;
                self.open_favorite(i);
            }
            KeyCode::Char('d') | KeyCode::Char('x') | KeyCode::Delete => {
                let i = p.sel;
                p.sel = p.sel.min(n.saturating_sub(2));
                if let Err(e) = self.favorites.remove(i) {
                    self.set_status(StatusKind::Error, e);
                }
            }
            KeyCode::Char(c) => {
                if let Some(i) = favorites::index_for_digit(c) {
                    self.open_favorite(i);
                }
            }
            _ => {}
        }
    }
}

/// Applies a line-editing key to an input. Returns true if the text changed.
fn edit_input(input: &mut InputState, key: KeyEvent) -> bool {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    match key.code {
        KeyCode::Char('u') if ctrl => {
            input.set_value(String::new());
            true
        }
        KeyCode::Char('a') if ctrl => {
            input.home();
            false
        }
        KeyCode::Char('e') if ctrl => {
            input.end();
            false
        }
        KeyCode::Char(c) if !ctrl => {
            input.insert_char(c);
            true
        }
        KeyCode::Backspace => {
            input.delete_before();
            true
        }
        KeyCode::Delete => {
            input.delete_at();
            true
        }
        KeyCode::Left => {
            input.move_left();
            false
        }
        KeyCode::Right => {
            input.move_right();
            false
        }
        KeyCode::Home => {
            input.home();
            false
        }
        KeyCode::End => {
            input.end();
            false
        }
        _ => false,
    }
}
