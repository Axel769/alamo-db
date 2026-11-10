//! Registry: result sets indexed by id and state machine of the
//! executions (§7.2, §8).
//!
//! The complete result set stays here, in Rust; the frontend asks only for
//! the visible window.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use alamo_core::error::DbError;
use alamo_core::meta::{ColumnMeta, StatsAccumulator};
use alamo_core::result_set::{ResultSet, ResultSetBuilder, ResultSetState};
use alamo_core::window::{Window, WINDOW_CELL_TRUNCATE};
use alamo_driver_postgres::{StopReason, StreamControl};
use tokio::sync::mpsc;

use crate::driver::Canceller;

/// A result set readable **during** streaming.
///
/// Windows are ~200 rows and the append is per chunk: contention on the lock
/// is negligible, and it keeps `fetch_window` out of the async path.
#[derive(Clone)]
pub struct LiveResult {
    inner: Arc<RwLock<LiveInner>>,
    /// Local sort view (§4.8, decision 10): a permutation, never a copy of
    /// the data. When present, window and inspection read in the view's
    /// space.
    sort: Arc<RwLock<Option<SortView>>>,
    /// Rows re-read from the database after a save from the grid: complete
    /// values per PHYSICAL row (`None` = NULL) that override the loaded ones
    /// in window, full cell and export — the row updates without reloading
    /// the table. The local sort stays on the original values.
    patches: Arc<RwLock<RowPatches>>,
    /// Rows deleted from the grid and already saved: they stay in the chunks
    /// (no copies) but leave the view — window, count, sort, export.
    /// PHYSICAL rows, sorted.
    hidden: Arc<RwLock<Vec<u64>>>,
    /// PHYSICAL rows appended by the save (rows inserted from the grid), not
    /// by a page: they do not count in the OFFSET of the next page, the
    /// server has them elsewhere in its ordering.
    local: Arc<RwLock<Vec<u64>>>,
    /// View without local sort when it is not the physical order: rows
    /// inserted from the grid stay where the user added them instead of
    /// going to the bottom (`place_rows`). PHYSICAL rows in view order,
    /// hidden ones left out. `None` = physical order minus `hidden`, no copy.
    order: Arc<RwLock<Option<Vec<u32>>>>,
    /// Rows of a later page already on screen (`hide_duplicates`): out of
    /// the view like the deleted ones, but the server still has them, so
    /// they count in the OFFSET. PHYSICAL rows, sorted.
    dups: Arc<RwLock<Vec<u64>>>,
    /// Key (PK) columns of the rows in `local`, by position: they find the
    /// copies a later page brings again.
    key_cols: Arc<RwLock<Vec<usize>>>,
}

/// View row → physical row skipping the hidden ones (sorted).
fn unhide(hidden: &[u64], row: u64) -> u64 {
    let mut phys = row;
    for &h in hidden {
        if h <= phys {
            phys += 1;
        } else {
            break;
        }
    }
    phys
}

/// Physical row → complete re-read values (`None` = NULL).
type RowPatches = HashMap<u64, Arc<[Option<String>]>>;

struct SortView {
    perm: Vec<u32>,
}

/// Outcome of `sort`/`clear_sort` for the UI: how many rows the view covers
/// and whether the result set is incomplete (§7.3: the badge "only the N
/// downloaded rows sorted" comes from here).
pub struct SortInfo {
    pub rows: u64,
    pub incomplete: bool,
}

enum LiveInner {
    Building(ResultSetBuilder),
    Done(ResultSet),
}

impl LiveResult {
    pub fn new(schema: Arc<[ColumnMeta]>) -> Self {
        let mut b = ResultSetBuilder::new(schema);
        b.set_state(ResultSetState::Streaming);
        LiveResult {
            inner: Arc::new(RwLock::new(LiveInner::Building(b))),
            sort: Arc::new(RwLock::new(None)),
            patches: Arc::new(RwLock::new(HashMap::new())),
            hidden: Arc::new(RwLock::new(Vec::new())),
            local: Arc::new(RwLock::new(Vec::new())),
            order: Arc::new(RwLock::new(None)),
            dups: Arc::new(RwLock::new(Vec::new())),
            key_cols: Arc::new(RwLock::new(Vec::new())),
        }
    }

    fn with_result<T>(&self, f: impl FnOnce(&ResultSet) -> T) -> T {
        match &*self.inner.read().unwrap() {
            LiveInner::Building(b) => f(b.result()),
            LiveInner::Done(r) => f(r),
        }
    }

