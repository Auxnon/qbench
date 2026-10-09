# qbench

A cozy terminal editor for **live Postgres tables**, built with [ratatui](https://ratatui.rs) and
the Charm-flavoured widgets from [ratatui-cheese](https://github.com/shashanktomar/ratatui-cheese).

```
  qbench   ⛁ postgres@localhost:55432/postgres                                        1 users  2 shop.orders

 Tables ─────────────────────────  public.users ─────────────────────────────────────────────────────────────
 / press / to filter                    ◆ id     email                display_name        mood     is_admin
                                        integer  text                 character varying…  mood ▾   boolean ▾
     notes                         ──────────────────────────────────────────────────────────────────────────
 │ ★ users                           1        1  user1@example.com    Grace 1             hungry   true
     shop.big_orders view            2        2  user2@example.com    Linus 2            ╭ mood mood ─────────────────╮
   ★ shop.orders                     3        3  user3@example.com    Ken 3              │› hng                    1/5│
                                     4        4  user4@example.com    Barbara 4          │────────────────────────────│
                                     5        5  user5@example.com    Edsger 5           │▸ hungry                    │
                                     6        6  user6@example.com    Margaret 6         ╰────────── ↑↓ · enter · esc ╯
```

## Features

- **Live cell editing.** Move around the grid and press <kbd>Enter</kbd> on a cell. Changes are written
  straight away, one row per transaction.
- **Dropdowns for enums and booleans.** Enum columns list their labels in declared order (domains over
  enums count too). Boolean columns offer `true` / `false`, and nullable columns also get `NULL`. Type to
  fuzzy-filter the list or use the arrow keys.
- **Typed text for everything else.** The value is sent as text and cast server-side to the column type,
  so Postgres reports anything invalid in the status line. <kbd>Ctrl+N</kbd> writes `NULL` and
  <kbd>Ctrl+D</kbd> writes `DEFAULT`. Dropdowns list `NULL` / `DEFAULT` when the column allows them.
- **Clone rows, then insert in bulk.** <kbd>o</kbd> / <kbd>O</kbd> clones the current row below or above
  as a local draft (`+` in the row-number column). Edit drafts like any other row and clone as many as
  you like; nothing is written yet. Keys the database generates (serial, identity, defaulted primary
  keys, generated columns) are left out and shown as `✱ auto`; type a value to set one yourself.
  <kbd>Ctrl+S</kbd> checks every draft's key against the table and always asks for confirmation,
  warning by name about any draft that would **overwrite** an existing row. All drafts are then written
  in one transaction, so any error rolls back the whole batch and keeps the drafts. <kbd>dd</kbd>
  discards a draft and <kbd>Ctrl+E</kbd> discards all of them. You can't page, reload or switch tables
  while drafts exist, and <kbd>q</kbd> asks before quitting.
- **Delete rows.** <kbd>dd</kbd> on a saved row, or <kbd>v</kbd> to select rows in visual mode then
  <kbd>d</kbd>, opens a confirmation listing how many rows will go and their keys. Deletes run in one
  transaction. Each must match exactly one row (by primary key, or ctid), otherwise nothing is deleted.
  Foreign-key errors are shown and rolled back. In visual mode, <kbd>y</kbd> yanks the selected rows
  as TSV.
- **Vim-style keys:** `hjkl`, `w`/`e`/`b` between columns, `gg`/`G`, `H`/`M`/`L`, `Ctrl+d/u/f/b`, and
  `i`/`a`/`s`/`cc` to edit. <kbd>Y</kbd> / <kbd>yy</kbd> yanks a cell to the system clipboard (wl-copy,
  xclip, xsel or pbcopy, otherwise OSC 52), and <kbd>p</kbd> pastes it into another cell.
- **Mouse:** click a table to open it, click a cell to select it, click it again to edit, click a
  dropdown option to apply it, and Shift+click to extend a visual selection. The wheel scrolls rows;
  a tilt wheel or trackpad side-scroll, or Shift+wheel, moves between columns. Pass `--no-mouse` to keep the terminal's own
  text selection.
- **Compact table list:** while the grid has focus, the table list shrinks to a narrow strip. It expands
  again when you focus it (<kbd>Esc</kbd>, <kbd>Tab</kbd>, <kbd>Ctrl+H</kbd> or a click).
- **Favorites.** <kbd>f</kbd> stars a table. You can have up to 10, and each gets a number key
  (<kbd>1</kbd>–<kbd>9</kbd>, <kbd>0</kbd>). <kbd>F</kbd> opens the favorites list, and
  <kbd>Alt</kbd>+number opens one from anywhere. They are saved per connection in
  `~/.config/qbench/favorites.json`.
- **Fuzzy table filter** (<kbd>/</kbd>) with highlighted matches, over every schema.
- Rows are found by **primary key**, or by `ctid` for tables without one. Views and materialized views
  are read-only.
- Server-side paging (<kbd>[</kbd> / <kbd>]</kbd>) and horizontal scrolling for wide tables.
- Themes from ratatui-cheese: `--theme charm|dark|light|ocean|sunset`.

## Install

```sh
cargo install --git https://github.com/Auxnon/qbench
```

## Usage

```sh
qbench postgres://user:pass@localhost:5432/mydb
# or
DATABASE_URL=postgres://... qbench --theme ocean --page-size 500
```

TLS options in the connection string (`?sslmode=require`) are supported via rustls.

### Try it with the demo database

```sh
docker run -d --name qbench-demo -e POSTGRES_PASSWORD=qbench -p 55432:5432 docker.io/library/postgres:17-alpine
psql postgres://postgres:qbench@localhost:55432/postgres -f examples/seed.sql
cargo run -- postgres://postgres:qbench@localhost:55432/postgres
```

## Keys

| Where | Keys |
| --- | --- |
| Anywhere | `tab` switch pane · `?` full help · `q` / `ctrl+c` quit · `r` refresh |
| Tables | `↑↓`/`jk` move · `enter` open · `/` fuzzy filter · `esc` clear filter |
| Grid motion | `←↑↓→`/`hjkl` · `w`/`e` next column · `b` previous column · `gg`/`G` first/last row · `^`/`$` first/last column · `H`/`M`/`L` top/middle/bottom of screen · `ctrl+d/u` half page · `ctrl+f/b`, `pgup`/`pgdn` full page · `[`/`]` previous/next page |
| Grid editing | `enter`/`a` edit · `i` edit, cursor at start · `s`/`cc` replace value · `Y`/`yy` yank · `p` paste · `o`/`O` clone row · `dd` delete row (confirms) or discard draft · `ctrl+s` commit drafts · `ctrl+e` discard all drafts |
| Visual | `v`/`V` start · any motion extends · `d`/`x` delete selected rows (confirms) · `y` yank rows as TSV · `esc`/`v` exit |
| Panes | `esc`/`ctrl+h` table list · `ctrl+l` grid · `/` filter tables |
| Dropdown | type to fuzzy find · `↑↓`, `tab`, `ctrl+j/k` select · `enter` or click apply · `esc` cancel |
| Text edit | `enter` save · `ctrl+n` NULL · `ctrl+d` DEFAULT · `ctrl+u` clear · `esc` cancel |
| Mouse | click table to open · click cell to select, click again to edit · shift+click extend selection · wheel scrolls rows · side-scroll / shift+wheel scrolls columns |
| Favorites | `f` toggle · `F` list · `1`–`9`, `0` open · `alt+1`–`0` open from anywhere · `d` remove (in list) |

## How it talks to Postgres

The database layer follows [rainfrog](https://github.com/achristmascarl/rainfrog): an `sqlx` pool on a
tokio runtime, with each request running as a background task and replying over a channel, so the UI
never blocks. Tables come from `pg_class`. Column types, enum labels and primary keys come from
`pg_attribute`, `pg_type`, `pg_enum` and `pg_index`. Values are selected as `::text`, so every type
displays, and updates use `UPDATE … SET col = $1::<type> WHERE <pk> = … RETURNING …` to refresh the
row in place.

## License

MIT
