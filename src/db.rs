//! Postgres access. Every request runs as a task on the tokio runtime and
//! reports back over a channel, so the UI thread never blocks on the network.

use std::future::Future;
use std::sync::mpsc::Sender;

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sqlx::postgres::PgPool;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TableRef {
    pub schema: String,
    pub name: String,
}

impl TableRef {
    pub fn qualified(&self) -> String {
        format!("{}.{}", quote_ident(&self.schema), quote_ident(&self.name))
    }

    /// `name` for the public schema, `schema.name` otherwise.
    pub fn short(&self) -> String {
        if self.schema == "public" {
            self.name.clone()
        } else {
            self.full()
        }
    }

    pub fn full(&self) -> String {
        format!("{}.{}", self.schema, self.name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelKind {
    Table,
    View,
    MatView,
    Foreign,
}

impl RelKind {
    fn from_relkind(k: &str) -> Self {
        match k {
            "v" => Self::View,
            "m" => Self::MatView,
            "f" => Self::Foreign,
            _ => Self::Table,
        }
    }

    pub fn editable(self) -> bool {
        self == Self::Table
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Table => "",
            Self::View => "view",
            Self::MatView => "matview",
            Self::Foreign => "foreign",
        }
    }
}

#[derive(Clone, Debug)]
pub struct TableInfo {
    pub table: TableRef,
    pub kind: RelKind,
}

#[derive(Clone, Debug)]
pub enum ColKind {
    Enum(Vec<String>),
    Bool,
    Other,
}

#[derive(Clone, Debug)]
pub struct Column {
    pub name: String,
    /// `format_type()` output, usable directly as a cast target.
    pub type_name: String,
    pub nullable: bool,
    pub is_pk: bool,
    pub kind: ColKind,
    /// Has a column default or is an identity column.
    pub has_default: bool,
    /// `GENERATED ALWAYS AS IDENTITY`: explicit values need `OVERRIDING SYSTEM VALUE`.
    pub identity_always: bool,
    /// Stored generated column; never written directly.
    pub generated: bool,
}

impl Column {
    /// Filled in by the database on insert (serial, identity, uuid default, …) when it is a key.
    pub fn auto_key(&self) -> bool {
        self.generated || (self.has_default && self.is_pk) || self.identity_always
    }
}

/// A value to write into a cell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CellValue {
    Value(String),
    Null,
    Default,
}

impl CellValue {
    pub fn label(&self) -> &str {
        match self {
            Self::Value(v) => v,
            Self::Null => "NULL",
            Self::Default => "DEFAULT",
        }
    }

    pub fn from_opt(v: Option<String>) -> Self {
        v.map_or(Self::Null, Self::Value)
    }
}

#[derive(Clone, Debug)]
pub struct DataRow {
    /// Physical row id, used to address rows of tables without a primary key.
    pub ctid: Option<String>,
    pub values: Vec<Option<String>>,
    /// Staged rows that exist only locally until committed. Holds, per column,
    /// whether the cell is left to the database default.
    pub draft: Option<Vec<bool>>,
}

impl DataRow {
    pub fn is_draft(&self) -> bool {
        self.draft.is_some()
    }

    pub fn is_default(&self, col: usize) -> bool {
        self.draft.as_ref().is_some_and(|d| d[col])
    }

    pub fn cell(&self, col: usize) -> CellValue {
        if self.is_default(col) {
            CellValue::Default
        } else {
            CellValue::from_opt(self.values[col].clone())
        }
    }
}

#[derive(Clone, Debug)]
pub struct TableData {
    pub info: TableInfo,
    pub columns: Vec<Column>,
    pub rows: Vec<DataRow>,
    pub total: i64,
    pub page: usize,
    pub page_size: usize,
}

impl TableData {
    pub fn has_pk(&self) -> bool {
        self.columns.iter().any(|c| c.is_pk)
    }

    pub fn draft_count(&self) -> usize {
        self.rows.iter().filter(|r| r.is_draft()).count()
    }
}

pub struct CellUpdate {
    pub info: TableInfo,
    pub columns: Vec<Column>,
    pub row_idx: usize,
    pub row: DataRow,
    pub col: usize,
    pub value: CellValue,
}

pub struct InsertBatch {
    pub info: TableInfo,
    pub columns: Vec<Column>,
    pub rows: Vec<DataRow>,
    /// Per row: upsert over an existing row with the same primary key.
    pub overwrite: Vec<bool>,
}

pub enum Response {
    Tables(Vec<TableInfo>),
    Data {
        seq: u64,
        data: TableData,
    },
    RowUpdated {
        table: TableRef,
        row_idx: usize,
        /// ctid the row had before the update, to find it again if rows shifted.
        old_ctid: Option<String>,
        column: String,
        row: DataRow,
    },
    /// Result of checking drafts against existing keys: (draft index, key description).
    InsertChecked {
        table: TableRef,
        collisions: Vec<(usize, String)>,
    },
    Inserted {
        table: TableRef,
        inserted: usize,
        overwritten: usize,
    },
    Error(String),
}

pub struct Db {
    pool: PgPool,
    rt: tokio::runtime::Handle,
    tx: Sender<Response>,
}

impl Db {
    pub fn new(pool: PgPool, rt: tokio::runtime::Handle, tx: Sender<Response>) -> Self {
        Self { pool, rt, tx }
    }

