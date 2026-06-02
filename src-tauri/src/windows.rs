// Windows-only Win32 helpers for tying spawned engines to Myo's lifetime.
//
// Ported from MyOwnLLM's `windows.rs` (the reference for "close what you
// started"). Two mechanisms, because Windows doesn't propagate a parent's death
// to its children the way a Unix process group does:
//
// 1. **Job Object (`KILL_ON_JOB_CLOSE`)** — assign each spawned engine to a job
//    Myo holds open. When Myo exits *any* way (clean quit, crash, taskkill), the
//    OS reclaims its handles, the job closes, and the engine is terminated. And
//    because MyOwnLLM puts *its* daemon in *its own* job, hard-killing
//    `myownllm` cascades to `myownmesh` — the chain Myo wants.
//
// 2. **Parent-PID watchdog** — a Tauri GUI app detaches from the terminal's
//    console, so a Ctrl-C in the shell that ran `just dev` reaches `cargo` but
//    not `myo`, leaving `myo` (and its engines) orphaned. The watchdog polls the
//    parent and, when it disappears, exits the process — so `Drop` +
//    `RunEvent::Exit` run and the engines come down with it.
//
// Bare `extern "system"` against kernel32 — no extra crates.
#![allow(dead_code, non_snake_case)] // Win32 FFI: PascalCase names, blob structs.

use std::ffi::c_void;

type Dword = u32;
type Bool = i32;
type Handle = *mut c_void;

const INVALID_HANDLE_VALUE: Handle = !0usize as Handle;
const PROCESS_TERMINATE: Dword = 0x0001;
const PROCESS_QUERY_LIMITED_INFORMATION: Dword = 0x1000;
const STILL_ACTIVE: Dword = 259;
const TH32CS_SNAPPROCESS: Dword = 0x0000_0002;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: Dword = 0x0000_2000;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

#[repr(C)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[repr(C)]
struct JobObjectBasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: Dword,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: Dword,
    affinity: usize,
    priority_class: Dword,
    scheduling_class: Dword,
}

#[repr(C)]
struct JobObjectExtendedLimitInformation {
    basic_limit_information: JobObjectBasicLimitInformation,
    io_info: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[repr(C)]
struct ProcessEntry32W {
    dw_size: Dword,
    cnt_usage: Dword,
    th32_process_id: Dword,
    th32_default_heap_id: usize,
    th32_module_id: Dword,
    cnt_threads: Dword,
    th32_parent_process_id: Dword,
    pc_pri_class_base: i32,
    dw_flags: Dword,
    sz_exe_file: [u16; 260],
}

extern "system" {
    fn CreateJobObjectW(lp_job_attributes: *mut c_void, lp_name: *const u16) -> Handle;
    fn SetInformationJobObject(
        h_job: Handle,
        job_object_information_class: i32,
        lp_job_object_information: *mut c_void,
        cb_job_object_information_length: Dword,
    ) -> Bool;
    fn AssignProcessToJobObject(h_job: Handle, h_process: Handle) -> Bool;
    fn OpenProcess(
        dw_desired_access: Dword,
        b_inherit_handle: Bool,
        dw_process_id: Dword,
    ) -> Handle;
    fn GetExitCodeProcess(h_process: Handle, lp_exit_code: *mut Dword) -> Bool;
    fn CloseHandle(h_object: Handle) -> Bool;
    fn GetCurrentProcessId() -> Dword;
    fn CreateToolhelp32Snapshot(dw_flags: Dword, th32_process_id: Dword) -> Handle;
    fn Process32FirstW(h_snapshot: Handle, lppe: *mut ProcessEntry32W) -> Bool;
    fn Process32NextW(h_snapshot: Handle, lppe: *mut ProcessEntry32W) -> Bool;
}

/// Assign a spawned engine to a fresh `KILL_ON_JOB_CLOSE` Job Object so it dies
/// when Myo does. The job handle is intentionally **leaked** — it must stay open
/// for the engine's lifetime; the OS reclaims it (closing the job, killing the
/// engine) when Myo's process ends. Best-effort: a Win32 failure just falls back
/// to `Drop`/`RunEvent::Exit` kill.
pub fn assign_to_kill_on_close_job(child_process_handle: *mut c_void) {
    unsafe {
        let job = CreateJobObjectW(std::ptr::null_mut(), std::ptr::null());
        if job.is_null() {
            return;
        }
        let mut info: JobObjectExtendedLimitInformation = std::mem::zeroed();
        info.basic_limit_information.limit_flags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
            &mut info as *mut _ as *mut c_void,
            std::mem::size_of::<JobObjectExtendedLimitInformation>() as Dword,
        );
        if ok == 0 || AssignProcessToJobObject(job, child_process_handle) == 0 {
            CloseHandle(job);
            return;
        }
        // Intentionally do NOT CloseHandle(job): keeping the handle open is what
        // keeps the engine tied to Myo (the job has no `Drop` — it's a raw
        // handle). Myo's exit reclaims it, closing the job and killing the engine.
    }
}

/// Our parent's PID, by walking the process snapshot. `None` if we can't find it.
fn parent_pid() -> Option<Dword> {
    unsafe {
        let our_pid = GetCurrentProcessId();
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: ProcessEntry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as Dword;
        let mut found = None;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32_process_id == our_pid {
                    found = Some(entry.th32_parent_process_id);
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        found
    }
}

/// Read a process's image name (lowercased) from the snapshot, for the
/// Explorer/services skip below.
fn process_image_name(pid: Dword) -> Option<String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut entry: ProcessEntry32W = std::mem::zeroed();
        entry.dw_size = std::mem::size_of::<ProcessEntry32W>() as Dword;
        let mut name = None;
        if Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                if entry.th32_process_id == pid {
                    let end = entry.sz_exe_file.iter().position(|&c| c == 0).unwrap_or(0);
                    name = Some(
                        String::from_utf16_lossy(&entry.sz_exe_file[..end]).to_ascii_lowercase(),
                    );
                    break;
                }
                if Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snapshot);
        name
    }
}

fn process_is_alive(handle: Handle) -> bool {
    unsafe {
        let mut code: Dword = 0;
        if GetExitCodeProcess(handle, &mut code) == 0 {
            return false;
        }
        code == STILL_ACTIVE
    }
}

/// Install the parent-PID watchdog: a daemon thread that exits the process when
/// the parent (e.g. `cargo` under `just dev`) disappears, so `Drop` +
/// `RunEvent::Exit` run and the spawned engines come down too.
///
/// Skipped when the parent is `explorer.exe`/`services.exe` (a real launch we'd
/// never want to die just because Explorer hiccuped) or when we can't find it.
pub fn install_parent_watchdog() {
    let Some(ppid) = parent_pid() else {
        return;
    };
    if let Some(name) = process_image_name(ppid) {
        if name == "explorer.exe" || name == "services.exe" {
            return;
        }
    }
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
            0,
            ppid,
        )
    };
    if handle.is_null() {
        return;
    }
    let handle_addr = handle as usize;
    std::thread::Builder::new()
        .name("parent-watchdog".into())
        .spawn(move || {
            let handle = handle_addr as Handle;
            loop {
                std::thread::sleep(std::time::Duration::from_millis(1000));
                if !process_is_alive(handle) {
                    eprintln!("[watchdog] parent pid {ppid} exited — shutting Myo down");
                    // exit(0) so Drop + RunEvent::Exit run and the engines die.
                    std::process::exit(0);
                }
            }
        })
        .ok();
}
