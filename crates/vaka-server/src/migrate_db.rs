//! `vaka-server migrate-db`: copy the metadata database from one backend to another
//! (typically SQLite -> Postgres). Events and crashes live in ClickHouse and are untouched.
//!
//! The copy is re-runnable: the target schema is migrated, every metadata table is emptied
//! and refilled in one transaction from a consistent snapshot of the source, sequences are
//! reset, and row counts are verified per table. See docs/self-hosting.md for the cutover.

use crate::db::{self, Pool};
use sqlx::any::AnyRow;
use sqlx::{Any, Row, Transaction};

pub const USAGE: &str = "\
vaka-server migrate-db --to <url> [--from <url>] [--replace]
    Copy the metadata database (users, projects, issues, tokens, alert rules, ...) into
    another database, typically SQLite -> Postgres. --from defaults to VAKA_DB_URL (or the
    default SQLite file under VAKA_DATA_DIR). The target schema is created if needed.
    --replace   overwrite a target that already has data (needed to re-run the copy)";

#[derive(Debug, Default, Clone)]
pub struct Options {
    pub from: String,
    pub to: String,
    pub replace: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableReport {
    pub table: String,
    /// Rows read from the source snapshot and written to the target.
    pub copied: i64,
    /// Rows in the target after the copy committed.
    pub target: i64,
    /// Rows in the source when the copy finished (differs while the server keeps writing).
    pub source_now: i64,
}

pub async fn run_cli(args: &[String]) -> anyhow::Result<()> {
    let mut o = Options::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) => (f, Some(v.to_string())),
            None => (a.as_str(), None),
        };
        let mut value = || inline.clone().or_else(|| it.next().cloned()).ok_or_else(|| anyhow::anyhow!("{flag} needs a value\n{USAGE}"));
        match flag {
            "--from" => o.from = value()?,
            "--to" => o.to = value()?,
            "--replace" => o.replace = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => anyhow::bail!("unknown option `{other}`\n{USAGE}"),
        }
    }
    if o.to.is_empty() {
        anyhow::bail!("--to is required\n{USAGE}");
    }
    if o.from.is_empty() {
        o.from = crate::config::Config::from_env()?.db_url;
    }
    println!("Copying metadata from {} to {}", redact(&o.from), redact(&o.to));
    let started = std::time::Instant::now();
    let report = migrate(&o).await?;
    let width = report.iter().map(|r| r.table.len()).max().unwrap_or(5).max(5);
    println!("{:width$}  {:>8}  {:>8}  status", "table", "copied", "target");
    let mut drift = 0;
    for r in &report {
        let status = if r.copied != r.target {
            "MISMATCH"
        } else if r.source_now != r.copied {
            drift += 1;
            "ok (source changed since)"
        } else {
            "ok"
        };
        println!("{:width$}  {:>8}  {:>8}  {status}", r.table, r.copied, r.target);
    }
    let total: i64 = report.iter().map(|r| r.copied).sum();
    println!("Copied {total} rows in {} tables in {:.1}s.", report.len(), started.elapsed().as_secs_f64());
    if drift > 0 {
        println!(
            "Note: {drift} table(s) changed in the source while copying. That is expected while the server is writable; \
             for the final copy, turn on read-only mode first (see docs/self-hosting.md)."
        );
    }
    Ok(())
}

fn redact(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut u) if u.password().is_some() => {
            let _ = u.set_password(Some("***"));
            u.to_string()
        }
        _ => url.to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Int,
    Real,
    Text,
    Blob,
}

