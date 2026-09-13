#![allow(dead_code, non_upper_case_globals, improper_ctypes_definitions)]

//! Standalone PostgreSQL symbols for projection loader fuzz targets.
//!
//! cargo-fuzz links the graph rlib into standalone binaries. That rlib carries
//! PostgreSQL backend symbols from pgrx extension modules even when the selected
//! fuzz target only calls pgrx-free projection loader wrappers. These stubs let
//! those binaries load outside a backend. Function stubs abort on use; global
//! stubs exist only to satisfy symbol relocation.

use std::ffi::{c_char, c_int, c_uint, c_void};

// PG17 declares MyDatabaseId as Oid (u32) and XactIsoLevel as C int.
#[no_mangle]
pub static mut MyDatabaseId: u32 = 0;
#[no_mangle]
pub static mut XactIsoLevel: std::ffi::c_int = 0;

// PG17 declares IsUnderPostmaster as bool. Standalone loaders must not enter
// PostgreSQL descriptor accounting. This symbol is linked only by fuzz targets.
#[no_mangle]
pub static mut IsUnderPostmaster: bool = false;

// Match the PG17 ABI for backend functions retained by the graph rlib. The
// projection parsers never call these; abort if a target crosses that boundary.
// Oid is u32, AclMode is u64, AclResult is C unsigned int, and LOCKMODE is C int.
#[no_mangle]
pub extern "C" fn superuser_arg(_role_id: u32) -> bool {
    std::process::abort();
}

#[no_mangle]
pub extern "C" fn check_enable_rls(_relation_id: u32, _role_id: u32, _no_error: bool) -> c_int {
    std::process::abort();
}

#[no_mangle]
pub extern "C" fn pg_parameter_aclcheck(_name: *const c_char, _role_id: u32, _mode: u64) -> c_uint {
    std::process::abort();
}

#[no_mangle]
pub extern "C" fn LockRelationOid(_relation_id: u32, _lock_mode: c_int) {
    std::process::abort();
}

#[no_mangle]
pub extern "C" fn AcquireExternalFD() -> bool {
    std::process::abort();
}

#[no_mangle]
pub extern "C" fn ReleaseExternalFD() {
    std::process::abort();
}

macro_rules! pg_stub_fn {
    ($($name:ident),+ $(,)?) => {
        $(
            #[no_mangle]
            pub extern "C" fn $name() -> *mut c_void {
                std::process::abort();
            }
        )+
    };
}

macro_rules! pg_stub_ptr {
    ($($name:ident),+ $(,)?) => {
        $(
            #[no_mangle]
            pub static mut $name: *mut c_void = std::ptr::null_mut();
        )+
    };
}

macro_rules! pg_stub_int {
    ($($name:ident),+ $(,)?) => {
        $(
            #[no_mangle]
            pub static mut $name: i32 = 0;
        )+
    };
}

pg_stub_ptr!(
    CacheMemoryContext,
    CurTransactionContext,
    CurrentMemoryContext,
    DataDir,
    ErrorContext,
    MessageContext,
    MyBgworkerEntry,
    MyLatch,
    PG_exception_stack,
    PortalContext,
    PostmasterContext,
    TopMemoryContext,
    TopTransactionContext,
    error_context_stack,
    shmem_startup_hook,
);

pg_stub_int!(
    ConfigReloadPending,
    InterruptHoldoffCount,
    InterruptPending,
    MyProcPid,
    ShutdownRequestPending,
);

