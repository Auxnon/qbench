//! Application state and input handling.

use std::time::{Duration, Instant};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};
use ratatui_cheese::input::InputState;
use ratatui_cheese::spinner::{SpinnerState, SpinnerType};
use ratatui_cheese::theme::Palette;

use crate::clipboard;
use crate::db::{CellUpdate, CellValue, ColKind, DataRow, Db, InsertBatch, Response, TableData, TableInfo, TableRef};
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

/// One entry in a choice dropdown.
pub type Choice = CellValue;

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
    pub current: CellValue,
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
    c.label()
}

/// Where the cursor starts when a text editor opens (vim `i` / `a` / `s`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EditStart {
    Start,
    End,
    Replace,
}

/// Screen regions recorded by the renderer each frame, for mouse hit-testing.
#[derive(Default)]
pub struct Hits {
    pub sidebar: Rect,
    pub grid: Rect,
    /// Sidebar rows → index into `visible_tables`.
    pub tables: Vec<(Rect, usize)>,
    /// Grid cells → (row, column).
    pub cells: Vec<(Rect, usize, usize)>,
    pub popup: Option<Rect>,
    /// Popup rows → dropdown position or favorite index.
    pub options: Vec<(Rect, usize)>,
}

fn hit<T: Copy>(regions: &[(Rect, T)], pos: Position) -> Option<T> {
    regions.iter().find(|(r, _)| r.contains(pos)).map(|&(_, v)| v)
}

pub struct FavPicker {
    pub sel: usize,
}

pub enum Confirm {
    /// Commit staged rows; `collisions` lists drafts whose key already exists.
    Insert {
        table: TableRef,
        rows: Vec<DataRow>,
        collisions: Vec<(usize, String)>,
    },
    Quit {
        drafts: usize,
    },
}

pub enum Popup {
    None,
    Editor(Editor),
    Favorites(FavPicker),
    Confirm(Confirm),
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
    pub hits: Hits,

    /// First key of a two-key vim sequence (`gg`, `yy`, `cc`).
    pub pending_key: Option<char>,
    /// Last yanked cell.
    register: Option<CellValue>,

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
            hits: Hits::default(),
            pending_key: None,
            register: None,
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

