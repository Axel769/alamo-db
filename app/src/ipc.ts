// Types mirroring the IPC surface of §8 (serde in alamo-session/events.rs).
import { Channel, invoke } from "@tauri-apps/api/core";
import { Settings, SettingValues } from "./settingsSchema";

/// A value of settings.json this build could not take (§16): default used.
export type SettingsIssue = { key: string; found: string; message: string };
export type SettingsOut = { settings: Settings; issues: SettingsIssue[] };

export type SemanticClass = "numeric" | "temporal" | "text" | "boolean" | "structured";

export type ColumnOut = {
  name: string;
  type_display: string;
  semantic_class: SemanticClass;
  nullable: boolean | null;
  numeric_text: boolean;
  undecoded: boolean;
  /// Source `db.table` (MySQL); null = computed or unknown.
  source_table: string | null;
};

export type ErrorOut = {
  severity: string;
  category: string;
  code: string | null;
  message: string;
  detail: string | null;
  hint: string | null;
  context: string | null;
  position_char: number | null;
  internal_query: string | null;
  statement_char_offset: number | null;
  statement_index: number | null;
};

export type OutcomeOut =
  | { kind: "row_set"; result_id: number; rows: number; state: string }
  | { kind: "affected"; rows: number }
  | { kind: "void" }
  | { kind: "failed"; error: ErrorOut }
  | { kind: "cancelled"; result_id: number | null; rows: number }
  | { kind: "skipped" };

export type ExecEvent =
  | { type: "started"; exec_id: number; statement_count: number }
  | {
      type: "statement_started";
      exec_id: number;
      index: number;
      char_offset: number;
      byte_offset: number;
    }
  | {
      type: "schema";
      exec_id: number;
      index: number;
      result_id: number;
      columns: ColumnOut[];
    }
  | {
      type: "rows";
      exec_id: number;
      result_id: number;
      rows: number;
      layout_bytes: number;
    }
  | {
      type: "budget_paused";
      exec_id: number;
      result_id: number;
      rows: number;
      layout_bytes: number;
      /// Precautionary pause at N rows (SQL tab), not the memory budget.
      by_rows: boolean;
    }
  | { type: "notice"; exec_id: number; index: number; notice: ErrorOut }
  | {
      type: "statement_finished";
      exec_id: number;
      index: number;
      outcome: OutcomeOut;
      elapsed_ms: number;
    }
  | { type: "finished"; exec_id: number; state: string };

/// Connection profile (§9.1). Never a password in here: it lives in the
/// system keychain or in memory only, and never crosses the IPC
/// towards the frontend (§12).
export type Profile = {
  id: number | null;
  name: string;
  /// Engine id (`engines.ts`): "postgres" or "mysql" (MariaDB included:
  /// recognized at handshake) in this build.
  dialect: string;
  host: string;
  port: number;
  database: string;
  username: string;
  sslmode: string;
  /// Custom root CA (PEM path) for verify-ca/verify-full (§12).
  ssl_root_cert: string | null;
  color: string | null;
  is_production: boolean;
  /// §9.7 point 4: read-only — writes rejected by the classifier and by
  /// the server (session read-only transactions), grid editing off.
  read_only: boolean;
  /// SSH tunnel (§12): reference to a saved SSH profile — the bastion
  /// is an entity of its own, shared by several database profiles. null = direct.
  ssh_profile_id: number | null;
  /// Filter of the databases/schemas shown (DBeaver model): comma-separated
  /// masks, `*` = any text. Empty = all. Display only: everything stays
  /// reachable from SQL.
  db_include: string;
  db_exclude: string;
};

/// SSH profile (§12): bastion saved once, reused by several database
/// profiles. The credential lives in the keychain, tied to this id.
export type SshProfile = {
  id: number | null;
  name: string;
  host: string;
  port: number;
  username: string;
  /// "agent" | "password" | "key"
  auth: string;
  key_path: string | null;
};

