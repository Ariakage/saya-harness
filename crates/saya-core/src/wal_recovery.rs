//! Read a stranded SQLite WAL into an in-memory preflight database without
//! creating files. The byte order and rolling checksums follow SQLite's WAL
//! format: https://www.sqlite.org/fileformat2.html#walformat
use crate::contract::{BoundaryError, ErrorInfo};
use rusqlite::{Connection, MAIN_DB};
use sha2::{Digest, Sha256};
use std::{fs::OpenOptions, io::Read, os::unix::fs::OpenOptionsExt, path::Path, sync::Arc};

// This is only a fallback for sessions created before guarded preflight. New
// sessions use the small durable guard and never need to copy the database.
const MAX_SNAPSHOT: u64 = 512 * 1024 * 1024;
pub(crate) type SnapshotCheck = Arc<dyn Fn() -> Result<(), BoundaryError> + Send + Sync>;
fn fail(code: &str, message: &str) -> BoundaryError {
    BoundaryError(
        ErrorInfo {
            code: code.into(),
            message: message.into(),
        },
        None,
        None,
    )
}
fn read_file(
    path: &Path,
    max_bytes: u64,
    check: &dyn Fn() -> Result<(), BoundaryError>,
) -> Result<Vec<u8>, BoundaryError> {
    check()?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| fail("storage_busy", "cannot read stranded database snapshot"))?;
    let metadata = file
        .metadata()
        .map_err(|_| fail("storage_error", "cannot inspect stranded database snapshot"))?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(fail(
            "storage_error",
            "stranded database snapshot is too large or not a regular file",
        ));
    }
    let mut data = Vec::new();
    let mut input = file.take(max_bytes + 1);
    let mut buffer = [0u8; 65_536];
    loop {
        check()?;
        let count = input
            .read(&mut buffer)
            .map_err(|_| fail("storage_error", "cannot read database snapshot"))?;
        if count == 0 {
            break;
        }
        data.extend_from_slice(&buffer[..count]);
    }
    if data.len() as u64 > max_bytes {
        return Err(fail(
            "storage_error",
            "stranded database snapshot is too large",
        ));
    }
    Ok(data)
}
fn hash_file(
    path: &Path,
    max_bytes: u64,
    check: &dyn Fn() -> Result<(), BoundaryError>,
) -> Result<sha2::digest::Output<Sha256>, BoundaryError> {
    check()?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| fail("storage_busy", "cannot read stranded database snapshot"))?;
    let metadata = file
        .metadata()
        .map_err(|_| fail("storage_error", "cannot inspect stranded database snapshot"))?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(fail(
            "storage_error",
            "stranded database snapshot is too large or not a regular file",
        ));
    }
    let mut input = file.take(max_bytes + 1);
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 65_536];
    let mut total = 0u64;
    loop {
        check()?;
        let count = input
            .read(&mut buffer)
            .map_err(|_| fail("storage_error", "cannot read stranded database snapshot"))?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > max_bytes {
            return Err(fail(
                "storage_error",
                "stranded database snapshot is too large",
            ));
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize())
}
fn be(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}
fn checksum(bytes: &[u8], big: bool, mut state: (u32, u32)) -> (u32, u32) {
    for block in bytes.as_chunks::<8>().0 {
        let a = [block[0], block[1], block[2], block[3]];
        let b = [block[4], block[5], block[6], block[7]];
        let (a, b) = if big {
            (u32::from_be_bytes(a), u32::from_be_bytes(b))
        } else {
            (u32::from_le_bytes(a), u32::from_le_bytes(b))
        };
        state.0 = state.0.wrapping_add(a).wrapping_add(state.1);
        state.1 = state.1.wrapping_add(b).wrapping_add(state.0);
    }
    state
}
fn build_image(
    mut db: Vec<u8>,
    wal: &[u8],
    max_bytes: u64,
    check: &dyn Fn() -> Result<(), BoundaryError>,
) -> Result<Vec<u8>, BoundaryError> {
    check()?;
    if db.len() as u64 > max_bytes {
        return Err(fail(
            "snapshot_limit",
            "database image exceeds snapshot source limit",
        ));
    }
    if db.len() < 100 || &db[..16] != b"SQLite format 3\0" || wal.len() < 32 {
        return Err(fail(
            "storage_corrupt",
            "invalid stranded database or WAL header",
        ));
    }
    let raw_page = u16::from_be_bytes([db[16], db[17]]);
    let page = if raw_page == 1 {
        65_536usize
    } else {
        usize::from(raw_page)
    };
    let magic = be(&wal[0..4]);
    if !(magic == 0x377f0682 || magic == 0x377f0683)
        || be(&wal[4..8]) != 3_007_000
        || !page.is_power_of_two()
        || !(512..=65_536).contains(&page)
        || be(&wal[8..12]) as usize != page
        || !db.len().is_multiple_of(page)
    {
        return Err(fail("storage_corrupt", "invalid stranded WAL format"));
    }
    let big = magic & 1 == 1;
    let mut sum = checksum(&wal[..24], big, (0, 0));
    if sum != (be(&wal[24..28]), be(&wal[28..32])) {
        return Err(fail("storage_corrupt", "invalid stranded WAL checksum"));
    }
    let frame_size = 24usize + page;
    let mut frames = Vec::new();
    let mut committed = None;
    for frame in wal[32..].chunks_exact(frame_size) {
        check()?;
        if frame[8..16] != wal[16..24] {
            break;
        }
        let page_no = be(&frame[0..4]);
        if page_no == 0 {
            return Err(fail("storage_corrupt", "invalid stranded WAL page"));
        }
        sum = checksum(&frame[..8], big, sum);
        sum = checksum(&frame[24..], big, sum);
        if sum != (be(&frame[16..20]), be(&frame[20..24])) {
            return Err(fail(
                "storage_corrupt",
                "invalid stranded WAL frame checksum",
            ));
        }
        let pages = be(&frame[4..8]);
        frames.push((page_no, &frame[24..]));
        if pages != 0 {
            committed = Some((frames.len(), pages));
        }
    }
    if let Some((count, pages)) = committed {
        let size = (pages as usize)
            .checked_mul(page)
            .filter(|size| *size as u64 <= max_bytes)
            .ok_or_else(|| {
                fail(
                    "snapshot_limit",
                    "database image exceeds snapshot source limit",
                )
            })?;
        check()?;
        db.resize(size, 0);
        for (number, contents) in frames.into_iter().take(count) {
            check()?;
            if number > pages {
                continue;
            }
            let start = (number as usize - 1) * page;
            db[start..start + page].copy_from_slice(contents);
        }
    }
    if db.len() < 100 || &db[..16] != b"SQLite format 3\0" {
        return Err(fail("storage_corrupt", "invalid stranded database image"));
    }
    // The in-memory image already contains the WAL's committed pages. Disable
    // WAL mode in this private copy so SQLite does not try to open side files.
    db[18] = 1;
    db[19] = 1;
    Ok(db)
}
/// Obtain a stable, checksum-verified in-memory SQLite image. No filesystem
/// writes or SQLite -shm creation occur, including for interrupted WALs.
pub(crate) fn snapshot(root: &Path) -> Result<Connection, BoundaryError> {
    snapshot_checked(root, MAX_SNAPSHOT, Arc::new(|| Ok(())))
}

