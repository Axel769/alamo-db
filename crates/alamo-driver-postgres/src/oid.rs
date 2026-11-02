//! OIDs of the PostgreSQL types.
//!
//! They are stable `pg_type` numbers for the built-in types. Custom types
//! (domains, enums, composites) have OIDs assigned per database: they cannot
//! live here, and indeed they all end up in the `Utf8` fallback of §4.4.

pub const BOOL: u32 = 16;
pub const BYTEA: u32 = 17;
pub const CHAR: u32 = 18;
pub const NAME: u32 = 19;
pub const INT8: u32 = 20;
pub const INT2: u32 = 21;
pub const INT4: u32 = 23;
pub const TEXT: u32 = 25;
pub const OID: u32 = 26;
pub const JSON: u32 = 114;
pub const XML: u32 = 142;
pub const FLOAT4: u32 = 700;
pub const FLOAT8: u32 = 701;
pub const MONEY: u32 = 790;
pub const INET: u32 = 869;
pub const CIDR: u32 = 650;
pub const MACADDR: u32 = 829;
pub const MACADDR8: u32 = 774;
pub const BPCHAR: u32 = 1042;
pub const VARCHAR: u32 = 1043;
pub const DATE: u32 = 1082;
pub const TIME: u32 = 1083;
pub const TIMESTAMP: u32 = 1114;
pub const TIMESTAMPTZ: u32 = 1184;
pub const INTERVAL: u32 = 1186;
pub const TIMETZ: u32 = 1266;
pub const BIT: u32 = 1560;
pub const VARBIT: u32 = 1562;
pub const NUMERIC: u32 = 1700;
pub const UUID: u32 = 2950;
pub const JSONB: u32 = 3802;

// Arrays. Represented as `Utf8` (§4.4) but with a dedicated decoder, because
// the binary format of an array is not text.
pub const BOOL_ARRAY: u32 = 1000;
pub const BYTEA_ARRAY: u32 = 1001;
pub const CHAR_ARRAY: u32 = 1002;
pub const NAME_ARRAY: u32 = 1003;
pub const INT2_ARRAY: u32 = 1005;
pub const INT4_ARRAY: u32 = 1007;
pub const TEXT_ARRAY: u32 = 1009;
pub const VARCHAR_ARRAY: u32 = 1015;
pub const INT8_ARRAY: u32 = 1016;
pub const FLOAT4_ARRAY: u32 = 1021;
pub const FLOAT8_ARRAY: u32 = 1022;
pub const NUMERIC_ARRAY: u32 = 1231;
pub const TIMESTAMP_ARRAY: u32 = 1115;
pub const DATE_ARRAY: u32 = 1182;
pub const TIME_ARRAY: u32 = 1183;
pub const TIMESTAMPTZ_ARRAY: u32 = 1185;
pub const UUID_ARRAY: u32 = 2951;
pub const JSON_ARRAY: u32 = 199;
pub const JSONB_ARRAY: u32 = 3807;
pub const INET_ARRAY: u32 = 1041;
pub const CIDR_ARRAY: u32 = 651;
pub const MACADDR_ARRAY: u32 = 1040;

/// True for the OIDs this driver knows how to render as text. The others are
/// marked `undecoded_wire_format` in the schema.
pub fn is_array(oid: u32) -> bool {
    matches!(
        oid,
        BOOL_ARRAY
            | BYTEA_ARRAY
            | CHAR_ARRAY
            | NAME_ARRAY
            | INT2_ARRAY
            | INT4_ARRAY
            | TEXT_ARRAY
            | VARCHAR_ARRAY
            | INT8_ARRAY
            | FLOAT4_ARRAY
            | FLOAT8_ARRAY
            | NUMERIC_ARRAY
            | TIMESTAMP_ARRAY
            | DATE_ARRAY
            | TIME_ARRAY
            | TIMESTAMPTZ_ARRAY
            | UUID_ARRAY
            | JSON_ARRAY
            | JSONB_ARRAY
            | INET_ARRAY
            | CIDR_ARRAY
            | MACADDR_ARRAY
    )
}

/// Days between the Unix epoch and PostgreSQL's internal epoch (2000-01-01).
pub const PG_EPOCH_DAYS: i32 = 10_957;
/// Microseconds between the Unix epoch and PostgreSQL's internal epoch.
pub const PG_EPOCH_MICROS: i64 = 946_684_800_000_000;