/// SSH profile as the manager shows it (see `SshProfileOut` in the backend).
export type SshProfileOut = SshProfile & {
  /// Database profiles that go through this bastion, by name.
  used_by: string[];
  /// Bastion key confirmed by the user, as a fingerprint; null = never seen.
  host_fingerprint: string | null;
  /// Its credential is in the keychain (the credential never arrives).
  secret_saved: boolean;
};

export type SortOut = { rows: number; incomplete: boolean };

/// Result of `tableDataMore`: total rows now in the result set, rows and
/// bytes added, and whether the page was full (there may be another one).
export type TableMoreOut = {
  rows: number;
  added: number;
  layout_bytes: number;
  more: boolean;
};

export type WindowOut = {
  row_start: number;
  row_end: number;
  columns: { index: number; values: string[]; nulls: boolean[]; truncated: boolean[] }[];
};

// Introspection (§10): mirroring alamo-session/introspect.rs.

export type SchemaOut = { name: string; is_system: boolean };
/// Tree favorite (per profile): `table` null = the whole database/schema.
export type Favorite = { schema: string; table: string | null };

/// Numbers per schema/database (connection page).
export type SchemaStatsOut = {
  name: string;
  is_system: boolean;
  tables: number;
  size_bytes: number | null;
  charset: string | null;
  collation: string | null;
};

export type RelationKind =
  | "table"
  | "view"
  | "materialized_view"
  | "foreign_table"
  | "partitioned_table";

export type RelationOut = {
  schema: string;
  name: string;
  kind: RelationKind;
  estimated_rows: number | null;
  comment: string | null;
};

export type ColumnInfoOut = {
  name: string;
  type_display: string;
  nullable: boolean;
  default: string | null;
  is_primary_key: boolean;
  ordinal: number;
};

/// Foreign key: `schema.table(columns)` → `ref_schema.ref_table(ref_columns)`.
export type ForeignKeyOut = {
  name: string;
  schema: string;
  table: string;
  columns: string[];
  ref_schema: string;
  ref_table: string;
  ref_columns: string[];
};

export type AutocompleteOut = { schema: string; table: string; column: string };

// Server-side re-query of the table tab (decision 10, second half):
// mirroring alamo-session/table.rs. snake_case keys: they cross serde.

export type FilterOp =
  | "eq"
  | "ne"
  | "lt"
  | "le"
  | "gt"
  | "ge"
  | "like"
  | "not_like"
  | "is_null"
  | "is_not_null";

export type ColumnFilter = { column: string; op: FilterOp; value: string };

export type OrderBy = { column: string; desc: boolean };

export type TableQuery = {
  /// ORDER BY columns in priority order; [] = no ordering.
  order_by: OrderBy[];
  filters: ColumnFilter[];
  /// Hand-written condition (WHERE bar), ANDed with the filters. "" = none.
  where_sql: string;
};

export function emptyTableQuery(): TableQuery {
  return { order_by: [], filters: [], where_sql: "" };
}

/// Query read from the saved workspace: missing fields (WHERE bar) and
/// `order_by` in the old shape (an object or null, before multi-column
/// sort) come back in the current shape.
export function normalizeTableQuery(raw: unknown): TableQuery {
  const q = (raw ?? {}) as Partial<TableQuery> & { order_by?: unknown };
  const ob = q.order_by;
  return {
    ...emptyTableQuery(),
    ...q,
    order_by: Array.isArray(ob) ? (ob as OrderBy[]) : ob && typeof ob === "object" ? [ob as OrderBy] : [],
  };
}

/// Column → value for the grid-editing UPDATE (M4). `null` = SQL
/// NULL. Mirrors alamo-session/table.rs.
/// `default: true` = `SET col = DEFAULT` (only in the SET, `value` ignored).
/// `numeric`: the column holds numbers, the value is sent bare in SET /
/// VALUES (MySQL BIT rejects the quoted form). See alamo-session/table.rs.
export type CellAssign = { column: string; value: string | null; default?: boolean; numeric?: boolean };