/// Stable source image with cooperative checks during I/O. Supports an empty
/// or absent WAL, and never opens SQLite against the source files.
pub(crate) fn snapshot_checked(
    root: &Path,
    max_bytes: u64,
    check: SnapshotCheck,
) -> Result<Connection, BoundaryError> {
    let db_path = root.join("sessions.sqlite3");
    let wal_path = root.join("sessions.sqlite3-wal");
    let wal_bytes = || -> Result<Vec<u8>, BoundaryError> {
        match std::fs::symlink_metadata(&wal_path) {
            Ok(_) => read_file(&wal_path, max_bytes, check.as_ref()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(_) => Err(fail("storage_error", "cannot inspect database WAL")),
        }
    };
    for _ in 0..3 {
        check()?;
        let db = read_file(&db_path, max_bytes, check.as_ref())?;
        let wal = wal_bytes()?;
        if (db.len() as u64).saturating_add(wal.len() as u64) > max_bytes {
            return Err(fail(
                "snapshot_limit",
                "database and WAL exceed snapshot source limit",
            ));
        }
        let db_hash = Sha256::digest(&db);
        let wal_hash = Sha256::digest(&wal);
        if db_hash != hash_file(&db_path, max_bytes, check.as_ref())?
            || wal_hash != Sha256::digest(wal_bytes()?)
        {
            continue;
        }
        let image = if wal.is_empty() {
            let mut db = db;
            if db.len() < 100 || &db[..16] != b"SQLite format 3\0" {
                return Err(fail("storage_corrupt", "invalid database snapshot header"));
            }
            db[18] = 1;
            db[19] = 1;
            db
        } else {
            build_image(db, &wal, max_bytes, check.as_ref())?
        };
        check()?;
        let mut conn = Connection::open_in_memory()
            .map_err(|_| fail("storage_error", "cannot open in-memory database snapshot"))?;
        conn.deserialize_read_exact(MAIN_DB, image.as_slice(), image.len(), true)
            .map_err(|_| fail("storage_corrupt", "cannot read database snapshot"))?;
        let vm_check = check.clone();
        conn.progress_handler(1000, Some(move || vm_check().is_err()));
        let result = conn.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0));
        check()?;
        let result = result.map_err(|_| fail("storage_corrupt", "invalid database snapshot"))?;
        if result != "ok" {
            return Err(fail("storage_corrupt", "invalid database snapshot"));
        }
        return Ok(conn);
    }
    Err(fail(
        "storage_busy",
        "database changed during read-only snapshot",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        boundary::{Store, StoreChange},
        contract::{HistoryItem, ModelProtocol, Session, SessionId, WorkMode},
        sqlite::SqliteStore,
    };

    #[tokio::test]
    async fn validates_wal_checksums_byte_orders_and_uncommitted_tails()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::TempDir::new()?;
        let store = SqliteStore::open(temp.path(), vec![])?;
        let session = Session {
            id: SessionId::new(),
            workspace: temp.path().into(),
            work_mode: WorkMode::Work,
            profile: "test".into(),
            protocol: ModelProtocol::ChatCompletions,
            model: "fake".into(),
            history: vec![],
        };
        store.create_session(&session, "hash").await?;
        store
            .commit(&[StoreChange::History {
                session_id: session.id,
                item: HistoryItem::UserText("committed".into()),
            }])
            .await?;
        let db = std::fs::read(temp.path().join("sessions.sqlite3"))?;
        let wal = std::fs::read(temp.path().join("sessions.sqlite3-wal"))?;
        let image = build_image(db.clone(), &wal, MAX_SNAPSHOT, &|| Ok(()))?;
        let mut conn = Connection::open_in_memory()?;
        conn.deserialize_read_exact(MAIN_DB, image.as_slice(), image.len(), true)?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM messages WHERE session_id=?1",
            [session.id.0.to_string()],
            |r| r.get(0),
        )?;
        assert_eq!(count, 1);
        let mut partial = wal.clone();
        partial.extend_from_slice(&[0x42; 13]);
        assert_eq!(
            build_image(db.clone(), &partial, MAX_SNAPSHOT, &|| Ok(()))?,
            image
        );
        let mut bad = wal.clone();
        let frame = 32 + 24;
        bad[frame] ^= 1;
        assert_eq!(
            build_image(db.clone(), &bad, MAX_SNAPSHOT, &|| Ok(()))
                .expect_err("bad checksum")
                .0
                .code,
            "storage_corrupt"
        );
        let mut big = wal.clone();
        big[..4].copy_from_slice(&0x377f0683u32.to_be_bytes());
        let mut sum = checksum(&big[..24], true, (0, 0));
        big[24..28].copy_from_slice(&sum.0.to_be_bytes());
        big[28..32].copy_from_slice(&sum.1.to_be_bytes());
        let size = be(&big[8..12]) as usize + 24;
        for frame in big[32..].chunks_exact_mut(size) {
            if frame[8..16] != wal[16..24] {
                break;
            }
            sum = checksum(&frame[..8], true, sum);
            sum = checksum(&frame[24..], true, sum);
            frame[16..20].copy_from_slice(&sum.0.to_be_bytes());
            frame[20..24].copy_from_slice(&sum.1.to_be_bytes());
        }
        assert_eq!(build_image(db, &big, MAX_SNAPSHOT, &|| Ok(()))?, image);
        Ok(())
    }

    #[test]
    fn wal_image_growth_obeys_the_callers_limit_before_allocation()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut db = vec![0u8; 512];
        db[..16].copy_from_slice(b"SQLite format 3\0");
        db[16..18].copy_from_slice(&512u16.to_be_bytes());
        let mut wal = vec![0u8; 32 + 24 + 512];
        wal[..4].copy_from_slice(&0x377f0682u32.to_be_bytes());
        wal[4..8].copy_from_slice(&3_007_000u32.to_be_bytes());
        wal[8..12].copy_from_slice(&512u32.to_be_bytes());
        let mut sum = checksum(&wal[..24], false, (0, 0));
        wal[24..28].copy_from_slice(&sum.0.to_be_bytes());
        wal[28..32].copy_from_slice(&sum.1.to_be_bytes());
        wal[32..36].copy_from_slice(&1u32.to_be_bytes());
        wal[36..40].copy_from_slice(&8u32.to_be_bytes());
        wal[56..].copy_from_slice(&db);
        sum = checksum(&wal[32..40], false, sum);
        sum = checksum(&wal[56..], false, sum);
        wal[48..52].copy_from_slice(&sum.0.to_be_bytes());
        wal[52..56].copy_from_slice(&sum.1.to_be_bytes());
        assert!(db.len() + wal.len() < 2048);
        assert_eq!(
            build_image(db, &wal, 2048, &|| Ok(())).unwrap_err().0.code,
            "snapshot_limit"
        );
        Ok(())
    }

    #[tokio::test]
    async fn wal_frames_and_sqlite_vm_observe_cancellation()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let temp = tempfile::tempdir()?;
        let store = SqliteStore::open(temp.path(), vec![])?;
        let session = Session {
            id: SessionId::new(),
            workspace: temp.path().into(),
            work_mode: WorkMode::Work,
            profile: "test".into(),
            protocol: ModelProtocol::ChatCompletions,
            model: "fake".into(),
            history: vec![],
        };
        store.create_session(&session, "hash").await?;
        let db = std::fs::read(temp.path().join("sessions.sqlite3"))?;
        let wal = std::fs::read(temp.path().join("sessions.sqlite3-wal"))?;
        let checks = AtomicUsize::new(0);
        let check = || {
            if checks.fetch_add(1, Ordering::SeqCst) >= 1 {
                Err(fail("cancelled", "cancel while checking frames"))
            } else {
                Ok(())
            }
        };
        assert_eq!(
            build_image(db, &wal, MAX_SNAPSHOT, &check)
                .unwrap_err()
                .0
                .code,
            "cancelled"
        );
        assert!(checks.load(Ordering::SeqCst) > 1);

        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let conn = snapshot_checked(
            temp.path(),
            MAX_SNAPSHOT,
            Arc::new(move || {
                if flag.load(Ordering::SeqCst) {
                    Err(fail("cancelled", "cancel during SQLite execution"))
                } else {
                    Ok(())
                }
            }),
        )?;
        cancelled.store(true, Ordering::SeqCst);
        let error = conn.query_row(
            "WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x<1000000) SELECT sum(x) FROM n",
            [], |r| r.get::<_, i64>(0),
        ).unwrap_err();
        assert_eq!(
            error.sqlite_error_code(),
            Some(rusqlite::ErrorCode::OperationInterrupted)
        );
        Ok(())
    }

    #[test]
    fn oversized_legacy_snapshot_fails_without_reading_its_contents()
    -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::TempDir::new()?;
        let file = std::fs::File::create(temp.path().join("sessions.sqlite3"))?;
        file.set_len(MAX_SNAPSHOT + 1)?;
        std::fs::write(temp.path().join("sessions.sqlite3-wal"), b"unused")?;
        assert_eq!(
            snapshot(temp.path())
                .expect_err("oversized snapshot")
                .0
                .code,
            "storage_error"
        );
        Ok(())
    }
}