fn kind_of(declared: &str) -> Kind {
    let t = declared.to_ascii_lowercase();
    if t.contains("int") {
        Kind::Int
    } else if t.contains("real") || t.contains("double") || t.contains("float") || t.contains("numeric") {
        Kind::Real
    } else if t.contains("blob") || t.contains("bytea") {
        Kind::Blob
    } else {
        Kind::Text
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Val {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// Read a column as the target's type, converting what SQLite's loose typing may have stored.
fn read(row: &AnyRow, i: usize, kind: Kind) -> Val {
    let int = || row.try_get::<Option<i64>, _>(i).ok().flatten();
    let int32 = || row.try_get::<Option<i32>, _>(i).ok().flatten().map(i64::from);
    let real = || row.try_get::<Option<f64>, _>(i).ok().flatten();
    let text = || row.try_get::<Option<String>, _>(i).ok().flatten();
    let blob = || row.try_get::<Option<Vec<u8>>, _>(i).ok().flatten();
    let v = match kind {
        Kind::Int => int()
            .or_else(int32)
            .map(Val::Int)
            .or_else(|| real().map(|f| Val::Int(f as i64)))
            .or_else(|| text().and_then(|s| s.trim().parse().ok()).map(Val::Int)),
        Kind::Real => real().or_else(|| int().map(|n| n as f64)).or_else(|| text().and_then(|s| s.parse().ok())).map(Val::Real),
        Kind::Text => text()
            .map(Val::Text)
            .or_else(|| int().or_else(int32).map(|n| Val::Text(n.to_string())))
            .or_else(|| real().map(|f| Val::Text(f.to_string())))
            .or_else(|| blob().map(|b| Val::Text(String::from_utf8_lossy(&b).into_owned()))),
        Kind::Blob => blob().or_else(|| text().map(String::into_bytes)).map(Val::Blob),
    };
    v.unwrap_or(Val::Null)
}

fn is_sqlite(url: &str) -> bool {
    url.starts_with("sqlite:")
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Metadata tables and their columns (name, kind), as the given database has them.
async fn schema(pool: &Pool, sqlite: bool) -> anyhow::Result<Vec<(String, Vec<(String, Kind)>)>> {
    let tables: Vec<String> = if sqlite {
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .fetch_all(pool)
            .await?
    } else {
        sqlx::query_scalar(
            "SELECT table_name::text FROM information_schema.tables WHERE table_schema = current_schema() AND table_type = 'BASE TABLE' ORDER BY table_name",
        )
        .fetch_all(pool)
        .await?
    };
    let mut out = Vec::new();
    for t in tables.into_iter().filter(|t| t != "schema_migrations") {
        let rows = if sqlite {
            sqlx::query("SELECT name, type FROM pragma_table_info($1) ORDER BY cid").bind(&t).fetch_all(pool).await?
        } else {
            sqlx::query(
                "SELECT column_name::text AS name, data_type::text AS type FROM information_schema.columns \
                 WHERE table_schema = current_schema() AND table_name = $1 ORDER BY ordinal_position",
            )
            .bind(&t)
            .fetch_all(pool)
            .await?
        };
        let cols = rows.iter().map(|r| (r.get::<String, _>("name"), kind_of(&r.get::<String, _>("type")))).collect();
        out.push((t, cols));
    }
    Ok(out)
}

async fn schema_version(pool: &Pool) -> anyhow::Result<i64> {
    let v: Option<i64> = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations")
        .fetch_one(pool)
        .await
        .map_err(|e| anyhow::anyhow!("source has no Vaka schema ({e}); is --from the right database?"))?;
    Ok(v.unwrap_or(0))
}

async fn count(pool: &Pool, table: &str) -> anyhow::Result<i64> {
    Ok(sqlx::query_scalar::<_, i64>(&format!("SELECT COUNT(*) FROM {}", quote(table))).fetch_one(pool).await?)
}

/// Rows per INSERT statement: stay well under SQLite's 32766 and Postgres' 65535 bind limits.
fn rows_per_insert(columns: usize) -> usize {
    (20_000 / columns.max(1)).clamp(1, 500)
}

async fn insert_chunk(tx: &mut Transaction<'_, Any>, table: &str, cols: &[(String, Kind)], rows: &[Vec<Val>]) -> anyhow::Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let names = cols.iter().map(|(c, _)| quote(c)).collect::<Vec<_>>().join(", ");
    let mut sql = format!("INSERT INTO {} ({names}) VALUES ", quote(table));
    let mut n = 0;
    for (ri, _) in rows.iter().enumerate() {
        if ri > 0 {
            sql.push_str(", ");
        }
        sql.push('(');
        for ci in 0..cols.len() {
            if ci > 0 {
                sql.push_str(", ");
            }
            n += 1;
            sql.push_str(&format!("${n}"));
        }
        sql.push(')');
    }
    let mut q = sqlx::query(&sql);
    for row in rows {
        for (v, (_, kind)) in row.iter().zip(cols) {
            q = match (v, kind) {
                (Val::Int(x), _) => q.bind(*x),
                (Val::Real(x), _) => q.bind(*x),
                (Val::Text(x), _) => q.bind(x.clone()),
                (Val::Blob(x), _) => q.bind(x.clone()),
                (Val::Null, Kind::Int) => q.bind(None::<i64>),
                (Val::Null, Kind::Real) => q.bind(None::<f64>),
                (Val::Null, Kind::Text) => q.bind(None::<String>),
                (Val::Null, Kind::Blob) => q.bind(None::<Vec<u8>>),
            };
        }
    }
    q.execute(&mut **tx).await?;
    Ok(())
}

/// Point every Postgres sequence (serial / identity columns) past the copied rows.
async fn reset_sequences(tx: &mut Transaction<'_, Any>) -> anyhow::Result<()> {
    let rows = sqlx::query(
        "SELECT table_name::text AS t, column_name::text AS c, pg_get_serial_sequence(quote_ident(table_name), column_name)::text AS s \
         FROM information_schema.columns WHERE table_schema = current_schema() \
         AND (column_default LIKE 'nextval(%' OR is_identity = 'YES')",
    )
    .fetch_all(&mut **tx)
    .await?;
    for r in rows {
        let (t, c, s): (String, String, Option<String>) = (r.get("t"), r.get("c"), r.get("s"));
        let Some(s) = s else { continue };
        sqlx::query(&format!("SELECT setval($1, COALESCE((SELECT MAX({}) FROM {}), 0) + 1, false)", quote(&c), quote(&t)))
            .bind(s)
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub async fn migrate(o: &Options) -> anyhow::Result<Vec<TableReport>> {
    if o.from.trim() == o.to.trim() {
        anyhow::bail!("--from and --to are the same database");
    }
    if let Some(path) = o.from.strip_prefix("sqlite://") {
        let file = path.split('?').next().unwrap_or(path);
        if !std::path::Path::new(file).exists() {
            anyhow::bail!("source database {file} does not exist");
        }
    }
    let (src_sqlite, dst_sqlite) = (is_sqlite(&o.from), is_sqlite(&o.to));
    let src = db::open(&o.from).await?;
    let version = schema_version(&src).await?;
    if version != db::latest_version() {
        anyhow::bail!(
            "source schema is at version {version}, this vaka-server expects {}: run the same Vaka version as the server",
            db::latest_version()
        );
    }
    let dst = db::connect(&o.to).await?;

    let src_schema = schema(&src, src_sqlite).await?;
    let dst_schema = schema(&dst, dst_sqlite).await?;
    for (t, cols) in &dst_schema {
        let Some((_, scols)) = src_schema.iter().find(|(s, _)| s == t) else {
            anyhow::bail!("source has no table `{t}`");
        };
        if let Some((c, _)) = cols.iter().find(|(c, _)| !scols.iter().any(|(s, _)| s == c)) {
            anyhow::bail!("source table `{t}` has no column `{c}`");
        }
    }

    if !o.replace {
        let mut existing = 0;
        for (t, _) in &dst_schema {
            existing += count(&dst, t).await?;
        }
        if existing > 0 {
            anyhow::bail!("the target already has {existing} rows of metadata; pass --replace to overwrite them with the source");
        }
    }

    // One consistent snapshot of the source. On SQLite the first read starts it (WAL mode
    // keeps the server writing meanwhile); on Postgres ask for it explicitly.
    let mut stx = src.begin().await?;
    if !src_sqlite {
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY").execute(&mut *stx).await?;
    }
    let mut ttx = dst.begin().await?;
    if dst_sqlite {
        for (t, _) in &dst_schema {
            sqlx::query(&format!("DELETE FROM {}", quote(t))).execute(&mut *ttx).await?;
        }
    } else if !dst_schema.is_empty() {
        let all = dst_schema.iter().map(|(t, _)| quote(t)).collect::<Vec<_>>().join(", ");
        sqlx::query(&format!("TRUNCATE TABLE {all}")).execute(&mut *ttx).await?;
    }

    let mut copied = Vec::new();
    for (t, cols) in &dst_schema {
        let names = cols.iter().map(|(c, _)| quote(c)).collect::<Vec<_>>().join(", ");
        let rows = sqlx::query(&format!("SELECT {names} FROM {}", quote(t))).fetch_all(&mut *stx).await?;
        let per = rows_per_insert(cols.len());
        let mut n = 0i64;
        for chunk in rows.chunks(per) {
            let vals: Vec<Vec<Val>> = chunk.iter().map(|r| cols.iter().enumerate().map(|(i, (_, k))| read(r, i, *k)).collect()).collect();
            insert_chunk(&mut ttx, t, cols, &vals).await.map_err(|e| anyhow::anyhow!("copying {t}: {e:#}"))?;
            n += vals.len() as i64;
        }
        copied.push((t.clone(), n));
    }
    if !dst_sqlite {
        reset_sequences(&mut ttx).await?;
    }
    ttx.commit().await?;
    stx.rollback().await.ok();

    let mut report = Vec::new();
    for (t, n) in copied {
        report.push(TableReport { target: count(&dst, &t).await?, source_now: count(&src, &t).await?, table: t, copied: n });
    }
    src.close().await;
    dst.close().await;
    if let Some(bad) = report.iter().find(|r| r.copied != r.target) {
        anyhow::bail!("row count mismatch in `{}`: copied {}, target has {}", bad.table, bad.copied, bad.target);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_sqlite() -> (std::path::PathBuf, String) {
        let dir = std::env::temp_dir().join(format!("vaka-migrate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite://{}/vaka.db?mode=rwc", dir.display());
        (dir, url)
    }

    /// Fill every metadata table with `n` rows of distinct values, so the test covers
    /// tables added by future migrations without changes.
    async fn seed(pool: &Pool, n: usize, offset: usize) {
        for (t, cols) in schema(pool, true).await.unwrap() {
            let nullable: Vec<String> = sqlx::query_scalar("SELECT name FROM pragma_table_info($1) WHERE \"notnull\" = 0 AND pk = 0")
                .bind(&t)
                .fetch_all(pool)
                .await
                .unwrap();
            let rows: Vec<Vec<Val>> = (offset..offset + n)
                .map(|i| {
                    cols.iter()
                        .enumerate()
                        .map(|(ci, (c, k))| match k {
                            // NULL in nullable columns on odd rows.
                            _ if i % 2 == 1 && nullable.contains(c) => Val::Null,
                            Kind::Int => Val::Int((i * 1000 + ci) as i64),
                            Kind::Real => Val::Real(i as f64 + 0.5),
                            Kind::Blob => Val::Blob(vec![i as u8, 0, 255]),
                            Kind::Text => Val::Text(format!("{t}.{c}.{i} 'quoted' \"double\" ✓")),
                        })
                        .collect()
                })
                .collect();
            let mut tx = pool.begin().await.unwrap();
            insert_chunk(&mut tx, &t, &cols, &rows).await.unwrap_or_else(|e| panic!("seed {t}: {e:#}"));
            tx.commit().await.unwrap();
        }
    }

    /// Every table's rows, as comparable text, sorted.
    async fn dump(pool: &Pool, sqlite: bool) -> Vec<(String, Vec<String>)> {
        let mut out = Vec::new();
        for (t, cols) in schema(pool, sqlite).await.unwrap() {
            let names = cols.iter().map(|(c, _)| quote(c)).collect::<Vec<_>>().join(", ");
            let rows = sqlx::query(&format!("SELECT {names} FROM {}", quote(&t))).fetch_all(pool).await.unwrap();
            let mut lines: Vec<String> = rows
                .iter()
                .map(|r| format!("{:?}", cols.iter().enumerate().map(|(i, (_, k))| read(r, i, *k)).collect::<Vec<_>>()))
                .collect();
            lines.sort();
            out.push((t, lines));
        }
        out
    }

    /// Callers hold `readonly::TEST_LOCK`: the read-only test flips process-wide state.
    async fn round_trip(target: &str) {
        let (dir, from) = temp_sqlite();
        let src = db::connect(&from).await.unwrap();
        seed(&src, 3, 0).await;
        let o = Options { from: from.clone(), to: target.to_string(), replace: false };

        let report = migrate(&o).await.unwrap();
        assert!(report.len() >= 20, "all metadata tables are copied: {report:?}");
        assert!(report.iter().all(|r| r.copied == 3 && r.target == 3 && r.source_now == 3), "{report:?}");
        let dst = db::open(target).await.unwrap();
        assert_eq!(dump(&src, true).await, dump(&dst, is_sqlite(target)).await);

        // A second copy needs --replace, and then mirrors the source again.
        let err = migrate(&o).await.unwrap_err().to_string();
        assert!(err.contains("--replace"), "{err}");
        sqlx::query("DELETE FROM users WHERE email LIKE '%.0 %'").execute(&src).await.unwrap();
        seed(&src, 2, 10).await;
        let report = migrate(&Options { replace: true, ..o.clone() }).await.unwrap();
        let users = report.iter().find(|r| r.table == "users").unwrap();
        assert_eq!((users.copied, users.target), (4, 4));
        assert_eq!(dump(&src, true).await, dump(&dst, is_sqlite(target)).await);

        // The server can start on the copy: migrations are already applied there.
        db::connect(target).await.unwrap();
        src.close().await;
        dst.close().await;
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn sqlite_to_sqlite() {
        let _serial = crate::readonly::TEST_LOCK.lock().await;
        let (dir, to) = temp_sqlite();
        round_trip(&to).await;
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Runs when VAKA_TEST_POSTGRES_URL points at a Postgres server (CI sets it); a fresh
    /// database is created for the test and dropped afterwards.
    #[tokio::test]
    async fn sqlite_to_postgres() {
        let Ok(base) = std::env::var("VAKA_TEST_POSTGRES_URL") else {
            eprintln!("skipping: VAKA_TEST_POSTGRES_URL is not set");
            return;
        };
        let _serial = crate::readonly::TEST_LOCK.lock().await;
        let admin = db::open(&base).await.unwrap();
        let name = format!("vaka_migrate_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE DATABASE {name}")).execute(&admin).await.unwrap();
        let mut url = url::Url::parse(&base).unwrap();
        url.set_path(&name);
        round_trip(url.as_str()).await;
        sqlx::query(&format!("DROP DATABASE {name} WITH (FORCE)")).execute(&admin).await.unwrap();
    }

    #[tokio::test]
    async fn refuses_missing_source_and_same_database() {
        let (dir, url) = temp_sqlite();
        let err = migrate(&Options { from: url.clone(), to: "sqlite:///tmp/x.db".into(), replace: false }).await.unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
        assert!(!dir.join("vaka.db").exists(), "must not create an empty source");
        let err = migrate(&Options { from: url.clone(), to: url, replace: true }).await.unwrap_err();
        assert!(err.to_string().contains("same database"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn redacts_passwords_and_maps_types() {
        assert_eq!(redact("postgres://vaka:secret@db:5432/vaka"), "postgres://vaka:***@db:5432/vaka");
        assert_eq!(redact("sqlite:///data/vaka.db"), "sqlite:///data/vaka.db");
        assert_eq!(kind_of("BIGINT"), Kind::Int);
        assert_eq!(kind_of("integer"), Kind::Int);
        assert_eq!(kind_of("text"), Kind::Text);
        assert_eq!(kind_of("double precision"), Kind::Real);
        assert_eq!(kind_of("bytea"), Kind::Blob);
    }
}
