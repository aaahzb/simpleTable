use super::*;
use crate::history::{FixedPointKind, UndoPolicy};
use crate::ram_load::{LoadEvent, LoadOptions};
use std::path::{Path, PathBuf};
use std::sync::Condvar;
use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn memory_doc() -> BigFile {
    BigFile::from_bytes(b"a,b\nx,y\n".to_vec(), false).unwrap()
}

fn fixture(label: &str, bytes: &[u8]) -> PathBuf {
    let root = std::env::current_dir()
        .unwrap()
        .join("target/task7-runtime-fixtures");
    fs::create_dir_all(&root).unwrap();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let sequence = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let path = root.join(format!(
        "{}-{nonce}-{sequence}-{label}.csv",
        std::process::id()
    ));
    fs::write(&path, bytes).unwrap();
    path
}

fn register_disk(runtime: &SharedRuntime, owner: &str, path: &Path) -> BigSession {
    register_new_document(
        runtime,
        owner,
        BigFile::open(path).unwrap(),
        Some(path.to_path_buf()),
        None,
        None,
    )
    .unwrap()
}

fn register_memory(runtime: &SharedRuntime, owner: &str) -> BigSession {
    register_new_document(runtime, owner, memory_doc(), None, None, None).unwrap()
}

fn policy_of(runtime: &SharedRuntime, owner: &str, session: &BigSession) -> UndoPolicy {
    authorize_big(runtime, owner, session.session, session.lease, Access::Read)
        .unwrap()
        .file
        .lock()
        .unwrap()
        .undo_policy()
}

fn enter_for_test(
    runtime: &SharedRuntime,
    owner: &str,
    session: &BigSession,
) -> MemoryModeOutcome {
    enter_memory_document_with_options(
        runtime,
        owner,
        session.session,
        session.lease,
        1,
        LoadOptions::default(),
        |_| {},
    )
    .unwrap()
}

#[test]
fn process_policy_prepare_failure_changes_no_document_or_future_default() {
    let runtime = SharedRuntime::default();
    let healthy = register_memory(&runtime, "main");
    let path = fixture("policy-overflow", b"a,b\nx,y\n");
    let failing = register_disk(&runtime, "doc-1", &path);
    {
        let entry = authorize_big(
            &runtime,
            "doc-1",
            failing.session,
            failing.lease,
            Access::Write,
        )
        .unwrap();
        let mut file = entry.file.lock().unwrap();
        file.enter_memory_mode_with_options(
            &AtomicBool::new(false),
            1,
            LoadOptions::default(),
            |_| {},
        )
        .unwrap();
        file.set_backing_bytes_for_test(u64::MAX);
    }
    let healthy_before = policy_of(&runtime, "main", &healthy);
    let failing_before = policy_of(&runtime, "doc-1", &failing);
    let error = set_runtime_undo_policy(&runtime, 1024 * 1024, 10_000).unwrap_err();
    assert!(error.contains("溢出"), "实际错误：{error}");
    assert_eq!(policy_of(&runtime, "main", &healthy), healthy_before);
    assert_eq!(policy_of(&runtime, "doc-1", &failing), failing_before);
    let guard = runtime.lock().unwrap();
    assert_eq!(guard.undo_budget_bytes, history::DEFAULT_HISTORY_BUDGET_BYTES);
    assert_eq!(guard.resident_permille, history::DEFAULT_RESIDENT_PERMILLE);
}

#[test]
fn process_policy_update_and_inflight_registration_use_complete_policy() {
    let runtime = SharedRuntime::default();
    let a = register_memory(&runtime, "main");
    let b = register_memory(&runtime, "doc-1");
    let expected = UndoPolicy {
        budget_bytes: 1024 * 1024,
        resident_permille: 250,
    };
    let infos = set_runtime_undo_policy(
        &runtime,
        expected.budget_bytes,
        expected.resident_permille,
    )
    .unwrap();
    assert_eq!(
        infos.iter().map(|info| info.session).collect::<Vec<_>>(),
        vec![a.session, b.session]
    );
    assert_eq!(policy_of(&runtime, "main", &a), expected);
    assert_eq!(policy_of(&runtime, "doc-1", &b), expected);
    let c = register_memory(&runtime, "main");
    assert_eq!(policy_of(&runtime, "main", &c), expected);
    assert_eq!(c.meta.effective_undo_budget_bytes, expected.budget_bytes);
}