/// The "column default" value in pending edits: the server decides it
/// on save. Single object, compared by identity.
export const DEFAULT_CELL = Object.freeze({ default: true as const });
/// Pending value of a cell: text, NULL or DEFAULT.
export type PendingValue = string | null | typeof DEFAULT_CELL;

export function isDefaultCell(v: PendingValue | undefined): v is typeof DEFAULT_CELL {
  return v === DEFAULT_CELL;
}

/// One row of the multi-cell batch (decision 12): SET + PK key.
export type RowUpdate = { set: CellAssign[]; key: CellAssign[] };
/// New row ("+ Row" / "Duplicate"): the columns not listed are filled by
/// the server with their default.
export type RowInsert = { values: CellAssign[] };
/// One write of the table tab's mixed batch. The backend runs the UPDATEs
/// first, then the INSERTs: `applied` and `error_index` follow that order.
export type TableWrite =
  | ({ kind: "update" } & RowUpdate)
  | ({ kind: "insert" } & RowInsert)
  | { kind: "delete"; key: CellAssign[] };

/// Result of the table tab's batch: statement errors live here, not in the
/// command's reject. Mirrors alamo-session/lib.rs.
export type BatchUpdateOut = {
  /// Affected rows per successful UPDATE (expected: all 1).
  applied: number[];
  error_index: number | null;
  error: ErrorOut | null;
  /// Prod §9.7: the batch is in the sticky's transaction, the user closes it.
  in_transaction: boolean;
  /// Error on the shared connection: everything undone with ROLLBACK.
  rolled_back: boolean;
  /// Inserted rows re-read and appended to the result set of `AppendTo`: the
  /// grid shows them without reloading. Fewer than the INSERTs = reload.
  appended: number;
  /// Deleted rows removed from the view of the result set of `AppendTo`.
  removed: number;
  /// Rows of the view after the batch (`null` without `AppendTo`).
  rows: number | null;
};

/// Where to append the just-inserted rows: the open result set, the PK
/// columns (to re-read them on MySQL, which has no RETURNING) and `at`, the
/// screen row of each one, where it stays after the save (empty = bottom).
export type AppendTo = { result_id: number; key_columns: string[]; at: number[] };

// Explicit transactions (§9.7) — mirroring alamo-session/txn.rs.

export type TxnState = {
  open: boolean;
  has_writes: boolean;
  writes: number;
  open_for_ms: number | null;
  idle_for_ms: number | null;
  idle_rollback_ms: number;
};

/// Writes of the open transaction (§9.7.2): at most 500, `total` counts all.
export type TxnLog = {
  writes: { sql: string; truncated: boolean; rows: number | null; at_ms: number }[];
  total: number;
};

export type StatementClass = "read" | "write" | "ddl" | "control" | "unknown";

export type StatementClassOut = {
  index: number;
  char_offset: number;
  class: StatementClass;
  /// For the prod gates (§9.7.3): unknown counts as a write.
  is_write: boolean;
  irreversible: boolean;
  /// UPDATE/DELETE without WHERE: confirmed on every profile.
  unfiltered: boolean;
};

/// Boundaries of a statement in the editor text, in characters (code points).
export type StatementRangeOut = { index: number; char_offset: number; char_len: number };

/// An engine of this build (alamo-session `EngineInfo`, §2.2).
export type EngineOut = {
  id: string;
  name: string;
  short_name: string;
  default_port: number;
  default_user: string;
  database_required: boolean;
  tls_modes: string[];
  sql_syntax: string;
  can_change_database_in_session: boolean;
  transactional_ddl: boolean;
};

// Export and formatted copy (decision 11) — mirroring alamo-session/export.rs.

export type ExportFormat = "tsv" | "csv" | "json" | "insert";