    /// Sorts by column and installs the permutation. On an open stream it
    /// covers the rows downloaded so far: whoever displays must recompute
    /// when the count grows (the window clamps in the meantime, §4.8).
    pub fn sort(&self, col: usize, desc: bool) -> SortInfo {
        let (mut perm, incomplete) =
            self.with_result(|r| (r.sort_permutation(col, desc), r.state().is_incomplete()));
        {
            let hidden = self.hidden.read().unwrap();
            if !hidden.is_empty() {
                perm.retain(|&p| hidden.binary_search(&(p as u64)).is_err());
            }
        }
        let rows = perm.len() as u64;
        *self.sort.write().unwrap() = Some(SortView { perm });
        SortInfo { rows, incomplete }
    }

    pub fn clear_sort(&self) -> SortInfo {
        *self.sort.write().unwrap() = None;
        let incomplete = self.with_result(|r| r.state().is_incomplete());
        SortInfo {
            rows: self.rows(),
            incomplete,
        }
    }

    pub fn window(&self, rows: std::ops::Range<u64>, cols: std::ops::Range<usize>) -> Window {
        let sort = self.sort.read().unwrap();
        let order = self.order.read().unwrap();
        let hidden = self.hidden.read().unwrap();
        let visible = self.rows_with(&hidden);
        let mut w = self.with_result(|r| match (&*sort, &*order) {
            (Some(v), _) => r.window_permuted(rows.clone(), cols.clone(), &v.perm),
            (None, Some(o)) => r.window_permuted(rows.clone(), cols.clone(), o),
            (None, None) if hidden.is_empty() => r.window(rows.clone(), cols.clone()),
            (None, None) => {
                let end = rows.end.min(visible);
                let start = rows.start.min(end);
                r.window_mapped(start..end, cols.clone(), |v| unhide(&hidden, v))
            }
        });
        drop(hidden);
        let patches = self.patches.read().unwrap();
        if patches.is_empty() {
            return w;
        }
        let hidden = self.hidden.read().unwrap();
        for (i, row) in w.rows.clone().enumerate() {
            let Some(p) = Self::phys(&sort, &order, &hidden, row).and_then(|ph| patches.get(&ph))
            else {
                continue;
            };
            for c in &mut w.columns {
                let Some(v) = p.get(c.index) else { continue };
                // Same rendering as the original window: NULL aside, values
                // beyond the threshold truncated (the full one comes from `cell_full`).
                match v {
                    None => {
                        c.values[i] = String::new();
                        c.nulls[i] = true;
                        c.truncated[i] = false;
                    }
                    Some(s) => {
                        let cut = floor_char_boundary(s, WINDOW_CELL_TRUNCATE);
                        c.values[i] = s[..cut].to_owned();
                        c.nulls[i] = false;
                        c.truncated[i] = cut < s.len();
                    }
                }
            }
        }
        w
    }

    /// View row → physical row (with the local sort, via permutation;
    /// without, through `order` or skipping the hidden rows).
    fn phys(
        sort: &Option<SortView>,
        order: &Option<Vec<u32>>,
        hidden: &[u64],
        row: u64,
    ) -> Option<u64> {
        let perm = match (sort, order) {
            (Some(v), _) => &v.perm,
            (None, Some(o)) => o,
            (None, None) => return Some(unhide(hidden, row)),
        };
        perm.get(usize::try_from(row).ok()?).map(|&p| p as u64)
    }

    /// Current value of a physical cell: the re-read row if there is one,
    /// otherwise the loaded one. `None` = NULL or out of range.
    fn cell_phys(&self, r: &ResultSet, phys: u64, col: usize) -> Option<String> {
        match self.patches.read().unwrap().get(&phys) {
            Some(p) => p.get(col).cloned().flatten(),
            None => r.cell_full(phys, col),
        }
    }

    /// Physical rows with those values in the columns `cols` (the PK key),
    /// one per key looked up; `None` = not found. Linear scan: the loaded
    /// rows are a few thousand, the keys those of one save.
    pub fn find_rows(&self, cols: &[usize], keys: &[Vec<Option<String>>]) -> Vec<Option<u64>> {
        let mut want: HashMap<&[Option<String>], usize> = HashMap::new();
        for (i, k) in keys.iter().enumerate() {
            want.entry(k.as_slice()).or_insert(i);
        }
        let mut out = vec![None; keys.len()];
        // Copy before reading the chunks: locks never nested in the reverse
        // order of `window` (hidden before inner).
        let hidden = self.hidden.read().unwrap().clone();
        self.with_result(|r| {
            let mut found = 0;
            for phys in 0..r.rows() {
                if hidden.binary_search(&phys).is_ok() {
                    continue;
                }
                let key: Vec<Option<String>> =
                    cols.iter().map(|&c| self.cell_phys(r, phys, c)).collect();
                if let Some(&i) = want.get(key.as_slice()) {
                    if out[i].is_none() {
                        out[i] = Some(phys);
                        found += 1;
                        if found == want.len() {
                            break;
                        }
                    }
                }
            }
        });
        // Duplicate keys in the request: same row.
        for (i, k) in keys.iter().enumerate() {
            if out[i].is_none() {
                if let Some(&j) = want.get(k.as_slice()) {
                    out[i] = out[j];
                }
            }
        }
        out
    }