    /// The table list shrinks to a narrow rail while the grid has focus.
    pub fn sidebar_collapsed(&self) -> bool {
        self.focus == Focus::Grid && (self.data.is_some() || self.loading.is_some())
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
                old_ctid,
                column,
                row,
            } => {
                if let Some(d) = &mut self.data
                    && d.info.table == table
                {
                    // Drafts may have been inserted since; prefer finding the row by its old ctid.
                    let idx = old_ctid
                        .as_ref()
                        .and_then(|c| d.rows.iter().position(|r| r.ctid.as_ref() == Some(c)))
                        .or((row_idx < d.rows.len() && !d.rows[row_idx].is_draft()).then_some(row_idx));
                    if let Some(i) = idx {
                        d.rows[i] = row;
                    }
                }
                self.set_status(StatusKind::Success, format!("saved {}.{column}", table.short()));
            }
            Response::InsertChecked { table, collisions } => {
                let Some(d) = &self.data else { return };
                if d.info.table != table || d.draft_count() == 0 {
                    return;
                }
                let rows = d.rows.iter().filter(|r| r.is_draft()).cloned().collect();
                self.popup = Popup::Confirm(Confirm::Insert {
                    table,
                    rows,
                    collisions,
                });
            }
            Response::Inserted {
                table,
                inserted,
                overwritten,
            } => {
                let mut msg = format!("inserted {inserted} row{}", if inserted == 1 { "" } else { "s" });
                if overwritten > 0 {
                    msg.push_str(&format!(", overwrote {overwritten}"));
                }
                self.set_status(StatusKind::Success, format!("{msg} into {}", table.short()));
                if let Some(d) = &mut self.data
                    && d.info.table == table
                {
                    d.rows.retain(|r| !r.is_draft());
                    let (info, page) = (d.info.clone(), d.page);
                    self.open_table(info, page);
                }
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

    fn draft_count(&self) -> usize {
        self.data.as_ref().map_or(0, TableData::draft_count)
    }

    /// Starts loading a table page. Refuses while drafts are staged, since a reload would drop them.
    fn open_table(&mut self, info: TableInfo, page: usize) -> bool {
        let drafts = self.draft_count();
        if drafts > 0 {
            self.set_status(
                StatusKind::Error,
                format!("{drafts} uncommitted draft row(s): ctrl+s to commit or dd to discard"),
            );
            return false;
        }
        // Drop the old grid when switching tables so no key can edit it while the new one loads.
        if self.data.as_ref().is_some_and(|d| d.info.table != info.table) {
            self.data = None;
        }
        self.loading = Some(info.table.clone());
        self.seq += 1;
        self.pending += 1;
        self.db.load_table(self.seq, info, page, self.page_size);
        true
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
        if self.open_table(info, 0) {
            self.focus = Focus::Grid;
        }
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
            if !self.open_table(info, page) {
                return;
            }
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
            Popup::Confirm(_) => return self.on_confirm_key(key),
            Popup::None => {}
        }
        if ctrl && key.code == KeyCode::Char('s') {
            return self.commit_drafts();
        }
        if self.focus == Focus::Tables && self.filter_active {
            return self.on_filter_key(key);
        }

        // Mid-sequence (`g…`, `y…`) and ctrl chords go straight to the pane.
        let pending = self.pending_key.take();
        if pending.is_some() || ctrl {
            return match self.focus {
                Focus::Tables => self.on_tables_key(key, pending),
                Focus::Grid => self.on_grid_key(key, pending),
            };
        }
        match key.code {
            KeyCode::Char('q') => match self.draft_count() {
                0 => self.quit = true,
                drafts => self.popup = Popup::Confirm(Confirm::Quit { drafts }),
            },
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
                Focus::Tables => self.on_tables_key(key, None),
                Focus::Grid => self.on_grid_key(key, None),
            },
        }
    }

    fn on_tables_key(&mut self, key: KeyEvent, pending: Option<char>) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last = self.visible_tables.len().saturating_sub(1);
        let sel = self.table_sel;
        match (pending, key.code) {
            (Some('g'), KeyCode::Char('g')) => self.table_sel = 0,
            (_, KeyCode::Char('g')) if !ctrl => self.pending_key = Some('g'),
            (_, KeyCode::Char('d')) if ctrl => self.table_sel = (sel + 10).min(last),
            (_, KeyCode::Char('u')) if ctrl => self.table_sel = sel.saturating_sub(10),
            (_, KeyCode::Char('l')) if ctrl => self.focus_grid(),
            _ if ctrl => {}
            (_, KeyCode::Up | KeyCode::Char('k')) => self.table_sel = sel.saturating_sub(1),
            (_, KeyCode::Down | KeyCode::Char('j')) => self.table_sel = (sel + 1).min(last),
            (_, KeyCode::PageUp) => self.table_sel = sel.saturating_sub(10),
            (_, KeyCode::PageDown) => self.table_sel = (sel + 10).min(last),
            (_, KeyCode::Home) => self.table_sel = 0,
            (_, KeyCode::End | KeyCode::Char('G')) => self.table_sel = last,
            (_, KeyCode::Char('/')) => self.start_filter(),
            (_, KeyCode::Esc) if !self.filter.value().is_empty() => {
                self.filter.set_value(String::new());
                self.refilter_tables();
            }
            (_, KeyCode::Enter | KeyCode::Right | KeyCode::Char('l' | 'o')) => self.open_selected_table(),
            _ => {}
        }
    }

    fn start_filter(&mut self) {
        self.focus = Focus::Tables;
        self.filter_active = true;
        self.filter.set_focused(true);
    }

    fn focus_grid(&mut self) {
        if self.data.is_some() || self.loading.is_some() {
            self.focus = Focus::Grid;
        }
    }

    fn open_selected_table(&mut self) {
        if let Some(info) = self.selected_table().cloned()
            && self.open_table(info, 0)
        {
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

    fn on_grid_key(&mut self, key: KeyEvent, pending: Option<char>) {
        let Some(d) = &self.data else {
            if self.loading.is_none() || key.code == KeyCode::Esc {
                self.focus = Focus::Tables;
            }
            return;
        };
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let last_row = d.rows.len().saturating_sub(1);
        let last_col = d.columns.len().saturating_sub(1);
        let last_page = (d.total.max(0) as usize).saturating_sub(1) / d.page_size.max(1);
        let page = d.page;
        let screen = self.grid_rows_visible.max(1);
        let half = (screen / 2).max(1);
        // Last row currently on screen, for H / M / L.
        let bottom = (self.row_scroll + screen - 1).min(last_row);
        let (row, col) = (self.cur_row, self.cur_col);
        match (pending, key.code) {
            // Two-key sequences.
            (Some('g'), KeyCode::Char('g')) => self.cur_row = 0,
            (Some('y'), KeyCode::Char('y')) => self.yank_cell(),
            (Some('c'), KeyCode::Char('c' | 'w' | 'W' | 'e' | 'E' | 'l' | '$')) => self.open_editor(EditStart::Replace),
            (Some('d'), KeyCode::Char('d')) => self.discard_draft(),
            (_, KeyCode::Char(c @ ('g' | 'y' | 'c' | 'd'))) if !ctrl => self.pending_key = Some(c),

            // Ctrl chords: scrolling and pane switching.
            (_, KeyCode::Char('d')) if ctrl => self.cur_row = (row + half).min(last_row),
            (_, KeyCode::Char('u')) if ctrl => self.cur_row = row.saturating_sub(half),
            (_, KeyCode::Char('f')) if ctrl => self.cur_row = (row + screen).min(last_row),
            (_, KeyCode::Char('b')) if ctrl => self.cur_row = row.saturating_sub(screen),
            (_, KeyCode::Char('h')) if ctrl => self.focus = Focus::Tables,
            _ if ctrl => {}

            // Motions.
            (_, KeyCode::Up | KeyCode::Char('k')) => self.cur_row = row.saturating_sub(1),
            (_, KeyCode::Down | KeyCode::Char('j')) => self.cur_row = (row + 1).min(last_row),
            (_, KeyCode::Left | KeyCode::Char('h' | 'b' | 'B')) => self.cur_col = col.saturating_sub(1),
            (_, KeyCode::Right | KeyCode::Char('l' | 'w' | 'W' | 'e' | 'E')) => self.cur_col = (col + 1).min(last_col),
            (_, KeyCode::PageUp) => self.cur_row = row.saturating_sub(screen),
            (_, KeyCode::PageDown) => self.cur_row = (row + screen).min(last_row),
            (_, KeyCode::Char('G')) => self.cur_row = last_row,
            (_, KeyCode::Char('H')) => self.cur_row = self.row_scroll.min(last_row),
            (_, KeyCode::Char('M')) => self.cur_row = (self.row_scroll + bottom) / 2,
            (_, KeyCode::Char('L')) => self.cur_row = bottom,
            (_, KeyCode::Home | KeyCode::Char('^')) => self.cur_col = 0,
            (_, KeyCode::End | KeyCode::Char('$')) => self.cur_col = last_col,
            (_, KeyCode::Char(']')) if page < last_page => {
                let info = d.info.clone();
                self.open_table(info, page + 1);
            }
            (_, KeyCode::Char('[')) if page > 0 => {
                let info = d.info.clone();
                self.open_table(info, page - 1);
            }

            // Editing.
            (_, KeyCode::Enter | KeyCode::Char('a' | 'A')) => self.open_editor(EditStart::End),
            (_, KeyCode::Char('i' | 'I')) => self.open_editor(EditStart::Start),
            (_, KeyCode::Char('s' | 'S')) => self.open_editor(EditStart::Replace),
            (_, KeyCode::Char('Y')) => self.yank_cell(),
            (_, KeyCode::Char('p' | 'P')) => self.paste_cell(),
            (_, KeyCode::Char('o')) => self.clone_row(false),
            (_, KeyCode::Char('O')) => self.clone_row(true),

            (_, KeyCode::Char('/')) => self.start_filter(),
            (_, KeyCode::Esc) => self.focus = Focus::Tables,
            _ => {}
        }
    }

    fn yank_cell(&mut self) {
        let Some(value) = self
            .data
            .as_ref()
            .and_then(|d| d.rows.get(self.cur_row))
            .filter(|r| self.cur_col < r.values.len())
            .map(|r| r.cell(self.cur_col))
        else {
            return;
        };
        let shown = match &value {
            CellValue::Value(v) => {
                let mut preview: String = v
                    .chars()
                    .take(40)
                    .map(|c| if c.is_control() { ' ' } else { c })
                    .collect();
                if v.chars().count() > 40 {
                    preview.push('…');
                }
                format!("yanked \"{preview}\"")
            }
            other => format!("yanked {} (clipboard gets an empty string)", other.label()),
        };
        let via = clipboard::copy(match &value {
            CellValue::Value(v) => v,
            _ => "",
        });
        self.register = Some(value);
        self.set_status(StatusKind::Success, format!("{shown} · {via}"));
    }

    fn paste_cell(&mut self) {
        match self.register.clone() {
            Some(value) => self.write_cell(self.cur_row, self.cur_col, value),
            None => self.set_status(StatusKind::Info, "nothing yanked yet (Y or yy)"),
        }
    }

    // ---------------------------------------------------------------------
    // Draft rows
    // ---------------------------------------------------------------------

    /// Stages a copy of the current row (below it, or above with `O`). Auto-filled
    /// keys are left to the database and shown as ✱ until given a value.
    fn clone_row(&mut self, above: bool) {
        let Some(d) = &mut self.data else { return };
        if !d.info.kind.editable() {
            let kind = d.info.kind.label();
            return self.set_status(StatusKind::Error, format!("can't insert into {kind}s"));
        }
        let Some(src) = d.rows.get(self.cur_row) else {
            return self.set_status(StatusKind::Info, "no row to clone");
        };
        let defaults: Vec<bool> = d
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| c.auto_key() || src.is_default(i))
            .collect();
        let values = src
            .values
            .iter()
            .zip(&defaults)
            .map(|(v, &def)| if def { None } else { v.clone() })
            .collect();
        let draft = DataRow {
            ctid: None,
            values,
            draft: Some(defaults),
        };
        let at = if above { self.cur_row } else { self.cur_row + 1 };
        d.rows.insert(at, draft);
        self.cur_row = at;
        let n = d.draft_count();
        self.set_status(
            StatusKind::Info,
            format!(
                "{n} draft row{} · edit freely · ctrl+s commit · dd discard",
                if n == 1 { "" } else { "s" }
            ),
        );
    }

    fn discard_draft(&mut self) {
        let Some(d) = &mut self.data else { return };
        if !d.rows.get(self.cur_row).is_some_and(DataRow::is_draft) {
            return self.set_status(
                StatusKind::Info,
                "dd only discards draft rows; deleting saved rows isn't supported",
            );
        }
        d.rows.remove(self.cur_row);
        let left = d.draft_count();
        self.clamp_cursor();
        self.set_status(StatusKind::Info, format!("draft discarded · {left} left"));
    }

    /// Checks drafts against existing keys; the reply opens the confirmation.
    fn commit_drafts(&mut self) {
        let Some(d) = &self.data else { return };
        let rows: Vec<DataRow> = d.rows.iter().filter(|r| r.is_draft()).cloned().collect();
        if rows.is_empty() {
            return self.set_status(StatusKind::Info, "no draft rows to commit (o clones a row)");
        }
        self.pending += 1;
        self.db.check_inserts(d.info.clone(), d.columns.clone(), rows);
    }

    fn on_confirm_key(&mut self, key: KeyEvent) {
        let yes = matches!(key.code, KeyCode::Char('y' | 'Y') | KeyCode::Enter);
        let no = matches!(key.code, KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc);
        if !yes && !no {
            return;
        }
        let Popup::Confirm(confirm) = std::mem::replace(&mut self.popup, Popup::None) else {
            return;
        };
        if no {
            return;
        }
        match confirm {
            Confirm::Quit { .. } => self.quit = true,
            Confirm::Insert {
                table,
                rows,
                collisions,
            } => {
                let Some(d) = self.data.as_ref().filter(|d| d.info.table == table) else {
                    return;
                };
                let overwrite = (0..rows.len())
                    .map(|i| collisions.iter().any(|&(c, _)| c == i))
                    .collect();
                self.pending += 1;
                self.db.insert_rows(InsertBatch {
                    info: d.info.clone(),
                    columns: d.columns.clone(),
                    rows,
                    overwrite,
                });
            }
        }
    }

    // ---------------------------------------------------------------------
    // Cell editor
    // ---------------------------------------------------------------------

    fn open_editor(&mut self, start: EditStart) {
        let Some(d) = &self.data else { return };
        if !d.info.kind.editable() {
            let kind = d.info.kind.label();
            return self.set_status(StatusKind::Error, format!("{kind}s are read-only"));
        }
        let (Some(row), Some(col)) = (d.rows.get(self.cur_row), d.columns.get(self.cur_col)) else {
            return;
        };
        if col.generated {
            return self.set_status(StatusKind::Error, format!("{} is a generated column", col.name));
        }
        let current = row.cell(self.cur_col);
        let mut input = InputState::new();
        input.set_focused(true);
        let mut options: Vec<Choice> = match &col.kind {
            ColKind::Enum(labels) => labels.iter().cloned().map(CellValue::Value).collect(),
            ColKind::Bool => vec![CellValue::Value("true".into()), CellValue::Value("false".into())],
            ColKind::Other => Vec::new(),
        };
        let kind = if options.is_empty() {
            if let CellValue::Value(v) = &current
                && start != EditStart::Replace
            {
                input.set_value(v.clone());
                if start == EditStart::End {
                    input.end();
                }
            }
            EditorKind::Text
        } else {
            if col.nullable {
                options.push(CellValue::Null);
            }
            if col.has_default {
                options.push(CellValue::Default);
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
            (EditorKind::Choice { filtered, sel, .. }, KeyCode::Char('p' | 'k')) if ctrl => {
                *sel = sel.saturating_sub(1).min(filtered.len().saturating_sub(1));
            }
            (EditorKind::Choice { filtered, sel, .. }, KeyCode::Char('n' | 'j')) if ctrl => {
                *sel = (*sel + 1).min(filtered.len().saturating_sub(1));
            }
            (EditorKind::Choice { options, filtered, sel }, KeyCode::Enter) => match filtered.get(*sel) {
                Some(&i) => commit = Some(options[i].clone()),
                None => return,
            },
            (EditorKind::Text, KeyCode::Enter) => commit = Some(CellValue::Value(ed.input.value().to_string())),
            (EditorKind::Text, KeyCode::Char('n')) if ctrl => commit = Some(CellValue::Null),
            (EditorKind::Text, KeyCode::Char('d')) if ctrl => commit = Some(CellValue::Default),
            _ => {
                if edit_input(&mut ed.input, key) {
                    ed.refilter();
                }
            }
        }
        let Some(value) = commit else { return };
        let (row, col) = (ed.row, ed.col);
        self.popup = Popup::None;
        self.write_cell(row, col, value);
    }

    /// Ok(false) when the write would change nothing.
    fn check_write(&self, row: usize, col: usize, value: &CellValue) -> Result<bool, String> {
        let Some(d) = &self.data else { return Ok(false) };
        if !d.info.kind.editable() {
            return Err(format!("{}s are read-only", d.info.kind.label()));
        }
        let (Some(r), Some(column)) = (d.rows.get(row), d.columns.get(col)) else {
            return Ok(false);
        };
        if r.cell(col) == *value {
            return Ok(false);
        }
        if column.generated {
            return Err(format!("{} is a generated column", column.name));
        }
        if *value == CellValue::Null && !column.nullable {
            return Err(format!("{} is NOT NULL", column.name));
        }
        if *value == CellValue::Default && !column.has_default {
            return Err(format!("{} has no default", column.name));
        }
        Ok(true)
    }

    /// Saves `value` into a cell, skipping no-op writes.
    fn write_cell(&mut self, row: usize, col: usize, value: CellValue) {
        match self.check_write(row, col, &value) {
            Err(e) => return self.set_status(StatusKind::Error, e),
            Ok(false) => return,
            Ok(true) => {}
        }
        let Some(d) = &mut self.data else { return };
        // Drafts are edited locally until committed.
        if let Some(defaults) = &mut d.rows[row].draft {
            defaults[col] = value == CellValue::Default;
            d.rows[row].values[col] = match value {
                CellValue::Value(v) => Some(v),
                _ => None,
            };
            return;
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

// -------------------------------------------------------------------------
// Mouse
// -------------------------------------------------------------------------

impl App {
    pub fn on_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        match m.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                self.pending_key = None;
                self.on_click(pos);
            }
            MouseEventKind::ScrollDown => self.on_scroll(pos, 3),
            MouseEventKind::ScrollUp => self.on_scroll(pos, -3),
            _ => {}
        }
    }

    fn on_click(&mut self, pos: Position) {
        if !matches!(self.popup, Popup::None) {
            let option = hit(&self.hits.options, pos);
            let inside = self.hits.popup.is_some_and(|r| r.contains(pos));
            match (&mut self.popup, option) {
                (Popup::Editor(ed), Some(i)) => {
                    if let EditorKind::Choice { sel, .. } = &mut ed.kind {
                        *sel = i;
                    }
                    self.on_editor_key(KeyEvent::from(KeyCode::Enter));
                }
                (Popup::Favorites(_), Some(i)) => self.open_favorite(i),
                _ if !inside => self.popup = Popup::None,
                _ => {}
            }
            return;
        }
        if let Some(i) = hit(&self.hits.tables, pos) {
            self.filter_active = false;
            self.filter.set_focused(false);
            self.table_sel = i;
            return self.open_selected_table();
        }
        if let Some((row, col)) = hit(
            &self
                .hits
                .cells
                .iter()
                .map(|&(r, row, col)| (r, (row, col)))
                .collect::<Vec<_>>(),
            pos,
        ) {
            // A click on the already-selected cell edits it.
            let again = self.focus == Focus::Grid && (row, col) == (self.cur_row, self.cur_col);
            self.focus = Focus::Grid;
            self.cur_row = row;
            self.cur_col = col;
            if again {
                self.open_editor(EditStart::End);
            }
            return;
        }
        if self.hits.sidebar.contains(pos) {
            self.focus = Focus::Tables;
        } else if self.hits.grid.contains(pos) {
            self.focus_grid();
        }
    }

    fn on_scroll(&mut self, pos: Position, delta: isize) {
        let step = |v: usize, len: usize| v.saturating_add_signed(delta).min(len.saturating_sub(1));
        match &mut self.popup {
            Popup::Editor(Editor {
                kind: EditorKind::Choice { filtered, sel, .. },
                ..
            }) => *sel = step(*sel, filtered.len()),
            Popup::Favorites(p) => p.sel = step(p.sel, self.favorites.list().len()),
            Popup::Editor(_) | Popup::Confirm(_) => {}
            Popup::None if self.hits.sidebar.contains(pos) => {
                self.table_sel = step(self.table_sel, self.visible_tables.len())
            }
            Popup::None if self.hits.grid.contains(pos) => {
                if let Some(d) = &self.data {
                    self.cur_row = step(self.cur_row, d.rows.len());
                }
            }
            Popup::None => {}
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