#[test]
fn process_policy_busy_document_does_not_block_registration_or_half_commit() {
    let runtime = SharedRuntime::default();
    let a = register_memory(&runtime, "main");
    let entry = authorize_big(&runtime, "main", a.session, a.lease, Access::Read).unwrap();
    let file_guard = entry.file.lock().unwrap();
    let setter_runtime = runtime.clone();
    let (busy_tx, busy_rx) = mpsc::channel();
    let setter = std::thread::spawn(move || {
        set_runtime_undo_policy_with_wait_hook(
            &setter_runtime,
            1024 * 1024,
            250,
            move || {
                let _ = busy_tx.send(());
            },
        )
    });
    busy_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let register_runtime = runtime.clone();
    let (register_tx, register_rx) = mpsc::channel();
    let registration = std::thread::spawn(move || {
        let result = register_new_document(
            &register_runtime,
            "doc-1",
            memory_doc(),
            None,
            None,
            None,
        );
        let _ = register_tx.send(result);
    });
    let registered = match register_rx.recv_timeout(Duration::from_secs(2)) {
        Ok(result) => result.unwrap(),
        Err(error) => {
            drop(file_guard);
            let _ = registration.join();
            let _ = setter.join();
            panic!("设置等待文档时仍阻塞新文档注册：{error}");
        }
    };
    assert_eq!(policy_of(&runtime, "doc-1", &registered), UndoPolicy::default());
    drop(file_guard);
    setter.join().unwrap().unwrap();
    registration.join().unwrap();
    let expected = UndoPolicy {
        budget_bytes: 1024 * 1024,
        resident_permille: 250,
    };
    assert_eq!(policy_of(&runtime, "main", &a), expected);
    assert_eq!(policy_of(&runtime, "doc-1", &registered), expected);
}

#[test]
fn process_policy_rejects_invalid_pair_before_changing_defaults() {
    let runtime = SharedRuntime::default();
    let document = register_memory(&runtime, "main");
    for (budget, resident) in [
        (0, 1000),
        (1025 * 1024 * 1024, 1000),
        (1024 * 1024, 10_001),
    ] {
        assert!(set_runtime_undo_policy(&runtime, budget, resident).is_err());
        assert_eq!(policy_of(&runtime, "main", &document), UndoPolicy::default());
    }
    let guard = runtime.lock().unwrap();
    assert_eq!(guard.undo_budget_bytes, history::DEFAULT_HISTORY_BUDGET_BYTES);
    assert_eq!(guard.resident_permille, history::DEFAULT_RESIDENT_PERMILLE);
}

#[test]
fn memory_mode_command_cancel_does_not_wait_for_document_write_lock() {
    let runtime = SharedRuntime::default();
    let mut bytes = b"a,b\n".to_vec();
    for row in 0..128 {
        bytes.extend_from_slice(format!("{row:04},abcdefghijklmnopqrstuvwxyz\n").as_bytes());
    }
    let path = fixture("cancel", &bytes);
    let session = register_disk(&runtime, "main", &path);
    let enter_runtime = runtime.clone();
    let (progress_tx, progress_rx) = mpsc::channel();
    let paused = Arc::new(AtomicBool::new(false));
    let observer_paused = paused.clone();
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let observer_gate = gate.clone();
    let session_id = session.session;
    let lease = session.lease;
    let enter = std::thread::spawn(move || {
        enter_memory_document_with_options(
            &enter_runtime,
            "main",
            session_id,
            lease,
            1,
            LoadOptions {
                chunk_bytes: 16,
                workers: 1,
                fault: crate::ram_load::LoadFault::None,
            },
            move |event| {
                if matches!(event, LoadEvent::Progress { .. })
                    && !observer_paused.swap(true, Ordering::SeqCst)
                {
                    let _ = progress_tx.send(());
                    let (lock, condvar) = &*observer_gate;
                    let mut released = lock.lock().unwrap();
                    while !*released {
                        released = condvar.wait(released).unwrap();
                    }
                }
            },
        )
    });
    progress_rx.recv_timeout(Duration::from_secs(2)).unwrap();

    let cancel_runtime = runtime.clone();
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let cancel = std::thread::spawn(move || {
        let result = cancel_memory_document(&cancel_runtime, "main", session_id, lease);
        let _ = cancel_tx.send(result);
    });
    let cancel_result = cancel_rx.recv_timeout(Duration::from_secs(2));
    {
        let (lock, condvar) = &*gate;
        *lock.lock().unwrap() = true;
        condvar.notify_all();
    }
    let outcome = enter.join().unwrap().unwrap();
    if cancel_result.is_err() {
        let _ = cancel.join();
        panic!("取消命令等待了文档写锁");
    }
    cancel_result.unwrap().unwrap();
    cancel.join().unwrap();
    assert!(!outcome.changed && outcome.cancelled);
    assert!(outcome.fixed_point.is_none());
    assert!(!outcome.meta.memory_mode && !outcome.meta.ram_backed);
    assert_eq!(outcome.meta.rows, 128);
}