    /// Current values of a physical row, patches included.
    pub fn row_values(&self, phys: u64) -> Vec<Option<String>> {
        let n = self.schema().len();
        self.with_result(|r| (0..n).map(|c| self.cell_phys(r, phys, c)).collect())
    }

    /// Replaces the values of a physical row with the re-read ones.
    pub fn patch_row(&self, phys: u64, values: Vec<Option<String>>) {
        self.patches
            .write()
            .unwrap()
            .insert(phys, Arc::from(values));
    }

    /// Schema of the result set (column names for export/copy, §M4 decision 11).
    pub fn schema(&self) -> Arc<[alamo_core::meta::ColumnMeta]> {
        self.with_result(|r| r.schema_arc())
    }

    pub fn cell_full(&self, row: u64, col: usize) -> Option<String> {
        // With the view active the row arrives in the view's space.
        let phys = Self::phys(
            &self.sort.read().unwrap(),
            &self.order.read().unwrap(),
            &self.hidden.read().unwrap(),
            row,
        )?;
        self.with_result(|r| self.cell_phys(r, phys, col))
    }

    /// Rows of the view: loaded minus those deleted from the grid.
    pub fn rows(&self) -> u64 {
        if let Some(o) = &*self.order.read().unwrap() {
            return o.len() as u64;
        }
        self.rows_with(&self.hidden.read().unwrap())
    }

    fn rows_with(&self, hidden: &[u64]) -> u64 {
        self.phys_rows() - hidden.len() as u64
    }

    /// Physical rows in the chunks, hidden ones included.
    pub fn phys_rows(&self) -> u64 {
        match &*self.inner.read().unwrap() {
            LiveInner::Building(b) => b.rows(),
            LiveInner::Done(r) => r.rows(),
        }
    }

    /// OFFSET of the table tab's next page: the rows the server has already
    /// sent and still has. Excluded: those appended by the save and the
    /// deleted ones (those that came from a page). The hidden copies
    /// (`dups`) count: the server sent them and still has them.
    pub fn server_offset(&self) -> u64 {
        let hidden = self.hidden.read().unwrap();
        let local = self.local.read().unwrap();
        let dups = self.dups.read().unwrap();
        let hidden_server = hidden
            .iter()
            .filter(|h| local.binary_search(h).is_err() && dups.binary_search(h).is_err())
            .count();
        self.phys_rows() - local.len() as u64 - hidden_server as u64
    }

    /// Removes physical rows from the view (deleted and saved). An active
    /// local sort loses them right away, without recomputing.
    pub fn hide_rows(&self, phys: &[u64]) {
        // Lock order as in `window`: sort, order, then hidden.
        let mut sort = self.sort.write().unwrap();
        let mut order = self.order.write().unwrap();
        let mut hidden = self.hidden.write().unwrap();
        hidden.extend_from_slice(phys);
        hidden.sort_unstable();
        hidden.dedup();
        for perm in sort.iter_mut().map(|v| &mut v.perm).chain(order.iter_mut()) {
            perm.retain(|&p| hidden.binary_search(&(p as u64)).is_err());
        }
    }

    /// Rows just appended by the save (`phys`, in INSERT order) moved to the
    /// view rows `at` — where the grid showed them while unsaved — instead
    /// of the bottom. `at` counts the other inserted rows too, as the grid
    /// did: placed in increasing order, each insertion leaves the earlier
    /// ones where they are. The first time it builds `order`.
    pub(crate) fn place_rows(&self, phys: std::ops::Range<u64>, at: &[u64]) {
        let mut order = self.order.write().unwrap();
        let hidden = self.hidden.read().unwrap();
        let o = order.get_or_insert_with(|| {
            (0..phys.start)
                .filter(|p| hidden.binary_search(p).is_err())
                .map(|p| p as u32)
                .collect()
        });
        o.retain(|&p| !phys.contains(&(p as u64)));
        let mut placed: Vec<(u64, u64)> = at.iter().copied().zip(phys).collect();
        placed.sort_unstable();
        for (at, p) in placed {
            let i = usize::try_from(at).unwrap_or(usize::MAX).min(o.len());
            o.insert(i, p as u32);
        }
    }

