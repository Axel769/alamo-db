//! Canonical M0 dataset — §15.
//!
//! **"1M rows" is not a benchmark.** Six integer columns and fourteen columns
//! with JSON and long text differ by an order of magnitude. Without a fixed
//! schema the numbers mean nothing and cannot be compared between two runs
//! weeks apart.

/// The engine of the bench server: the same tables and the same data in
/// each one's SQL (`launch` measures both).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    /// MySQL 8.x (`INSERT … WITH RECURSIVE`, not in MariaDB).
    MySql,
}

impl Engine {
    pub fn of_url(url: &str) -> Engine {
        if url.starts_with("mysql:") {
            Engine::MySql
        } else {
            Engine::Postgres
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Table {
    /// T1 — narrow (~60 B/row), best case, measures the **per-field** overhead.
    Narrow,
    /// T2 — wide (~500 B/row), realistic case, exercises the fallback paths
    /// (`numeric` -> Utf8, `jsonb` -> Utf8, `text[]` -> Utf8).
    Wide,
    /// T3 — 60 integer columns, to isolate the **per-column** overhead.
    Int60,
}

impl Table {
    pub fn parse(s: &str) -> Option<Table> {
        match s.to_ascii_lowercase().as_str() {
            "t1" | "narrow" => Some(Table::Narrow),
            "t2" | "wide" => Some(Table::Wide),
            "t3" | "int60" => Some(Table::Int60),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Table::Narrow => "bench_narrow",
            Table::Wide => "bench_wide",
            Table::Int60 => "bench_int60",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Table::Narrow => "T1 (narrow)",
            Table::Wide => "T2 (wide)",
            Table::Int60 => "T3 (60 int)",
        }
    }

    pub fn all() -> [Table; 3] {
        [Table::Narrow, Table::Wide, Table::Int60]
    }

    pub fn column_count(&self) -> usize {
        match self {
            Table::Narrow => 6,
            Table::Wide => 14,
            Table::Int60 => 60,
        }
    }

    pub fn ddl(&self, engine: Engine) -> String {
        if engine == Engine::MySql {
            return self.mysql_ddl();
        }
        match self {
            Table::Narrow => "\
CREATE TABLE bench_narrow (
  id         bigint,
  parent_id  bigint,
  qty        integer,
  ratio      double precision,
  is_active  boolean,
  created_at timestamptz
)"
            .to_owned(),
            Table::Wide => "\
CREATE TABLE bench_wide (
  id          bigint,
  parent_id   bigint,
  uid         uuid,
  code        varchar(16),
  name        text,
  description text,
  amount      numeric(14,2),
  ratio       double precision,
  qty         integer,
  is_active   boolean,
  created_at  timestamptz,
  updated_at  timestamptz,
  payload     jsonb,
  tags        text[]
)"
            .to_owned(),
            Table::Int60 => {
                let cols: Vec<String> = (0..60).map(|i| format!("  c{i:02} integer")).collect();
                format!("CREATE TABLE bench_int60 (\n{}\n)", cols.join(",\n"))
            }
        }
    }

    /// The same columns in MySQL types: `uuid` as `char(36)`, `jsonb` and
    /// `text[]` as `json`, `timestamptz` as `timestamp`.
    fn mysql_ddl(&self) -> String {
        match self {
            Table::Narrow => "\
CREATE TABLE bench_narrow (
  id         bigint,
  parent_id  bigint,
  qty        int,
  ratio      double,
  is_active  boolean,
  created_at timestamp NULL
)"
            .to_owned(),
            Table::Wide => "\
CREATE TABLE bench_wide (
  id          bigint,
  parent_id   bigint,
  uid         char(36),
  code        varchar(16),
  name        text,
  description text,
  amount      decimal(14,2),
  ratio       double,
  qty         int,
  is_active   boolean,
  created_at  timestamp NULL,
  updated_at  timestamp NULL,
  payload     json,
  tags        json
)"
            .to_owned(),
            Table::Int60 => {
                let cols: Vec<String> = (0..60).map(|i| format!("  c{i:02} int")).collect();
                format!("CREATE TABLE bench_int60 (\n{}\n)", cols.join(",\n"))
            }
        }
    }

    /// Statements that fill rows `from..=to`. Generation happens on the
    /// server (`generate_series`, a recursive CTE on MySQL): inserting from
    /// the client would make the preparation slower than the measurement it
    /// is meant to enable.
    pub fn fill_sql(&self, engine: Engine, from: u64, to: u64) -> String {
        if engine == Engine::MySql {
            return self.mysql_fill_sql(from, to);
        }
        match self {
            Table::Narrow => format!(
                "INSERT INTO bench_narrow
                 SELECT g,
                        (g % 1000) + 1,
                        (g % 977)::int,
                        (g % 1000) / 7.0,
                        (g % 2 = 0),
                        timestamptz '2020-01-01 00:00:00+00' + ((g % 100000) * interval '1 second')
                 FROM generate_series({from}, {to}) g"
            ),
            // 15% NULLs on `description`, scattered: §9.10 asks for NULLs in
            // columns of different types, not all in the same one.
            Table::Wide => format!(
                "INSERT INTO bench_wide
                 SELECT g,
                        (g % 1000) + 1,
                        md5(g::text)::uuid,
                        left(md5(g::text), 16),
                        left(repeat(md5(g::text), 2), 24),
                        CASE WHEN g % 100 < 15 THEN NULL
                             ELSE left(repeat(md5(g::text), 4), 120) END,
                        ((g % 100000000)::numeric / 100),
                        (g % 1000) / 7.0,
                        (g % 977)::int,
                        (g % 2 = 0),
                        timestamptz '2020-01-01 00:00:00+00' + ((g % 100000) * interval '1 second'),
                        CASE WHEN g % 7 = 0 THEN NULL
                             ELSE timestamptz '2021-01-01 00:00:00+00' + ((g % 90000) * interval '1 second') END,
                        jsonb_build_object(
                          'id', g,
                          'hash', md5(g::text),
                          'nested', jsonb_build_object('a', g % 13, 'b', md5((g+1)::text)),
                          'flags', jsonb_build_array(g % 2 = 0, g % 3 = 0, g % 5 = 0)
                        ),
                        ARRAY[left(md5(g::text), 8), left(md5((g+1)::text), 8), 'tag' || (g % 20)]
                 FROM generate_series({from}, {to}) g"
            ),
            Table::Int60 => {
                let cols: Vec<String> = (0..60).map(|i| format!("(g % {})::int", i + 7)).collect();
                format!(
                    "INSERT INTO bench_int60 SELECT {} FROM generate_series({from}, {to}) g",
                    cols.join(", ")
                )
            }
        }
    }

    /// Same values as the PG fill, row by row (`g` is the row number).
    fn mysql_fill_sql(&self, from: u64, to: u64) -> String {
        let select = match self {
            Table::Narrow => "SELECT g, (g % 1000) + 1, g % 977, (g % 1000) / 7.0, g % 2 = 0,
                       TIMESTAMP '2020-01-01 00:00:00' + INTERVAL (g % 100000) SECOND"
                .to_owned(),
            Table::Wide => "SELECT g, (g % 1000) + 1,
                       INSERT(INSERT(INSERT(INSERT(md5(g), 21, 0, '-'), 17, 0, '-'), 13, 0, '-'), 9, 0, '-'),
                       left(md5(g), 16),
                       left(repeat(md5(g), 2), 24),
                       CASE WHEN g % 100 < 15 THEN NULL ELSE left(repeat(md5(g), 4), 120) END,
                       (g % 100000000) / 100,
                       (g % 1000) / 7.0,
                       g % 977,
                       g % 2 = 0,
                       TIMESTAMP '2020-01-01 00:00:00' + INTERVAL (g % 100000) SECOND,
                       CASE WHEN g % 7 = 0 THEN NULL
                            ELSE TIMESTAMP '2021-01-01 00:00:00' + INTERVAL (g % 90000) SECOND END,
                       JSON_OBJECT('id', g, 'hash', md5(g),
                                   'nested', JSON_OBJECT('a', g % 13, 'b', md5(g + 1)),
                                   'flags', JSON_ARRAY(g % 2 = 0, g % 3 = 0, g % 5 = 0)),
                       JSON_ARRAY(left(md5(g), 8), left(md5(g + 1), 8), CONCAT('tag', g % 20))"
                .to_owned(),
            Table::Int60 => {
                let cols: Vec<String> = (0..60).map(|i| format!("g % {}", i + 7)).collect();
                format!("SELECT {}", cols.join(", "))
            }
        };
        // The CTE recursion limit is 1000 by default: one batch at a time.
        format!(
            "SET SESSION cte_max_recursion_depth = {};
             INSERT INTO {} WITH RECURSIVE seq (g) AS
               (SELECT {from} UNION ALL SELECT g + 1 FROM seq WHERE g < {to})
             {select} FROM seq",
            to - from + 2,
            self.name()
        )
    }

    /// Statistics after the fill: without them the first SELECT also pays
    /// for them and the first repetition turns out artificially slow.
    pub fn analyze_sql(&self, engine: Engine) -> String {
        match engine {
            Engine::Postgres => format!("ANALYZE {}", self.name()),
            Engine::MySql => format!("ANALYZE TABLE {}", self.name()),
        }
    }

    /// The measured query.
    ///
    /// §5.2: **no SQL rewriting** to inject `LIMIT`. It is not even needed
    /// here: limits are applied at the protocol level by stopping the stream
    /// (`FetchOptions::max_rows`).
    pub fn select_sql(&self) -> String {
        format!("SELECT * FROM {}", self.name())
    }

    /// Estimated logical bytes per row. Only there to give an order of
    /// magnitude in the report; the real denominator of the metrics is
    /// `wire_bytes`, which is measured.
    pub fn nominal_row_bytes(&self) -> u64 {
        match self {
            Table::Narrow => 60,
            Table::Wide => 500,
            Table::Int60 => 240,
        }
    }
}
