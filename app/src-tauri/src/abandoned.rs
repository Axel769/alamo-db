//! Server connections left behind by a closed session (architecture §3.2).
//! After a sleep the network or the SSH tunnel is dead: closing the session
//! never reaches the server, which keeps its connections until
//! `wait_timeout` (8 hours by default), counted against the user's limit.
//! With 3 connections allowed, Disconnect + Connect after a sleep left one
//! free, and the next sleep none. They are kept here, per profile, and the
//! next connect of the profile closes them (`Session::kill_abandoned`).

use alamo_session::driver::ServerConnId;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::storage;

/// Beyond this the server has closed them by itself (`wait_timeout`) and
/// an old id is only a risk: it is forgotten.
const KEEP_SECS: i64 = 24 * 3600;

#[derive(Serialize, Deserialize)]
struct Left {
    /// When the session ended, Unix seconds.
    at: i64,
    conn: ServerConnId,
}

fn key(profile_id: i64) -> String {
    format!("abandoned:{profile_id}")
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The ones still worth closing at `at`. Unreadable = none: at worst they
/// expire on the server.
fn read(db: &Connection, profile_id: i64, at: i64) -> rusqlite::Result<Vec<Left>> {
    let raw = storage::kv_get(db, &key(profile_id))?;
    let all: Vec<Left> = raw
        .and_then(|r| serde_json::from_str(&r).ok())
        .unwrap_or_default();
    Ok(all.into_iter().filter(|l| at - l.at <= KEEP_SECS).collect())
}

/// A session of the profile ended: its connections are kept.
pub fn remember(
    db: &Connection,
    profile_id: i64,
    conns: Vec<ServerConnId>,
) -> rusqlite::Result<()> {
    remember_at(db, profile_id, conns, now())
}

fn remember_at(
    db: &Connection,
    profile_id: i64,
    conns: Vec<ServerConnId>,
    at: i64,
) -> rusqlite::Result<()> {
    if conns.is_empty() {
        return Ok(());
    }
    let mut all = read(db, profile_id, at)?;
    all.extend(conns.into_iter().map(|conn| Left { at, conn }));
    let json = serde_json::to_string(&all).expect("serializable");
    storage::kv_set(db, &key(profile_id), &json)
}

/// For the session just connected: the connections to close, forgotten here.
pub fn take(db: &Connection, profile_id: i64) -> rusqlite::Result<Vec<ServerConnId>> {
    take_at(db, profile_id, now())
}

fn take_at(db: &Connection, profile_id: i64, at: i64) -> rusqlite::Result<Vec<ServerConnId>> {
    let left = read(db, profile_id, at)?;
    forget(db, profile_id)?;
    Ok(left.into_iter().map(|l| l.conn).collect())
}

/// The profile is deleted: its ids must not reach a new profile that
/// reuses the same SQLite rowid.
pub fn forget(db: &Connection, profile_id: i64) -> rusqlite::Result<()> {
    storage::kv_delete(db, &key(profile_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(id: u32) -> ServerConnId {
        ServerConnId {
            server: "uuid".into(),
            start: 1000,
            id,
        }
    }

    fn mem() -> Connection {
        let db = Connection::open_in_memory().unwrap();
        storage::migrate(&db).unwrap();
        db
    }

    #[test]
    fn kept_per_profile_and_taken_once() {
        let db = mem();
        remember_at(&db, 1, vec![conn(5), conn(6)], 100).unwrap();
        remember_at(&db, 1, vec![conn(9)], 200).unwrap();
        remember_at(&db, 2, vec![conn(7)], 200).unwrap();
        assert_eq!(
            take_at(&db, 1, 300).unwrap(),
            vec![conn(5), conn(6), conn(9)]
        );
        assert!(take_at(&db, 1, 300).unwrap().is_empty(), "taken once");
        assert_eq!(take_at(&db, 2, 300).unwrap(), vec![conn(7)]);
    }

    #[test]
    fn expired_ones_are_dropped() {
        let db = mem();
        remember_at(&db, 1, vec![conn(5)], 100).unwrap();
        remember_at(&db, 1, vec![conn(6)], 100 + KEEP_SECS).unwrap();
        assert_eq!(take_at(&db, 1, 101 + KEEP_SECS).unwrap(), vec![conn(6)]);
    }

    #[test]
    fn deleted_profile_leaves_nothing() {
        let db = mem();
        remember_at(&db, 1, vec![conn(5)], 100).unwrap();
        forget(&db, 1).unwrap();
        assert!(take_at(&db, 1, 100).unwrap().is_empty());
    }
}