    /// Appends rows inserted from the grid (see `local`), with the
    /// positions of the key columns. `false` = the stream is still open,
    /// nothing is appended.
    pub(crate) fn append_local(&self, chunk: alamo_core::chunk::Chunk, key_cols: &[usize]) -> bool {
        let before = self.phys_rows();
        let n = chunk.rows() as u64;
        if !self.append_chunk(chunk) {
            return false;
        }
        self.local.write().unwrap().extend(before..before + n);
        *self.key_cols.write().unwrap() = key_cols.to_vec();
        true
    }

    /// Key of a physical row (current values, re-read ones included).
    fn key_phys(&self, r: &ResultSet, phys: u64, cols: &[usize]) -> Vec<Option<String>> {
        cols.iter().map(|&c| self.cell_phys(r, phys, c)).collect()
    }

    /// A page just appended (`new`, physical rows) after rows were inserted
    /// from the grid: the rows it brings that are already on screen leave
    /// the view — the inserted row itself, which the server sorts further
    /// on, or the last of the previous page, pushed down by an insert that
    /// sorts before it. Found by key; nothing to do without inserted rows.
    pub(crate) fn hide_duplicates(&self, new: std::ops::Range<u64>) {
        let cols = self.key_cols.read().unwrap().clone();
        if cols.is_empty() || new.is_empty() || self.local.read().unwrap().is_empty() {
            return;
        }
        let hidden = self.hidden.read().unwrap().clone();
        let found: Vec<u64> = self.with_result(|r| {
            let seen: HashSet<Vec<Option<String>>> = (0..new.start)
                .filter(|p| hidden.binary_search(p).is_err())
                .map(|p| self.key_phys(r, p, &cols))
                .collect();
            new.filter(|&p| seen.contains(&self.key_phys(r, p, &cols)))
                .collect()
        });
        if found.is_empty() {
            return;
        }
        {
            let mut dups = self.dups.write().unwrap();
            dups.extend_from_slice(&found);
            dups.sort_unstable();
        }
        self.hide_rows(&found);
    }

    /// Rows deleted from the grid and saved, found by key: out of the view.
    /// An inserted row whose page copy was hidden takes the copy out of
    /// `dups`: the server no longer has it, the OFFSET stops counting it.
    /// Returns how many were found.
    pub fn remove_rows(&self, cols: &[usize], keys: &[Vec<Option<String>>]) -> u32 {
        let found: Vec<u64> = self.find_rows(cols, keys).into_iter().flatten().collect();
        let gone: HashSet<Vec<Option<String>>> = {
            let local = self.local.read().unwrap();
            self.with_result(|r| {
                found
                    .iter()
                    .filter(|p| local.binary_search(p).is_ok())
                    .map(|&p| self.key_phys(r, p, cols))
                    .collect()
            })
        };
        if !gone.is_empty() {
            let mut dups = self.dups.write().unwrap();
            let kept: Vec<u64> = self.with_result(|r| {
                dups.iter()
                    .copied()
                    .filter(|&d| !gone.contains(&self.key_phys(r, d, cols)))
                    .collect()
            });
            *dups = kept;
        }
        self.hide_rows(&found);
        found.len() as u32
    }

    pub fn state(&self) -> ResultSetState {
        match &*self.inner.read().unwrap() {
            LiveInner::Building(b) => b.result().state(),
            LiveInner::Done(r) => r.state(),
        }
    }

    pub(crate) fn push_chunk(
        &self,
        chunk: alamo_core::chunk::Chunk,
        stats: Vec<StatsAccumulator>,
        wire_bytes_delta: u64,
    ) {
        if let LiveInner::Building(b) = &mut *self.inner.write().unwrap() {
            b.push_chunk(chunk, stats);
            b.add_wire_bytes(wire_bytes_delta);
        }
    }

    /// Appends a chunk to an ALREADY closed result set: the next pages of the
    /// table tab (`Session::fetch_table_more`). The column statistics stay
    /// those of the first page (they feed the widths).
    /// `false` = the stream is still open, nothing is appended.
    pub(crate) fn append_chunk(&self, chunk: alamo_core::chunk::Chunk) -> bool {
        // Lock order as in `window`: order before the chunks.
        let mut order = self.order.write().unwrap();
        match &mut *self.inner.write().unwrap() {
            LiveInner::Building(_) => false,
            LiveInner::Done(r) => {
                let before = r.rows();
                r.push_chunk(chunk);
                if let Some(o) = &mut *order {
                    o.extend(before as u32..r.rows() as u32);
                }
                true
            }
        }
    }

