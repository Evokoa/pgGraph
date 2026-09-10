//! Capture a closed sync-log prefix without retaining a writer lock afterwards.

use std::cell::Cell;
use std::ffi::{CStr, CString};
use std::panic::AssertUnwindSafe;

use pgrx::{
    datum::DatumWithOid,
    pg_sys,
    spi::{SpiClient, SpiCursor},
};

use crate::safety::{GraphError, GraphResult};

/// The callback must copy every result into Rust-owned values before returning.
pub(crate) fn capture<R>(operation: impl FnOnce() -> GraphResult<R>) -> GraphResult<R> {
    // PostgreSQL stores both identifiers in byte-sized LOCKTAG fields. Validate
    // the generated binding constants before acquiring any backend resources.
    let locktag_type = u8::try_from(pg_sys::LockTagType::LOCKTAG_ADVISORY).map_err(|_| {
        GraphError::Internal("PostgreSQL advisory lock tag does not fit in a byte".into())
    })?;
    let locktag_lockmethodid = u8::try_from(pg_sys::USER_LOCKMETHOD).map_err(|_| {
        GraphError::Internal("PostgreSQL user lock method does not fit in a byte".into())
    })?;
    let tag = pg_sys::LOCKTAG {
        // SAFETY: the current database OID is initialized before any SQL entrypoint.
        locktag_field1: unsafe { pg_sys::MyDatabaseId.to_u32() },
        locktag_field2: crate::sync::SYNC_WRITER_LOCK_CLASS as u32,
        locktag_field3: crate::sync::SYNC_WRITER_LOCK_KEY as u32,
        locktag_field4: 2,
        locktag_type,
        locktag_lockmethodid,
    };
    let held = Cell::new(false);
    let pushed = Cell::new(false);
    pg_sys::PgTryBuilder::new(AssertUnwindSafe(|| {
        // SAFETY: this is PostgreSQL's two-int advisory lock tag. A successful
        // session acquisition adds exactly one local hold, released in finally.
        let acquired =
            unsafe { pg_sys::LockAcquire(&tag, pg_sys::ExclusiveLock as i32, true, true) };
        if acquired == pg_sys::LockAcquireResult::LOCKACQUIRE_NOT_AVAIL {
            return Err(GraphError::BuildLocked);
        }
        held.set(true);
        // SAFETY: capture runs in an active transaction. Incrementing the command
        // counter exposes completed local writes, as writable SPI does. Acquire
        // a fresh READ COMMITTED snapshot after fencing writers; fixed isolation
        // deliberately keeps its transaction snapshot. Push copies/pins it until
        // finally pops it, including PostgreSQL errors and Rust unwinding.
        unsafe {
            pg_sys::CommandCounterIncrement();
            pg_sys::PushActiveSnapshot(pg_sys::GetTransactionSnapshot());
        }
        pushed.set(true);
        operation()
    }))
    .finally(|| {
        if pushed.replace(false) {
            // SAFETY: exactly one matching snapshot push completed above.
            unsafe { pg_sys::PopActiveSnapshot() };
        }
        if held.replace(false) {
            // SAFETY: release only this scope's session hold, without SPI or
            // changing the source writers' transaction-level shared holds.
            unsafe { pg_sys::LockRelease(&tag, pg_sys::ExclusiveLock as i32, true) };
        }
    })
    .execute()
}

/// Open a SELECT on the explicitly pinned capture snapshot regardless of XID.
/// Call only inside `capture`, with the cursor dropped before capture returns.
pub(crate) fn open_cursor<'conn>(
    client: &SpiClient<'conn>,
    sql: &str,
    args: &[DatumWithOid<'_>],
) -> GraphResult<SpiCursor<'conn>> {
    let sql = CString::new(sql)
        .map_err(|_| GraphError::Internal("sync capture query contains NUL".into()))?;
    let count = i32::try_from(args.len())
        .map_err(|_| GraphError::Internal("too many sync capture parameters".into()))?;
    let mut types: Vec<_> = args.iter().map(DatumWithOid::oid).collect();
    let mut values: Vec<_> = args
        .iter()
        .map(|arg| {
            arg.datum()
                .map_or(pg_sys::Datum::from(0usize), |datum| datum.sans_lifetime())
        })
        .collect();
    let nulls: Vec<_> = args
        .iter()
        .map(|arg| (if arg.datum().is_some() { b' ' } else { b'n' }) as std::ffi::c_char)
        .collect();
    // SAFETY: SQL and equally sized parameter arrays live through the call.
    // read_only=true selects our active snapshot rather than allowing pgrx's
    // XID-dependent SPI mode to acquire another one. PostgreSQL copies bound
    // parameters into the portal and raises an error if opening it fails.
    let portal = unsafe {
        pg_sys::SPI_cursor_open_with_args(
            std::ptr::null(),
            sql.as_ptr(),
            count,
            types.as_mut_ptr(),
            values.as_mut_ptr(),
            nulls.as_ptr(),
            true,
            0,
        )
    };
    if portal.is_null() {
        return Err(GraphError::Internal(
            "sync capture cursor was not opened".into(),
        ));
    }
    // SAFETY: the successfully opened portal owns a valid NUL-terminated name.
    let name = unsafe { CStr::from_ptr((*portal).name) }.to_string_lossy();
    client.find_cursor(&name).map_err(|error| {
        GraphError::Internal(format!("sync capture cursor lookup failed: {error}"))
    })
}

#[cfg(all(not(test), feature = "development"))]
pub(crate) fn cancel_during_fetch() -> GraphResult<()> {
    capture(|| {
        pgrx::Spi::connect(|client| {
            let mut cursor = open_cursor(
                client,
                "SELECT pg_cancel_backend(pg_backend_pid()), pg_sleep(10)",
                &[],
            )?;
            cursor.fetch(1).map_err(|error| {
                GraphError::Internal(format!("capture cancellation fetch: {error}"))
            })?;
            Ok(())
        })
    })
}