export type ExportOptions = {
  format: ExportFormat;
  /// Header row (TSV/CSV only).
  headers: boolean;
  /// Table name for the INSERT format ("schema.table" allowed).
  table_name: string;
  /// TSV/CSV: `'` in front of values Excel would read as a formula.
  /// Absent = on (backend default).
  neutralize_formulas?: boolean;
};

export type ExportOut = { path: string; rows: number };

/// One result of a script export (serde `export::ScriptItem`).
export type ScriptItem = { result_id: number; position: number; sql: string; table_name: string };
/// Where the results of a script go: one file with a section per statement,
/// or a new folder with one file per statement.
export type ScriptExportTarget = "file" | "folder";

/// Response of the connect from profile (M3.5): the id of the open connection —
/// every later command carries it — and the profile.
/// What really protects the open connection (§12.2), measured on the
/// session: `prefer` may have fallen back to plaintext. `null` = the engine
/// cannot tell (shown as "not available").
export type ConnSecurity = {
  encrypted: boolean;
  verified: boolean;
  protocol: string | null;
  cipher: string | null;
  cert: CertOut | null;
};

/// A certificate as the backend summarises it: the server's, or the
/// profile's root CA. Dates in unix seconds.
export type CertOut = {
  subject: string | null;
  issuer: string | null;
  not_before: number | null;
  not_after: number | null;
};

/// Outcome of "Test connection".
export type TestOut = { database: string | null; elapsed_ms: number; security: ConnSecurity };

export type ConnectOut = {
  conn_id: number;
  profile: Profile;
  security: ConnSecurity;
  /// PG (§3.3): one database per connection, the others via `openDatabase`.
  one_db_per_conn: boolean;
};

/// Connections export file (serde `transfer::ExportDoc`). The frontend
/// shows it and sends it back as is.
export type ExportDoc = {
  format: string;
  version: number;
  exported_at: string | null;
  ssh_profiles: { ref: string; name: string; host: string; port: number; username: string; auth: string; key_path?: string }[];
  profiles: {
    name: string;
    dialect: string;
    host: string;
    port: number;
    database: string;
    username: string;
    sslmode: string;
    color?: string;
    is_production: boolean;
    read_only: boolean;
    ssh?: string;
  }[];
  /// Encrypted passwords (opaque blob): present = the import asks for the file password.
  secrets?: unknown;
};

/// Preview row, aligned by index to `doc.profiles`.
export type ImportRow = { name: string; renamed: boolean; existing: boolean; warnings: string[] };
export type ImportPreview = { file_name: string; doc: ExportDoc; rows: ImportRow[] };
export type ImportOutcome = {
  imported: number;
  ssh_created: number;
  ssh_reused: number;
  passwords_saved: number;
  warnings: string[];
};