    pub(crate) fn set_state(&self, state: ResultSetState) {
        if let LiveInner::Building(b) = &mut *self.inner.write().unwrap() {
            b.set_state(state);
        }
    }

    /// Closes the construction. Partial data is kept and the state marks it
    /// as incomplete (§7.3): sort/filter/export will have to look at it.
    pub(crate) fn finish(&self, state: ResultSetState, error: Option<DbError>) {
        let mut guard = self.inner.write().unwrap();
        if let LiveInner::Building(b) = &mut *guard {
            let builder = std::mem::replace(b, ResultSetBuilder::new(Arc::from([])));
            let done = match error {
                Some(e) => builder.finish_with_error(state, e),
                None => builder.finish(state),
            };
            *guard = LiveInner::Done(done);
        }
    }
}

fn floor_char_boundary(s: &str, mut index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    while index > 0 && !s.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// §7.2: `Pending → Running → (Completed | Failed | Cancelling → Cancelled)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecState {
    Pending,
    Running,
    Cancelling,
    Cancelled,
    Completed,
    Failed,
}

impl ExecState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            ExecState::Cancelled | ExecState::Completed | ExecState::Failed
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ExecState::Pending => "pending",
            ExecState::Running => "running",
            ExecState::Cancelling => "cancelling",
            ExecState::Cancelled => "cancelled",
            ExecState::Completed => "completed",
            ExecState::Failed => "failed",
        }
    }
}

/// On which connection an execution runs (§3.1): the editor's sticky one or
/// the shared read-only one of the table tabs. Each lane admits a single
/// execution at a time (one connection = one command in flight), but the
/// lanes are independent: the table is browsed while the sticky executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lane {
    Sticky,
    Shared,
}

struct ExecEntry {
    state: ExecState,
    lane: Lane,
    /// Control channel towards the stream **of the current statement**;
    /// replaced at every statement of the batch.
    control: Option<mpsc::Sender<StreamControl>>,
    cancel: Arc<dyn Canceller>,
    /// Budget pause in progress (§5.1): the counter tells the pauses apart,
    /// so the watchdog of an old pause does not stop the new one.
    paused: bool,
    pause_gen: u64,
}

#[derive(Default)]
pub struct Registry {
    execs: Mutex<HashMap<u64, ExecEntry>>,
    results: Mutex<HashMap<u64, LiveResult>>,
    next_exec: AtomicU64,
    next_result: AtomicU64,
}

impl Registry {
    pub fn new() -> Self {
        Registry::default()
    }

    pub fn try_new_execution(&self, cancel: Arc<dyn Canceller>) -> Option<u64> {
        self.try_new_execution_on(Lane::Sticky, cancel)
    }

    /// **Atomic** check-and-register: under a single lock it checks that the
    /// lane is free and registers the execution. `None` = lane busy.
    ///
    /// The check and the registration must sit under the same lock: with two
    /// separate acquisitions two concurrent calls would both pass the check
    /// and start interleaved on the same connection.
    pub fn try_new_execution_on(&self, lane: Lane, cancel: Arc<dyn Canceller>) -> Option<u64> {
        let mut execs = self.execs.lock().unwrap();
        // Pruning of the terminal entries: without it, the map would grow
        // forever. It is safe because nobody queries an outcome after the
        // fact — the flow towards the UI is event-based (§8), `cancel`/
        // `set_exec_state` on a missing id are no-ops and the `run_execution`
        // task tracks its own cancellation locally too, without re-reading the entry.
        execs.retain(|_, e| !e.state.is_terminal());
        if execs.values().any(|e| e.lane == lane) {
            return None;
        }
        let id = self.next_exec.fetch_add(1, Ordering::Relaxed) + 1;
        execs.insert(
            id,
            ExecEntry {
                state: ExecState::Pending,
                lane,
                control: None,
                cancel,
                paused: false,
                pause_gen: 0,
            },
        );
        Some(id)
    }

    /// A single active execution on the sticky connection (§3: there is one sticky).
    pub fn has_active_execution(&self) -> bool {
        self.has_active_on(Lane::Sticky)
    }

    /// A connection admits one command in flight: holds per lane (§3.1).
    pub fn has_active_on(&self, lane: Lane) -> bool {
        self.execs
            .lock()
            .unwrap()
            .values()
            .any(|e| e.lane == lane && !e.state.is_terminal())
    }