    fn spawn<F>(&self, context: &'static str, fut: F)
    where
        F: Future<Output = Result<Response>> + Send + 'static,
    {
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let resp = fut
                .await
                .unwrap_or_else(|e| Response::Error(format!("{context}: {}", error_text(&e))));
            let _ = tx.send(resp);
        });
    }

    pub fn load_tables(&self) {
        let pool = self.pool.clone();
        self.spawn("loading tables", async move {
            Ok(Response::Tables(fetch_tables(&pool).await?))
        });
    }

    pub fn load_table(&self, seq: u64, info: TableInfo, page: usize, page_size: usize) {
        let pool = self.pool.clone();
        self.spawn("loading table", async move {
            let data = fetch_table(&pool, info, page, page_size).await?;
            Ok(Response::Data { seq, data })
        });
    }

    pub fn update_cell(&self, upd: CellUpdate) {
        let pool = self.pool.clone();
        self.spawn("update failed", async move { update_cell(&pool, upd).await });
    }

    pub fn check_inserts(&self, info: TableInfo, columns: Vec<Column>, rows: Vec<DataRow>) {
        let pool = self.pool.clone();
        self.spawn("checking keys", async move {
            let collisions = find_collisions(&pool, &info, &columns, &rows).await?;
            Ok(Response::InsertChecked {
                table: info.table,
                collisions,
            })
        });
    }

    pub fn insert_rows(&self, batch: InsertBatch) {
        let pool = self.pool.clone();
        self.spawn("insert failed, nothing was written", async move {
            insert_rows(&pool, batch).await
        });
    }
}

fn error_text(e: &anyhow::Error) -> String {
    match e.downcast_ref::<sqlx::Error>() {
        Some(sqlx::Error::Database(db)) => db.message().to_string(),
        _ => e.to_string(),
    }
}

pub fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

const TABLES_QUERY: &str = "
    select n.nspname::text, c.relname::text, c.relkind::text
    from pg_class c
    join pg_namespace n on n.oid = c.relnamespace
    where c.relkind in ('r', 'p', 'v', 'm', 'f')
      and not c.relispartition
      and n.nspname not in ('pg_catalog', 'information_schema')
      and n.nspname not like 'pg\\_toast%'
      and n.nspname not like 'pg\\_temp%'
    order by n.nspname <> 'public', n.nspname, c.relname";

// Domains are resolved to their base type so a domain over an enum still gets a dropdown.
const COLUMNS_QUERY: &str = "
    select a.attname::text,
           format_type(a.atttypid, a.atttypmod),
           not a.attnotnull,
           coalesce(pk.is_pk, false),
           bt.typname = 'bool',
           case when bt.typtype = 'e' then array(
               select e.enumlabel::text from pg_enum e
               where e.enumtypid = bt.oid order by e.enumsortorder)
           end,
           a.atthasdef or a.attidentity <> '',
           a.attidentity = 'a',
           a.attgenerated <> ''
    from pg_attribute a
    join pg_type t on t.oid = a.atttypid
    join pg_type bt on bt.oid = case when t.typtype = 'd' then t.typbasetype else t.oid end
    left join lateral (
        select true as is_pk from pg_index i
        where i.indrelid = a.attrelid and i.indisprimary and a.attnum = any(i.indkey)
    ) pk on true
    where a.attrelid = format('%I.%I', $1::text, $2::text)::regclass
      and a.attnum > 0 and not a.attisdropped
    order by a.attnum";

async fn fetch_tables(pool: &PgPool) -> Result<Vec<TableInfo>> {
    let rows = sqlx::query(TABLES_QUERY).fetch_all(pool).await?;
    rows.iter()
        .map(|r| {
            Ok(TableInfo {
                table: TableRef {
                    schema: r.try_get(0)?,
                    name: r.try_get(1)?,
                },
                kind: RelKind::from_relkind(&r.try_get::<String, _>(2)?),
            })
        })
        .collect()
}