// M3.5 (DBeaver model): several connections open at once. Every command that
// touches a session carries `connId`: exec_id and result_id are PER-session
// counters, without the id the connections collide.
export const ipc = {
  engines: () => invoke<EngineOut[]>("engines"),
  connect: (url: string | null) => invoke<number>("connect", { url }),
  /// password/sshSecret null or empty = use the keychain entries, if
  /// any. savePassword is the desired state of the keychain entries
  /// (§12). sshSecret = bastion password or key passphrase.
  connectProfile: (
    id: number,
    password: string | null,
    savePassword: boolean,
    sshSecret: string | null,
  ) => invoke<ConnectOut>("connect_profile", { id, password, savePassword, sshSecret }),
  /// Credentials never cross the IPC towards the frontend: only the flags.
  /// "Test connection": opens, runs a query, closes. Does not touch the keychain.
  testProfile: (id: number, password: string | null, sshSecret: string | null) =>
    invoke<TestOut>("test_profile", { id, password, sshSecret }),
  /// Native picker for the profile's root CA. `null` = cancelled.
  pickRootCert: () => invoke<string | null>("pick_root_cert"),
  /// Subject, issuer and validity of the root CA file; rejects with the
  /// reason (missing, unreadable, not a PEM certificate).
  inspectRootCert: (path: string) => invoke<CertOut>("inspect_root_cert", { path }),
  profilePasswordPresent: (id: number) =>
    invoke<{ db: boolean; ssh: boolean }>("profile_password_present", { id }),
  sshProfilesList: () => invoke<SshProfileOut[]>("ssh_profiles_list"),
  /// Only the profile's own fields cross: a `SshProfileOut` is fine as input.
  sshProfileSave: ({ id, name, host, port, username, auth, key_path }: SshProfile) =>
    invoke<number>("ssh_profile_save", { profile: { id, name, host, port, username, auth, key_path } }),
  sshProfileDelete: (id: number) => invoke<void>("ssh_profile_delete", { id }),
  /// "Test tunnel" on a SAVED SSH profile: bastion, host key, credential —
  /// nothing forwarded. Empty secret = the keychain's; with saveSecret a typed
  /// one is saved once accepted.
  sshProfileTest: (id: number, secret: string | null, saveSecret: boolean) =>
    invoke<{ elapsed_ms: number }>("ssh_profile_test", { id, secret, saveSecret }),
  /// As `sshHostKeyTrust`, but `id` is the SSH profile (from the manager).
  sshProfileHostKeyTrust: (id: number, fingerprint: string) =>
    invoke<void>("ssh_profile_host_key_trust", { id, fingerprint }),
  sshHostKeyForget: (id: number) => invoke<void>("ssh_host_key_forget", { id }),
  sshSecretForget: (id: number) => invoke<void>("ssh_secret_forget", { id }),
  /// Never-seen bastion (error `ssh_host_key_unknown`): the user trusts
  /// the fingerprint shown. The backend saves the key it saw itself,
  /// only if the fingerprint matches; then the connection is retried.
  sshHostKeyTrust: (id: number, fingerprint: string) =>
    invoke<void>("ssh_host_key_trust", { id, fingerprint }),
  profilesList: () => invoke<Profile[]>("profiles_list"),
  profileSave: (profile: Profile) => invoke<number>("profile_save", { profile }),
  /// Databases of the server (PG: each one opened with `openDatabase`).
  serverDatabases: (connId: number) => invoke<SchemaOut[]>("server_databases", { connId }),
  /// Another database of the profile's PG server, on a connection of its
  /// own (§3.3): the profile's database gives back the profile's connection.
  openDatabase: (connId: number, db: string) => invoke<ConnectOut>("open_database", { connId, db }),
  profileDelete: (id: number) => invoke<void>("profile_delete", { id }),
  /// Export of the chosen connections to a JSON file: native dialog on the Rust side.
  /// With `passphrase` the keychain passwords go into the file, encrypted.
  /// `null` = cancelled.
  connectionsExport: (profileIds: number[], passphrase: string | null, suggestedName: string) =>
    invoke<{ path: string; count: number; passwords: number } | null>("connections_export", {
      profileIds,
      passphrase,
      suggestedName,
    }),
  /// Import, first half: native open dialog + preview, writes
  /// nothing. `null` = cancelled.
  connectionsImportOpen: () => invoke<ImportPreview | null>("connections_import_open"),
  /// Import, second half: the preview document goes back with the
  /// chosen indices; all in one transaction. `passphrase` = file
  /// password for the encrypted passwords (null = import without).
  connectionsImportApply: (doc: ExportDoc, selected: number[], passphrase: string | null) =>
    invoke<ImportOutcome>("connections_import_apply", { doc, selected, passphrase }),
  autosaveGet: (tabId: string) => invoke<string | null>("autosave_get", { tabId }),
  autosaveSet: (tabId: string, sql: string) => invoke<void>("autosave_set", { tabId, sql }),
  autosaveDelete: (tabId: string) => invoke<void>("autosave_delete", { tabId }),
  workspaceGet: () => invoke<string | null>("workspace_get"),
  workspaceSet: (json: string) => invoke<void>("workspace_set", { json }),
  tableDefaultSortGet: (profileId: number, schema: string, table: string) =>
    invoke<OrderBy[] | null>("table_default_sort_get", { profileId, schema, table }),
  /// Empty `order` = removes the default.
  tableDefaultSortSet: (profileId: number, schema: string, table: string, order: OrderBy[]) =>
    invoke<void>("table_default_sort_set", { profileId, schema, table, order }),
  favoritesList: (profileId: number) => invoke<Favorite[]>("favorites_list", { profileId }),
  /// `table` null = the whole database/schema.
  favoriteSet: (profileId: number, schema: string, table: string | null, on: boolean) =>
    invoke<void>("favorite_set", { profileId, schema, table, on }),
  schemaList: (connId: number) => invoke<SchemaOut[]>("schema_list", { connId }),
  schemaStats: (connId: number) => invoke<SchemaStatsOut[]>("schema_stats", { connId }),
  schemaRelations: (connId: number, schema: string) =>
    invoke<RelationOut[]>("schema_relations", { connId, schema }),
  /// Names of every relation of the server in one round trip (tree
  /// filter): `estimated_rows` and `comment` are always null here.
  schemaRelationsAll: (connId: number) =>
    invoke<RelationOut[]>("schema_relations_all", { connId }),
  schemaColumns: (connId: number, schema: string, table: string) =>
    invoke<ColumnInfoOut[]>("schema_columns", { connId, schema, table }),
  /// Foreign keys touching the table, outgoing and incoming.
  schemaForeignKeys: (connId: number, schema: string, table: string) =>
    invoke<ForeignKeyOut[]>("schema_foreign_keys", { connId, schema, table }),
  /// `schema` = only that database/schema (fast, never truncated); without,
  /// the whole server up to the cap.
  autocompleteCatalog: (connId: number, schema: string | null = null) =>
    invoke<AutocompleteOut[]>("autocomplete_catalog", { connId, schema }),
  terminateConnection: (connId: number) => invoke<boolean>("terminate_connection", { connId }),
  disconnect: (connId: number) => invoke<void>("disconnect", { connId }),
  execute(connId: number, sql: string, onEvent: (ev: ExecEvent) => void): Promise<number> {
    const channel = new Channel<ExecEvent>();
    channel.onmessage = onEvent;
    return invoke<number>("execute", { connId, sql, onEvent: channel });
  },
  /// Data for the table tab (§9.5): SQL generated by the app, shared
  /// connection (§3.1). Same events as execute. `query` carries server-side
  /// sort and filters (decision 10, second half).
  tableData(
    connId: number,
    schema: string,
    table: string,
    query: TableQuery,
    onEvent: (ev: ExecEvent) => void
  ): Promise<number> {
    const channel = new Channel<ExecEvent>();
    channel.onmessage = onEvent;
    return invoke<number>("table_data", { connId, schema, table, query, onEvent: channel });
  },
  /// Next page of the table tab, appended to the same result set
  /// (the grid asks for it when reaching the bottom). Responds once the page has arrived.
  tableDataMore: (
    connId: number,
    schema: string,
    table: string,
    query: TableQuery,
    resultId: number
  ) => invoke<TableMoreOut>("table_data_more", { connId, schema, table, query, resultId }),
  /// The SELECT that `tableData` runs for that query (same generator).
  tableDataSql: (connId: number, schema: string, table: string, query: TableQuery) =>
    invoke<string>("table_data_sql", { connId, schema, table, query }),
  /// Transaction state of the sticky (§9.7): requested at the end of execution and
  /// after commit/rollback, never polled.
  txnState: (connId: number) => invoke<TxnState>("txn_state", { connId }),
  /// Before using the editor's connection: if it dropped, reopen it.
  ensureConnected: (connId: number) =>
    invoke<{ reconnected: boolean; lost_txn: boolean; lost_writes: boolean }>("ensure_connected", {
      connId,
    }),
  /// "Reconnect": tunnel and connections rebuilt right away, without checking.
  reconnect: (connId: number) =>
    invoke<{ reconnected: boolean; lost_txn: boolean; lost_writes: boolean }>("reconnect", { connId }),
  txnLog: (connId: number) => invoke<TxnLog>("txn_log", { connId }),
  setManualCommit: (connId: number, on: boolean) => invoke<void>("set_manual_commit", { connId, on }),
  txnCommit: (connId: number) => invoke<TxnState>("txn_commit", { connId }),
  txnRollback: (connId: number) => invoke<TxnState>("txn_rollback", { connId }),
  /// Conservative classification of the batch (§9.7.3) for the prod gates.
  classifySql: (connId: number, sql: string) =>
    invoke<StatementClassOut[]>("classify_sql", { connId, sql }),
  /// Same splitter as execution: for "run the query under the cursor".
  splitSql: (connId: number, sql: string) =>
    invoke<StatementRangeOut[]>("split_sql", { connId, sql }),
  /// Row estimate from EXPLAIN for the confirmation dialogs (§9.7): best-effort,
  /// null = not available (batch, unexplainable statement, error).
  explainEstimate: (connId: number, sql: string) =>
    invoke<number | null>("explain_estimate", { connId, sql }),
  /// Preview of the grid-editing UPDATE (M4): generated from the same
  /// parameters tableUpdate then runs — the preview cannot lie.
  tableUpdateSql: (
    connId: number,
    schema: string,
    table: string,
    set: CellAssign[],
    key: CellAssign[]
  ) => invoke<string>("table_update_sql", { connId, schema, table, set, key }),
  /// UPDATE of one row (M4): shared lane in auto-commit, same events
  /// as execute — the outcome arrives as `affected`.
  tableUpdate(
    connId: number,
    schema: string,
    table: string,
    set: CellAssign[],
    key: CellAssign[],
    onEvent: (ev: ExecEvent) => void
  ): Promise<number> {
    const channel = new Channel<ExecEvent>();
    channel.onmessage = onEvent;
    return invoke<number>("table_update", { connId, schema, table, set, key, onEvent: channel });
  },
  /// Preview of the multi-cell batch (decision 12).
  tableUpdateSqlBatch: (connId: number, schema: string, table: string, updates: RowUpdate[]) =>
    invoke<string>("table_update_sql_batch", { connId, schema, table, updates }),
  /// Multi-cell/paste batch: blocking, outcome in the return value.
  tableUpdateBatch: (connId: number, schema: string, table: string, updates: RowUpdate[]) =>
    invoke<BatchUpdateOut>("table_update_batch", { connId, schema, table, updates }),
  tableWriteSqlBatch: (connId: number, schema: string, table: string, writes: TableWrite[]) =>
    invoke<string>("table_write_sql_batch", { connId, schema, table, writes }),
  tableWriteBatch: (
    connId: number,
    schema: string,
    table: string,
    writes: TableWrite[],
    append: AppendTo | null = null
  ) => invoke<BatchUpdateOut>("table_write_batch", { connId, schema, table, writes, append }),
  /// After a save: re-reads the saved rows and replaces them in the
  /// result set. `missing > 0` = some not found, better to reload.
  tableRefreshRows: (
    connId: number,
    schema: string,
    table: string,
    resultId: number,
    updates: RowUpdate[]
  ) =>
    invoke<{ refreshed: number; missing: number }>("table_refresh_rows", {
      connId,
      schema,
      table,
      resultId,
      updates,
    }),
  tableDdl: (connId: number, schema: string, table: string) =>
    invoke<string>("table_ddl", { connId, schema, table }),
  /// Close confirmed by the "pending writes" dialog (§9.7.2): exits
  /// without going through the close gates again.
  appQuit: () => invoke<void>("app_quit"),
  /// Unsaved grid edits: they too stop the close.
  setUnsavedEdits: (on: boolean) => invoke<void>("set_unsaved_edits", { on }),
  /// Settings (§16): the file is Rust's; `issues` only on the first read.
  settingsGet: () => invoke<SettingsOut>("settings_get"),
  settingsSet: (patch: Partial<SettingValues>) => invoke<SettingsOut>("settings_set", { patch }),
  /// Back to the defaults of one category, or of everything with `null`.
  settingsReset: (category: string | null) => invoke<SettingsOut>("settings_reset", { category }),
  /// Alpha updates: new version published (null = none).
  updateCheck: () => invoke<{ version: string; current_version: string; notes: string | null } | null>("update_check"),
  /// Downloads, installs and restarts: if it succeeds it does not return.
  updateInstall: () => invoke<void>("update_install"),
  /// Integrated title bar: drag / maximize the window.
  windowStartDrag: () => invoke<void>("window_start_drag"),
  windowToggleMaximize: () => invoke<void>("window_toggle_maximize"),
  /// Current database of the sticky (on MySQL reflects the last USE).
  currentDatabase: (connId: number) => invoke<string | null>("current_database", { connId }),
  /// USE on the sticky (MySQL only): the database select in the toolbar.
  useDatabase: (connId: number, db: string) => invoke<void>("use_database", { connId, db }),
  cancel: (connId: number, execId: number) => invoke<void>("cancel", { connId, execId }),
  budgetResume: (connId: number, execId: number) =>
    invoke<void>("budget_resume", { connId, execId }),
  /// Paged SQL tab: the next page (scroll to the bottom), then pause again.
  budgetMore: (connId: number, execId: number) => invoke<void>("budget_more", { connId, execId }),
  budgetStop: (connId: number, execId: number) => invoke<void>("budget_stop", { connId, execId }),
  fetchWindow: (
    connId: number,
    resultId: number,
    rowStart: number,
    rowEnd: number,
    colStart: number,
    colEnd: number
  ) => invoke<WindowOut>("fetch_window", { connId, resultId, rowStart, rowEnd, colStart, colEnd }),
  /// Local sort on a permutation (§4.8, decision 10). `col: null` removes the view.
  sortResult: (connId: number, resultId: number, col: number | null, desc: boolean) =>
    invoke<SortOut>("sort_result", { connId, resultId, col, desc }),
  /// Export to file (decision 11): native dialog on the Rust side, streaming
  /// write with the full values. `null` = cancelled.
  exportResult: (
    connId: number,
    resultId: number,
    opts: ExportOptions,
    suggestedName: string
  ) => invoke<ExportOut | null>("export_result", { connId, resultId, opts, suggestedName }),
  /// Export of all the results of a script: native dialog (file or folder)
  /// on the Rust side; `suggestedName` without extension. null = cancelled.
  exportScript: (
    connId: number,
    items: ScriptItem[],
    opts: ExportOptions,
    target: ScriptExportTarget,
    suggestedName: string
  ) => invoke<ExportOut | null>("export_script", { connId, items, opts, target, suggestedName }),
  /// Formatted copy of the selection: the clipboard is written in Rust
  /// (navigator.clipboard dies silently in the WKWebView).
  copyResultCells: (
    connId: number,
    resultId: number,
    range: { row_start: number; row_end: number; col_start: number; col_end: number },
    opts: ExportOptions
  ) => invoke<number>("copy_result_cells", { connId, resultId, range, opts }),
  getCellFull: (connId: number, resultId: number, row: number, col: number) =>
    invoke<string | null>("get_cell_full", { connId, resultId, row, col }),
  releaseResult: (connId: number, resultId: number) =>
    invoke<void>("release_result", { connId, resultId }),
};