    pub fn exec_state(&self, exec_id: u64) -> Option<ExecState> {
        self.execs.lock().unwrap().get(&exec_id).map(|e| e.state)
    }

    pub(crate) fn set_exec_state(&self, exec_id: u64, state: ExecState) {
        if let Some(e) = self.execs.lock().unwrap().get_mut(&exec_id) {
            // The in-flight cancellation wins over the natural outcome: if the
            // user cancelled, the execution ends `Cancelled` even if the
            // current statement had already finished (a perfectly normal race, §7.2).
            if e.state == ExecState::Cancelling && !state.is_terminal() {
                return;
            }
            e.state = state;
        }
    }

    pub(crate) fn set_control(&self, exec_id: u64, control: mpsc::Sender<StreamControl>) {
        if let Some(e) = self.execs.lock().unwrap().get_mut(&exec_id) {
            e.control = Some(control);
        }
    }

    /// Idempotent cancel (§7.2): two clicks = a single action; after
    /// completion it is a no-op, not an error.
    ///
    /// Order of §7: **first** the server-side `CancelRequest` (out-of-band, on
    /// a new connection), **then** we stop consuming the stream.
    pub fn cancel(&self, exec_id: u64) {
        let (cancel, control) = {
            let mut execs = self.execs.lock().unwrap();
            let Some(e) = execs.get_mut(&exec_id) else {
                return;
            };
            match e.state {
                ExecState::Pending | ExecState::Running => {
                    e.state = ExecState::Cancelling;
                    e.paused = false;
                    (Some(Arc::clone(&e.cancel)), e.control.clone())
                }
                _ => return,
            }
        };
        tokio::spawn(async move {
            // Order of §7 preserved by the Canceller: first the server-side
            // shot (best-effort: it may arrive after the query has ended, and
            // behind a pooler it may not arrive at all), then we stop
            // consuming the stream.
            if let Some(c) = cancel {
                // Advisory outcome (§7.1): delivery may fail (pooler, query
                // already finished) and there is no channel towards the UI
                // here — the fallback stays the escalation of §7.2 in the user's hands.
                let _delivered = c.cancel().await;
            }
            if let Some(ctrl) = control {
                let _ = ctrl.send(StreamControl::Stop(StopReason::Cancelled)).await;
            }
        });
    }

    /// Answer to the banner of §5.
    pub fn budget_resume(&self, exec_id: u64) {
        self.clear_paused(exec_id);
        self.send_control(exec_id, StreamControl::Resume);
    }

    /// Another N rows (scroll to the bottom). Only if the execution is really
    /// paused: a duplicate request arriving after the fetch has restarted
    /// must not consume the NEXT pause.
    pub fn budget_more(&self, exec_id: u64) {
        let was_paused = match self.execs.lock().unwrap().get_mut(&exec_id) {
            Some(e) => std::mem::replace(&mut e.paused, false),
            None => false,
        };
        if was_paused {
            self.send_control(exec_id, StreamControl::ResumeRows);
        }
    }

    pub fn budget_stop(&self, exec_id: u64) {
        self.clear_paused(exec_id);
        self.send_control(exec_id, StreamControl::Stop(StopReason::UserStopped));
    }

    /// Marks the start of a budget pause and returns the generation: the
    /// watchdog of §5.1 will stop the fetch only if **this** pause is still
    /// in progress when the timeout expires.
    pub(crate) fn mark_paused(&self, exec_id: u64) -> u64 {
        let mut execs = self.execs.lock().unwrap();
        match execs.get_mut(&exec_id) {
            Some(e) => {
                e.paused = true;
                e.pause_gen += 1;
                e.pause_gen
            }
            None => 0,
        }
    }

    pub(crate) fn is_still_paused(&self, exec_id: u64, gen: u64) -> bool {
        self.execs
            .lock()
            .unwrap()
            .get(&exec_id)
            .map(|e| e.paused && e.pause_gen == gen)
            .unwrap_or(false)
    }

    fn clear_paused(&self, exec_id: u64) {
        if let Some(e) = self.execs.lock().unwrap().get_mut(&exec_id) {
            e.paused = false;
        }
    }

    fn send_control(&self, exec_id: u64, msg: StreamControl) {
        let control = self
            .execs
            .lock()
            .unwrap()
            .get(&exec_id)
            .and_then(|e| e.control.clone());
        if let Some(ctrl) = control {
            tokio::spawn(async move {
                let _ = ctrl.send(msg).await;
            });
        }
    }