async fn fetch_columns(pool: &PgPool, t: &TableRef) -> Result<Vec<Column>> {
    let rows = sqlx::query(COLUMNS_QUERY)
        .bind(&t.schema)
        .bind(&t.name)
        .fetch_all(pool)
        .await?;
    rows.iter()
        .map(|r| {
            let is_bool: bool = r.try_get(4)?;
            let labels: Option<Vec<String>> = r.try_get(5)?;
            Ok(Column {
                name: r.try_get(0)?,
                type_name: r.try_get(1)?,
                nullable: r.try_get(2)?,
                is_pk: r.try_get(3)?,
                kind: match (labels, is_bool) {
                    (Some(l), _) => ColKind::Enum(l),
                    (None, true) => ColKind::Bool,
                    _ => ColKind::Other,
                },
                has_default: r.try_get(6)?,
                identity_always: r.try_get(7)?,
                generated: r.try_get(8)?,
            })
        })
        .collect()
}

/// Select list that renders every column as text, prefixed by ctid for editable relations.
/// Outputs are aliased so `order by` still sees the real, typed columns.
fn select_list(columns: &[Column], with_ctid: bool) -> String {
    let mut sel: Vec<String> = Vec::with_capacity(columns.len() + 1);
    if with_ctid {
        sel.push("ctid::text as qb_ctid".into());
    }
    sel.extend(
        columns
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}::text as qb_{i}", quote_ident(&c.name))),
    );
    sel.join(", ")
}

fn decode_row(r: &sqlx::postgres::PgRow, with_ctid: bool, ncols: usize) -> Result<DataRow> {
    let off = usize::from(with_ctid);
    let ctid = if with_ctid { r.try_get(0)? } else { None };
    let values = (0..ncols)
        .map(|i| r.try_get::<Option<String>, _>(i + off))
        .collect::<Result<_, _>>()?;
    Ok(DataRow {
        ctid,
        values,
        draft: None,
    })
}

async fn fetch_table(pool: &PgPool, info: TableInfo, page: usize, page_size: usize) -> Result<TableData> {
    let columns = fetch_columns(pool, &info.table).await?;
    let editable = info.kind.editable();
    let pks: Vec<String> = columns
        .iter()
        .filter(|c| c.is_pk)
        .map(|c| quote_ident(&c.name))
        .collect();
    let order = if !pks.is_empty() {
        format!("order by {}", pks.join(", "))
    } else if editable {
        "order by ctid".into()
    } else {
        String::new()
    };
    let sql = format!(
        "select {} from {} {order} limit {page_size} offset {}",
        select_list(&columns, editable),
        info.table.qualified(),
        page * page_size,
    );
    let count_sql = format!("select count(*) from {}", info.table.qualified());
    let (rows, total) = tokio::try_join!(
        sqlx::query(&sql).fetch_all(pool),
        sqlx::query_scalar::<_, i64>(&count_sql).fetch_one(pool),
    )?;
    let rows = rows
        .iter()
        .map(|r| decode_row(r, editable, columns.len()))
        .collect::<Result<_>>()?;
    Ok(TableData {
        info,
        columns,
        rows,
        total,
        page,
        page_size,
    })
}

/// Writes one cell. Rows are addressed by primary key when there is one and by
/// ctid otherwise; values travel as text and are cast server-side to the column type.
async fn update_cell(pool: &PgPool, u: CellUpdate) -> Result<Response> {
    let col = &u.columns[u.col];
    let mut binds: Vec<Option<String>> = Vec::new();
    let set_expr = match &u.value {
        CellValue::Default => "DEFAULT".to_string(),
        v => {
            binds.push(match v {
                CellValue::Value(s) => Some(s.clone()),
                _ => None,
            });
            format!("$1::{}", col.type_name)
        }
    };
    let pk_idx: Vec<usize> = (0..u.columns.len()).filter(|&i| u.columns[i].is_pk).collect();
    let mut conds = Vec::new();
    if pk_idx.is_empty() {
        let Some(ctid) = u.row.ctid.clone() else {
            bail!(
                "{} has no primary key and no ctid; it cannot be edited",
                u.info.table.full()
            );
        };
        binds.push(Some(ctid));
        conds.push(format!("ctid = ${}::tid", binds.len()));
    } else {
        for i in pk_idx {
            binds.push(u.row.values[i].clone());
            let c = &u.columns[i];
            conds.push(format!("{} = ${}::{}", quote_ident(&c.name), binds.len(), c.type_name));
        }
    }
    let sql = format!(
        "update {} set {} = {set_expr} where {} returning {}",
        u.info.table.qualified(),
        quote_ident(&col.name),
        conds.join(" and "),
        select_list(&u.columns, true),
    );
    let mut q = sqlx::query(&sql);
    for b in binds {
        q = q.bind(b);
    }
    let mut tx = pool.begin().await?;
    let rows = q.fetch_all(&mut *tx).await?;
    if rows.len() != 1 {
        tx.rollback().await?;
        if rows.is_empty() {
            bail!("no row matched (changed or deleted elsewhere?) — press r to refresh");
        }
        bail!("would have touched {} rows; rolled back", rows.len());
    }
    tx.commit().await?;
    let mut row = decode_row(&rows[0], true, u.columns.len())?;
    if u.row.ctid.is_none() {
        row.ctid = None;
    }
    Ok(Response::RowUpdated {
        table: u.info.table,
        row_idx: u.row_idx,
        old_ctid: u.row.ctid,
        column: col.name.clone(),
        row,
    })
}

