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
  so Postgres reports anything invalid in the status line. <kbd>Ctrl+N</kbd> writes `NULL`.
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
| Grid | `←↑↓→`/`hjkl` move · `g`/`G` first/last row · `^`/`$` first/last column · `pgup`/`pgdn` · `[`/`]` page · `enter`/`e` edit · `esc` back to tables |
| Dropdown | type to fuzzy find · `↑↓` / `tab` select · `enter` apply · `esc` cancel |
| Text edit | `enter` save · `ctrl+n` NULL · `ctrl+u` clear · `esc` cancel |
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