    /// Registers an already complete result set (bench/M0 path, where the
    /// collection is `execute_collect` to get the statistics of §15).
    pub fn new_result_done(&self, rs: ResultSet) -> u64 {
        let live = LiveResult {
            inner: Arc::new(RwLock::new(LiveInner::Done(rs))),
            sort: Arc::new(RwLock::new(None)),
            patches: Arc::new(RwLock::new(HashMap::new())),
            hidden: Arc::new(RwLock::new(Vec::new())),
            local: Arc::new(RwLock::new(Vec::new())),
            order: Arc::new(RwLock::new(None)),
            dups: Arc::new(RwLock::new(Vec::new())),
            key_cols: Arc::new(RwLock::new(Vec::new())),
        };
        self.new_result(live)
    }

    pub(crate) fn new_result(&self, live: LiveResult) -> u64 {
        let id = self.next_result.fetch_add(1, Ordering::Relaxed) + 1;
        self.results.lock().unwrap().insert(id, live);
        id
    }

    pub fn result(&self, result_id: u64) -> Option<LiveResult> {
        self.results.lock().unwrap().get(&result_id).cloned()
    }

    pub fn release_result(&self, result_id: u64) {
        self.results.lock().unwrap().remove(&result_id);
    }
}

/// Inert canceller for the crate's tests: the state machine is exercised
/// without real connections.
#[cfg(test)]
pub(crate) fn test_canceller() -> Arc<dyn Canceller> {
    use futures_util::FutureExt;
    struct Noop;
    impl Canceller for Noop {
        fn cancel(&self) -> futures_util::future::BoxFuture<'static, bool> {
            async { true }.boxed()
        }
    }
    Arc::new(Noop)
}

#[cfg(test)]
mod hidden_tests {
    use super::unhide;