/// Primary-key condition for a draft, or `None` when part of the key is left to the database.
fn key_condition(columns: &[Column], row: &DataRow, binds: &mut Vec<Option<String>>) -> Option<String> {
    let mut conds = Vec::new();
    for (i, c) in columns.iter().enumerate().filter(|(_, c)| c.is_pk) {
        if row.is_default(i) {
            return None;
        }
        binds.push(row.values[i].clone());
        conds.push(format!("{} = ${}::{}", quote_ident(&c.name), binds.len(), c.type_name));
    }
    (!conds.is_empty()).then(|| conds.join(" and "))
}

fn key_label(columns: &[Column], row: &DataRow) -> String {
    columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_pk)
        .map(|(i, c)| format!("{}={}", c.name, row.values[i].as_deref().unwrap_or("NULL")))
        .collect::<Vec<_>>()
        .join(", ")
}

async fn find_collisions(
    pool: &PgPool,
    info: &TableInfo,
    columns: &[Column],
    rows: &[DataRow],
) -> Result<Vec<(usize, String)>> {
    let mut out = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let mut binds = Vec::new();
        let Some(cond) = key_condition(columns, row, &mut binds) else {
            continue;
        };
        let sql = format!("select exists(select 1 from {} where {cond})", info.table.qualified());
        let mut q = sqlx::query_scalar::<_, bool>(&sql);
        for b in binds {
            q = q.bind(b);
        }
        if q.fetch_one(pool).await? {
            out.push((i, key_label(columns, row)));
        }
    }
    Ok(out)
}

/// Inserts every draft in one transaction; any failure rolls back the whole batch.
async fn insert_rows(pool: &PgPool, b: InsertBatch) -> Result<Response> {
    let pk_list: Vec<String> = b
        .columns
        .iter()
        .filter(|c| c.is_pk)
        .map(|c| quote_ident(&c.name))
        .collect();
    let mut tx = pool.begin().await?;
    let mut overwritten = 0;
    for (row, &overwrite) in b.rows.iter().zip(&b.overwrite) {
        let mut names = Vec::new();
        let mut exprs = Vec::new();
        let mut binds: Vec<Option<String>> = Vec::new();
        let mut overriding = false;
        for (i, c) in b.columns.iter().enumerate() {
            if c.generated || row.is_default(i) {
                continue;
            }
            binds.push(row.values[i].clone());
            names.push(quote_ident(&c.name));
            exprs.push(format!("${}::{}", binds.len(), c.type_name));
            overriding |= c.identity_always;
        }
        let mut sql = if names.is_empty() {
            format!("insert into {} default values", b.info.table.qualified())
        } else {
            format!(
                "insert into {} ({}){} values ({})",
                b.info.table.qualified(),
                names.join(", "),
                if overriding { " overriding system value" } else { "" },
                exprs.join(", "),
            )
        };
        if overwrite && !pk_list.is_empty() {
            let sets: Vec<String> = b
                .columns
                .iter()
                .filter(|c| !c.is_pk && !c.generated)
                .map(|c| format!("{0} = excluded.{0}", quote_ident(&c.name)))
                .collect();
            sql.push_str(&format!(" on conflict ({}) do ", pk_list.join(", ")));
            sql.push_str(&if sets.is_empty() {
                "nothing".to_string()
            } else {
                format!("update set {}", sets.join(", "))
            });
            overwritten += 1;
        }
        let mut q = sqlx::query(&sql);
        for v in binds {
            q = q.bind(v);
        }
        q.execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(Response::Inserted {
        table: b.info.table,
        inserted: b.rows.len() - overwritten,
        overwritten,
    })
}