pg_stub_fn!(
    AllocSetContextCreateInternal,
    BackgroundWorkerInitializeConnection,
    BackgroundWorkerInitializeConnectionByOid,
    BackgroundWorkerUnblockSignals,
    BlessTupleDesc,
    CommandCounterIncrement,
    CommitTransactionCommand,
    CopyErrorData,
    CreateTupleDescCopyConstr,
    DecrTupleDescRefCount,
    DefineCustomBoolVariable,
    DefineCustomIntVariable,
    DefineCustomRealVariable,
    DefineCustomStringVariable,
    ExceptionalCondition,
    FlushErrorState,
    FreeErrorData,
    GUC_check_errcode,
    GUC_check_errdetail_string,
    GUC_check_errhint_string,
    GUC_check_errmsg_string,
    GetBackgroundWorkerPid,
    GetCurrentStatementStartTimestamp,
    GetCurrentTimestamp,
    GetCurrentTransactionId,
    GetCurrentTransactionIdIfAny,
    GetCurrentTransactionNestLevel,
    GetCurrentTransactionStartTimestamp,
    GetDatabaseEncoding,
    GetMemoryChunkContext,
    GetOldestNonRemovableTransactionId,
    GetOuterUserId,
    GetSQLCurrentTimestamp,
    GetSQLLocalTimestamp,
    GetTransactionSnapshot,
    GetUserId,
    HeapTupleHeaderGetDatum,
    IsBinaryCoercible,
    LWLockRelease,
    LookupFuncName,
    LockAcquire,
    LockRelease,
    MemoryContextAlloc,
    MemoryContextAllocExtended,
    MemoryContextAllocZero,
    MemoryContextDelete,
    MemoryContextGetParent,
    MemoryContextRegisterResetCallback,
    MemoryContextReset,
    MemoryContextStrdup,
    OpernameGetOprid,
    ProcessInterrupts,
    PopActiveSnapshot,
    PushActiveSnapshot,
    RegisterBackgroundWorker,
    RegisterDynamicBackgroundWorker,
    RegisterSubXactCallback,
    RegisterXactCallback,
    RelationClose,
    RelationGetIndexList,
    RelationIdGetRelation,
    ReleaseSysCache,
    ResetLatch,
    SPI_connect,
    SPI_cursor_close,
    SPI_cursor_fetch,
    SPI_cursor_find,
    SPI_cursor_open,
    SPI_cursor_open_with_args,
    SPI_execute,
    SPI_execute_plan,
    SPI_execute_with_args,
    SPI_finish,
    SPI_fname,
    SPI_fnumber,
    SPI_freeplan,
    SPI_getargcount,
    SPI_getbinval,
    SPI_gettypeid,
    SPI_keepplan,
    SPI_prepare,
    SPI_processed,
    SPI_result,
    SPI_tuptable,
    SearchSysCache,
    SearchSysCache1,
    SetLatch,
    SetCurrentStatementStartTimestamp,
    StartTransactionCommand,
    SysCacheGetAttr,
    TerminateBackgroundWorker,
    TransactionIdPrecedesOrEquals,
    UnregisterSubXactCallback,
    WaitForBackgroundWorkerShutdown,
    WaitForBackgroundWorkerStartup,
    WaitLatch,
    accumArrayResult,
    appendBinaryStringInfo,
    array_contains_nulls,
    date_cmp,
    date_eq,
    date_in,
    date_mi_interval,
    date_mii,
    date_out,
    date_pl_interval,
    date_pli,
    date_timestamp,
    date_timestamptz,
    datetime_timestamp,
    datetimetz_timestamptz,
    end_MultiFuncCall,
    enlargeStringInfo,
    errcode,
    errdetail,
    errfinish,
    errhint,
    errmsg,
    errstart,
    eval_const_expressions,
    expression_tree_walker_impl,
    extract_date,
    extract_time,
    extract_timestamp,
    extract_timestamptz,
    extract_timetz,
    float8_timestamptz,
    format_type_extended,
    get_array_type,
    get_call_result_type,
    get_element_type,
    get_fn_expr_argtype,
    get_namespace_name,
    get_typlenbyvalalign,
    heap_copy_tuple_as_datum,
    heap_copytuple,
    heap_form_tuple,
    heap_modify_tuple,
    inet_in,
    inet_out,
    initArrayResult,
    init_MultiFuncCall,
    int4_numeric,
    interval_cmp,
    interval_div,
    interval_eq,
    interval_in,
    interval_justify_days,
    interval_justify_hours,
    interval_justify_interval,
    interval_mi,
    interval_mul,
    interval_out,
    interval_pl,
    interval_time,
    interval_trunc,
    interval_um,
    jsonb_in,
    jsonb_out,
    lappend,
    list_free,
    lookup_rowtype_tupdesc,
    lookup_rowtype_tupdesc_copy,
    lookup_type_cache,
    makeArrayResult,
    makeString,
    makeStringInfo,
    make_date,
    make_interval,
    make_time,
    make_timestamp,
    make_timestamptz,
    message_level_is_interesting,
    nodeToString,
    numeric_abs,
    numeric_add,
    numeric_ceil,
    numeric_cmp,
    numeric_div,
    numeric_exp,
    numeric_float8,
    numeric_floor,
    numeric_gcd,
    numeric_int2,
    numeric_int4,
    numeric_int8,
    numeric_is_nan,
    numeric_log,
    numeric_mod,
    numeric_mul,
    numeric_normalize,
    numeric_out,
    numeric_sqrt,
    numeric_sub,
    palloc,
    palloc0,
    parse_ident,
    pfree,
    pg_class_aclcheck,
    pg_detoast_datum,
    pg_detoast_datum_copy,
    pg_detoast_datum_packed,
    pgstat_assoc_relation,
    planstate_tree_walker_impl,
    pqsignal,
    pstrdup,
    query_or_expression_tree_walker_impl,
    query_tree_walker_impl,
    quote_identifier,
    quote_literal_cstr,
    range_table_entry_walker_impl,
    range_table_walker_impl,
    raw_expression_tree_walker_impl,
    regtypein,
    relation_close,
    relation_open,
    repalloc,
    stringToNode,
    time_cmp,
    time_eq,
    time_in,
    time_interval,
    time_mi_interval,
    time_mi_time,
    time_out,
    time_pl_interval,
    time_timetz,
    timeofday,
    timestamp_age,
    timestamp_cmp,
    timestamp_date,
    timestamp_eq,
    timestamp_in,
    timestamp_mi,
    timestamp_mi_interval,
    timestamp_out,
    timestamp_pl_interval,
    timestamp_time,
    timestamp_timestamptz,
    timestamp_trunc,
    timestamptz_date,
    timestamptz_in,
    timestamptz_mi_interval,
    timestamptz_out,
    timestamptz_pl_interval,
    timestamptz_timestamp,
    timestamptz_timetz,
    timestamptz_trunc,
    timestamptz_zone,
    timetz_cmp,
    timetz_eq,
    timetz_in,
    timetz_izone,
    timetz_mi_interval,
    timetz_out,
    timetz_pl_interval,
    timetz_time,
    timetz_zone,
    to_regclass,
);

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn backend_function_stub_aborts_instead_of_returning() {
        const CHILD: &str = "PGGRAPH_FUZZ_STUB_ABORT_CHILD";
        if let Some(symbol) = std::env::var_os(CHILD) {
            match symbol.to_str().expect("stub symbol") {
                "GetTransactionSnapshot" => {
                    super::GetTransactionSnapshot();
                }
                "superuser_arg" => {
                    super::superuser_arg(0);
                }
                "check_enable_rls" => {
                    super::check_enable_rls(0, 0, false);
                }
                "pg_parameter_aclcheck" => {
                    super::pg_parameter_aclcheck(c"graph.rls_mode".as_ptr(), 0, 0);
                }
                "LockRelationOid" => super::LockRelationOid(0, 1),
                "AcquireExternalFD" => {
                    super::AcquireExternalFD();
                }
                "ReleaseExternalFD" => super::ReleaseExternalFD(),
                other => panic!("unknown stub symbol: {other}"),
            }
            panic!("PostgreSQL stub returned instead of aborting");
        }
        for symbol in [
            "GetTransactionSnapshot",
            "superuser_arg",
            "check_enable_rls",
            "pg_parameter_aclcheck",
            "LockRelationOid",
            "AcquireExternalFD",
            "ReleaseExternalFD",
        ] {
            let status =
                std::process::Command::new(std::env::current_exe().expect("test executable"))
                    .args([
                        "--exact",
                        "tests::backend_function_stub_aborts_instead_of_returning",
                    ])
                    .env(CHILD, symbol)
                    .status()
                    .expect("start abort probe");
            // SIGABRT is 6 on the supported Linux and macOS fuzz hosts.
            assert_eq!(status.signal(), Some(6), "stub {symbol} must abort");
        }
    }
}