    #[test]
    fn unhide_skips_the_hidden_rows() {
        let h = [1, 2, 5];
        let phys: Vec<u64> = (0..5).map(|v| unhide(&h, v)).collect();
        assert_eq!(phys, vec![0, 3, 4, 6, 7]);
        assert_eq!(unhide(&[], 3), 3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Chunk of (id, note) rows, `None` = NULL.
    fn chunk(rows: &[(&str, Option<&str>)]) -> (alamo_core::chunk::Chunk, Vec<StatsAccumulator>) {
        use alamo_core::chunk::{ChunkBuilder, ChunkLimits};
        use alamo_core::column::ColumnKind;
        let mut b = ChunkBuilder::new(
            &[ColumnKind::Utf8, ColumnKind::Utf8],
            ChunkLimits::default(),
        );
        for &(id, note) in rows {
            b.push_text(0, id).unwrap();
            match note {
                Some(s) => b.push_text(1, s).unwrap(),
                None => b.column_mut(1).unwrap().push_null(),
            }
            b.end_row();
        }
        b.finish()
    }

    /// 3×2 result set (id, note) with one NULL.
    fn live3() -> LiveResult {
        use alamo_core::meta::NativeType;
        let pg = |oid| NativeType::Postgres {
            oid,
            type_modifier: -1,
        };
        let schema: Arc<[ColumnMeta]> = vec![
            ColumnMeta::new("id", pg(23), "int4"),
            ColumnMeta::new("note", pg(25), "text"),
        ]
        .into();
        let live = LiveResult::new(schema);
        let (chunk, stats) = chunk(&[("1", Some("a")), ("2", None), ("3", Some("c"))]);
        live.push_chunk(chunk, stats, 0);
        live.finish(ResultSetState::Complete, None);
        live
    }

    fn ids(live: &LiveResult) -> Vec<String> {
        live.window(0..live.rows(), 0..1).columns[0].values.clone()
    }

    #[test]
    fn placed_rows_keep_their_view_row_through_deletes_pages_and_sort() {
        let live = live3();
        assert!(live.append_local(chunk(&[("4", None), ("5", None)]).0, &[0]));
        live.place_rows(3..5, &[3, 1]);
        assert_eq!(ids(&live), ["1", "5", "2", "4", "3"]);
        assert_eq!(live.cell_full(1, 0).as_deref(), Some("5"));
        // Deleted and saved: out of the order too.
        live.hide_rows(&[1]);
        assert_eq!(ids(&live), ["1", "5", "4", "3"]);
        // Next page: at the bottom of the order.
        assert!(live.append_chunk(chunk(&[("6", None)]).0));
        assert_eq!(live.rows(), 5);
        assert_eq!(ids(&live), ["1", "5", "4", "3", "6"]);
        // The local sort reads the physical rows, then the order comes back.
        live.sort(0, true);
        assert_eq!(ids(&live), ["6", "5", "4", "3", "1"]);
        live.clear_sort();
        assert_eq!(ids(&live), ["1", "5", "4", "3", "6"]);
        assert_eq!(live.server_offset(), 3, "pages: 1, 3 and 6 (2 deleted)");
    }

    #[test]
    fn a_page_bringing_rows_already_on_screen_hides_them_and_counts_them() {
        let live = live3();
        assert!(live.append_local(chunk(&[("9", None)]).0, &[0]));
        live.place_rows(3..4, &[1]);
        assert_eq!(live.server_offset(), 3);
        // Next page: 4, then 9, the inserted row, where the server sorts it.
        assert!(live.append_chunk(chunk(&[("4", None), ("9", None)]).0));
        live.hide_duplicates(4..6);
        assert_eq!(ids(&live), ["1", "9", "2", "3", "4"]);
        assert_eq!(live.server_offset(), 5, "the server sent 1, 2, 3, 4 and 9");
        // An insert sorting before the offset pushed 4 into the next page.
        assert!(live.append_chunk(chunk(&[("4", None), ("5", None)]).0));
        live.hide_duplicates(6..8);
        assert_eq!(ids(&live), ["1", "9", "2", "3", "4", "5"]);
        assert_eq!(live.server_offset(), 7);
        // The inserted row deleted: its copy stops counting.
        assert_eq!(live.remove_rows(&[0], &[vec![Some("9".into())]]), 1);
        assert_eq!(ids(&live), ["1", "2", "3", "4", "5"]);
        assert_eq!(live.server_offset(), 6);
    }

    #[test]
    fn the_reread_row_overrides_window_and_full_cell_even_with_sort() {
        let live = live3();
        let rows = live.find_rows(&[0], &[vec![Some("2".into())], vec![Some("9".into())]]);
        assert_eq!(rows, vec![Some(1), None]);
        live.patch_row(1, vec![Some("2".into()), Some("new".into())]);
        let w = live.window(0..3, 0..2);
        assert_eq!(w.columns[1].values[1], "new");
        assert!(!w.columns[1].nulls[1]);
        assert_eq!(live.cell_full(1, 1).as_deref(), Some("new"));
        // Descending sort on id: physical row 1 stays in the middle, the
        // correction follows it.
        live.sort(0, true);
        let w = live.window(0..3, 1..2);
        assert_eq!(w.columns[0].values[1], "new");
        assert_eq!(live.cell_full(0, 1).as_deref(), Some("c"));
        // Re-read NULL: bitmap, not a string.
        live.patch_row(0, vec![Some("1".into()), None]);
        assert_eq!(live.cell_full(2, 1), None);
        // The key changed by the correction is found with the new value.
        live.patch_row(2, vec![Some("30".into()), Some("c".into())]);
        assert_eq!(
            live.find_rows(&[0], &[vec![Some("30".into())]]),
            vec![Some(2)]
        );
    }

    #[test]
    fn busy_lane_refuses_but_lanes_are_independent() {
        let r = Registry::new();
        let a = r.try_new_execution(test_canceller()).expect("sticky free");
        assert!(r.try_new_execution(test_canceller()).is_none());
        // §3.1: the shared lane is not blocked by the sticky one.
        assert!(r
            .try_new_execution_on(Lane::Shared, test_canceller())
            .is_some());
        r.set_exec_state(a, ExecState::Completed);
        assert!(r.try_new_execution(test_canceller()).is_some());
    }

    #[test]
    fn registration_prunes_the_terminal_entries() {
        let r = Registry::new();
        let a = r.try_new_execution(test_canceller()).expect("sticky free");
        r.set_exec_state(a, ExecState::Running);
        r.set_exec_state(a, ExecState::Failed);
        let b = r
            .try_new_execution(test_canceller())
            .expect("terminal does not block");
        assert_eq!(r.exec_state(a), None, "terminal entry pruned");
        assert_eq!(r.exec_state(b), Some(ExecState::Pending));
        // On a pruned id cancel/set_exec_state are no-ops, not panics.
        r.cancel(a);
        r.set_exec_state(a, ExecState::Completed);
        assert_eq!(r.exec_state(a), None);
    }

    #[test]
    fn pruning_does_not_touch_live_executions() {
        let r = Registry::new();
        let a = r.try_new_execution(test_canceller()).expect("sticky free");
        r.set_exec_state(a, ExecState::Running);
        let b = r
            .try_new_execution_on(Lane::Shared, test_canceller())
            .expect("shared free");
        assert_eq!(r.exec_state(a), Some(ExecState::Running));
        assert_eq!(r.exec_state(b), Some(ExecState::Pending));
    }
}