#[test]
fn memory_mode_command_existing_ram_promotes_without_copy_progress() {
    let runtime = SharedRuntime::default();
    let path = fixture("promote", b"a,b\nx,y\n");
    let session = register_disk(&runtime, "main", &path);
    {
        let entry = authorize_big(
            &runtime,
            "main",
            session.session,
            session.lease,
            Access::Write,
        )
        .unwrap();
        entry
            .file
            .lock()
            .unwrap()
            .load_into_ram(&AtomicBool::new(false), &mut |_, _| {})
            .unwrap();
    }
    let events = AtomicUsize::new(0);
    let outcome = enter_memory_document_with_options(
        &runtime,
        "main",
        session.session,
        session.lease,
        1,
        LoadOptions::default(),
        |_| {
            events.fetch_add(1, Ordering::Relaxed);
        },
    )
    .unwrap();
    assert!(outcome.changed && !outcome.cancelled);
    assert!(outcome.meta.memory_mode && outcome.meta.ram_backed);
    assert_eq!(events.load(Ordering::Relaxed), 0);
}

#[test]
fn memory_mode_command_exit_maps_fixed_point_then_success() {
    let runtime = SharedRuntime::default();
    let mut bytes = b"a,b\nx,".to_vec();
    bytes.resize(bytes.len() + 512 * 1024, b'p');
    bytes.extend_from_slice(b"\n");
    let path = fixture("exit", &bytes);
    let session = register_disk(&runtime, "main", &path);
    enter_for_test(&runtime, "main", &session);
    set_runtime_undo_policy(&runtime, 1024 * 1024, 10_000).unwrap();
    {
        let entry = authorize_big(
            &runtime,
            "main",
            session.session,
            session.lease,
            Access::Write,
        )
        .unwrap();
        let edit = entry
            .file
            .lock()
            .unwrap()
            .set_cell(0, 0, "z".repeat(1200 * 1024), Approval::deny())
            .unwrap();
        assert!(edit.changed, "测试必须形成超过普通预算的真实历史");
    }
    let denied = exit_memory_document(
        &runtime,
        "main",
        session.session,
        session.lease,
        Approval::deny(),
    )
    .unwrap();
    assert!(!denied.changed && !denied.cancelled);
    assert!(denied.meta.memory_mode && denied.meta.ram_backed);
    let request = denied.fixed_point.unwrap();
    assert_eq!(request.kind, FixedPointKind::MemoryModeExit);
    let exited = exit_memory_document(
        &runtime,
        "main",
        session.session,
        session.lease,
        Approval {
            allow_fixed_point: true,
            preflight_id: Some(request.preflight_id),
        },
    )
    .unwrap();
    assert!(exited.changed && !exited.cancelled);
    assert!(exited.fixed_point.is_none());
    assert!(!exited.meta.memory_mode && !exited.meta.ram_backed);
    assert_eq!(exited.meta.effective_undo_budget_bytes, 1024 * 1024);
}

#[test]
fn memory_mode_command_progress_event_uses_current_owner_and_lease() {
    assert_eq!(MEMORY_PROGRESS_EVENT, "big-memory-progress");
    let runtime = SharedRuntime::default();
    let session = register_memory(&runtime, "main");
    let ticket = runtime
        .lock()
        .unwrap()
        .docs
        .seal(
            session.session,
            "main",
            session.lease,
            "doc-1",
            "A",
            0,
            serde_json::json!({}),
        )
        .unwrap();
    let adopted = runtime.lock().unwrap().docs.accept(ticket, "doc-1").unwrap();
    let stale_events = AtomicUsize::new(0);
    assert!(!route_memory_progress(
        &runtime,
        "main",
        session.session,
        session.lease,
        1,
        2,
        |_| {
            stale_events.fetch_add(1, Ordering::Relaxed);
        },
    ));
    assert_eq!(stale_events.load(Ordering::Relaxed), 0);
    assert!(route_memory_progress(
        &runtime,
        "doc-1",
        session.session,
        adopted.lease,
        1,
        2,
        |progress| {
            assert_eq!(progress.session, session.session);
            assert_eq!(progress.lease, adopted.lease);
            assert_eq!((progress.done, progress.total), (1, 2));
        },
    ));
}
