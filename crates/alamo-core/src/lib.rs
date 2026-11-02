//! # alamo-core
//!
//! Alamo's columnar data model, schema, errors and dialect abstraction.
//!
//! **Non-negotiable constraint (ARCHITECTURE.md §2): this crate does not depend
//! on Tauri.** It has no external dependencies at all. This is what makes it
//! possible to run spikes (`alamo-bench`) and integration tests from the CLI,
//! without a webview — and at M0 that is exactly the point: phase 1 of the
//! spike measures the pipeline *without* a webview that could mask the numbers.
//!
//! The `§N` references in the comments point to `ARCHITECTURE.md` at the root
//! of the repo. They are not decoration: every counter-intuitive layout choice
//! (inverted bitmap, `i32` offsets, irreversible degradation to `Utf8`) has a
//! rationale there that cannot be reconstructed from the code.

#![forbid(unsafe_code)]

pub mod bitmap;
pub mod chunk;
pub mod classify;
pub mod column;
pub mod dialect;
pub mod error;
pub mod fmt;
pub mod lob;
pub mod meta;
pub mod result_set;
pub mod schema_info;
pub mod sort;
pub mod splitter;
pub mod stream;
pub mod tls;
pub mod value;
pub mod varbuf;
pub mod window;

pub use bitmap::Bitmap;
pub use chunk::{Chunk, ChunkBuilder, ChunkLimits};
pub use column::{Column, ColumnBuilder, ColumnData, ColumnKind};
pub use dialect::{Capabilities, Dialect};
pub use error::{DbError, ErrorCategory, ErrorCode, ErrorPosition, Severity};
pub use lob::{LobArena, LobHandle, LOB_INLINE_CAP};
pub use meta::{
    ColumnMeta, ColumnSource, ColumnStats, NativeType, SemanticClass, StatsAccumulator,
};
pub use result_set::{ExecutionOutcome, ResultSet, ResultSetBuilder, ResultSetState};
pub use schema_info::{ColumnInfo, RelKind, RelationInfo, SchemaInfo};
pub use value::ValueRef;
pub use varbuf::VarBuf;
pub use window::{Window, WindowColumn};

use std::fmt as stdfmt;

/// Errors **internal** to the data model, distinct from `DbError`, which
/// describes what the server said. Confusing them is like confusing a bug with
/// an SQL error: the UI reacts differently (§6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// The varlen buffer would exceed the `i32` range. It should never reach
    /// the user: the `ChunkBuilder` closes long before that (§5).
    OffsetOverflow {
        current: usize,
        adding: usize,
    },
    /// Push of a type onto a column that cannot hold it and is not degradable.
    KindMismatch {
        expected: ColumnKind,
    },
    ColumnOutOfRange {
        index: usize,
    },
}

impl stdfmt::Display for CoreError {
    fn fmt(&self, f: &mut stdfmt::Formatter<'_>) -> stdfmt::Result {
        match self {
            CoreError::OffsetOverflow { current, adding } => write!(
                f,
                "i32 offset overflow: {current} bytes in the buffer, +{adding} incoming \
                 (the chunk should have been closed earlier, see ChunkLimits)"
            ),
            CoreError::KindMismatch { expected } => {
                write!(f, "type incompatible with column {expected:?}")
            }
            CoreError::ColumnOutOfRange { index } => {
                write!(f, "column {index} out of range")
            }
        }
    }
}

impl std::error::Error for CoreError {}

impl From<CoreError> for DbError {
    fn from(e: CoreError) -> Self {
        DbError::internal(e.to_string()).with_category(ErrorCategory::Internal)
    }
}

/// TCP keepalive of every database connection: after this many seconds of
/// silence the operating system sends a probe. It keeps the entries of NATs
/// and firewalls along the way alive (AWS cuts idle TCP connections after
/// 350 s: the classic "connection dropped after 5 minutes") and detects an
/// already dead connection early. The server does not see it as activity.
pub const TCP_KEEPALIVE_IDLE: std::time::Duration = std::time::Duration::from_secs(30);
