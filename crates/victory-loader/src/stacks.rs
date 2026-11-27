//! Where every thread is: `VICTORY_STACKS=<seconds>[,<seconds>...]` prints
//! each thread's unwound stack as `module+rva` that many seconds after
//! [`dump_stacks_on_schedule`], to find what a title waits on.

use std::ffi::c_void;

use windows_sys::Wdk::System::Threading::{
    NtQueryInformationThread, ThreadBasicInformation, ThreadQuerySetWin32StartAddress,
};
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::System::Diagnostics::Debug::{
    CONTEXT, CONTEXT_FULL_AMD64, GetThreadContext,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentProcessId, GetCurrentThreadId, GetThreadDescription, OpenThread, ResumeThread,
    SuspendThread, THREAD_GET_CONTEXT, THREAD_QUERY_INFORMATION, THREAD_SUSPEND_RESUME,
};

use crate::crash::{MAX_FRAMES, locate, unwind_into};

/// Reads `VICTORY_STACKS` and, if set, starts a thread that prints every
/// thread's stack at each listed time (seconds from now).
pub fn dump_stacks_on_schedule() {
    if let Ok(prefix) = std::env::var("VICTORY_STACKS_OF") {
        watch_deep(prefix);
    }
    let Ok(list) = std::env::var("VICTORY_STACKS") else {
        return;
    };
    let times = schedule(&list);
    let start = std::time::Instant::now();
    std::thread::spawn(move || {
        for at in times {
            std::thread::sleep(at.saturating_sub(start.elapsed()));
            dump_stacks();
        }
    });
}

/// The times in a `VICTORY_STACKS` list (seconds, comma-separated), in
/// order. Negative times are now; times no `Duration` holds (`inf`, `NaN`,
/// `1e20`) are left out instead of panicking the dump thread.
fn schedule(list: &str) -> Vec<std::time::Duration> {
    let mut times: Vec<_> = list
        .split(',')
        .filter_map(|t| t.trim().parse::<f64>().ok())
        .filter(|t| !t.is_nan())
        .filter_map(|t| std::time::Duration::try_from_secs_f64(t.max(0.0)).ok())
        .collect();
    times.sort();
    times
}

/// `THREAD_BASIC_INFORMATION` (ntddk).
#[repr(C)]
struct BasicInfo {
    exit_status: i32,
    teb: *const usize,
    client_id: [usize; 2],
    affinity: usize,
    priority: i32,
    base_priority: i32,
}

/// `GetThreadContext` wants a 16-byte aligned context.
#[repr(C, align(16))]
struct Aligned(CONTEXT);

/// The ids of this process's threads but the caller's.
pub(crate) fn other_threads() -> Vec<u32> {
    // SAFETY: plain queries and a snapshot of this process's threads.
    let (me, process, snapshot) = unsafe {
        (
            GetCurrentThreadId(),
            GetCurrentProcessId(),
            CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0),
        )
    };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }
    let mut ids = Vec::new();
    // SAFETY: zeroed is a valid THREADENTRY32; its size is set before use.
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = size_of::<THREADENTRY32>() as u32;
    // SAFETY: `snapshot` is open; `entry` is sized.
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == process && entry.th32ThreadID != me {
            ids.push(entry.th32ThreadID);
        }
        // SAFETY: as above.
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    // SAFETY: we opened it.
    unsafe { CloseHandle(snapshot) };
    ids
}

/// Prints every other thread's stack to stderr. Each thread is suspended
/// only while its context is read and unwound into a buffer allocated
/// beforehand (it may hold the heap lock).
pub fn dump_stacks() {
    use std::fmt::Write as _;
    let ids = other_threads();
    // Also to the log in one record: a build for the windows subsystem has
    // no stderr under Wine, where `VICTORY_LOG_FILE` still gets it.
    let mut all = String::new();
    let mut say = |line: String| {
        eprintln!("{line}");
        let _ = writeln!(all, "{line}");
    };
    say(format!("VIctory: stacks of {} threads", ids.len()));
    for id in ids {
        // SAFETY: opens a thread of our own process.
        let thread = unsafe {
            OpenThread(
                THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION,
                0,
                id,
            )
        };
        if thread.is_null() {
            continue;
        }
        let (frames, (priority, base, affinity)) = frames_of(thread);
        say(format!(
            "  thread {id} \"{}\" start {} priority {priority} (base {base}) affinity {affinity:#x}",
            name_of(thread),
            locate(start_of(thread))
        ));
        for (i, pc) in frames.into_iter().enumerate() {
            say(format!("    #{i:<2} {}", locate(pc as usize)));
        }
        // SAFETY: we opened it.
        unsafe { CloseHandle(thread) };
    }
    tracing::info!("thread stacks:\n{all}");
}

/// `VICTORY_STACKS_OF=<name prefix>`: the threads whose names begin so are
/// looked at every few milliseconds for the first minute, and the first
/// time each is found more than 1 MiB down its stack, its frames are logged
/// (where a thread recursed until it ran out of stack: Wine says only where
/// the last fault was taken).
fn watch_deep(prefix: String) {
    const DEEP: usize = 1 << 20;
    let _ = std::thread::Builder::new()
        .name("victory-stacks-of".into())
        .spawn(move || {
            let start = std::time::Instant::now();
            let mut said: Vec<u32> = Vec::new();
            let mut watched: Vec<(u32, HANDLE, String)> = Vec::new();
            let mut looked = std::time::Instant::now() - std::time::Duration::from_secs(1);
            while start.elapsed() < std::time::Duration::from_secs(60) {
                if looked.elapsed() > std::time::Duration::from_millis(100) {
                    looked = std::time::Instant::now();
                    for id in other_threads() {
                        if watched.iter().any(|w| w.0 == id) {
                            continue;
                        }
                        // SAFETY: opens a thread of our own process.
                        let thread = unsafe {
                            OpenThread(
                                THREAD_SUSPEND_RESUME
                                    | THREAD_GET_CONTEXT
                                    | THREAD_QUERY_INFORMATION,
                                0,
                                id,
                            )
                        };
                        if thread.is_null() {
                            continue;
                        }
                        let name = name_of(thread);
                        if name.starts_with(&prefix) {
                            watched.push((id, thread, name));
                        } else {
                            // SAFETY: we opened it.
                            unsafe { CloseHandle(thread) };
                        }
                    }
                }
                for (id, thread, name) in &watched {
                    if said.contains(id) {
                        continue;
                    }
                    if let Some((depth, frames)) = deep_frames(*thread, DEEP) {
                        said.push(*id);
                        let mut all =
                            format!("thread {id} \"{name}\" {depth} bytes down its stack:");
                        for (i, pc) in frames.into_iter().enumerate() {
                            all.push_str(&format!("\n    #{i:<2} {}", locate(pc as usize)));
                        }
                        tracing::warn!("{all}");
                        eprintln!("VIctory: {all}");
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        });
}

/// The thread's frames, and how far down its stack it is, if past `deep`.
fn deep_frames(thread: HANDLE, deep: usize) -> Option<(usize, Vec<u64>)> {
    // (Taken before the suspension: the thread may hold the heap lock.)
    let mut frames = Vec::with_capacity(MAX_FRAMES);
    let mut info: BasicInfo = BasicInfo {
        exit_status: 0,
        teb: std::ptr::null(),
        client_id: [0; 2],
        affinity: 0,
        priority: 0,
        base_priority: 0,
    };
    // SAFETY: a thread of this process that is not this one.
    if unsafe { SuspendThread(thread) } == u32::MAX {
        return None;
    }
    // SAFETY: writes `info`, whose layout and size are the system's.
    let status = unsafe {
        NtQueryInformationThread(
            thread,
            ThreadBasicInformation,
            (&raw mut info).cast(),
            size_of::<BasicInfo>() as u32,
            std::ptr::null_mut(),
        )
    };
    let mut found = None;
    if status == 0 && !info.teb.is_null() {
        // SAFETY: as in [`frames_of`].
        let (high, low) = unsafe { (info.teb.add(1).read(), info.teb.add(2).read()) };
        // SAFETY: zeroed is a valid CONTEXT.
        let mut context = Aligned(unsafe { std::mem::zeroed() });
        context.0.ContextFlags = CONTEXT_FULL_AMD64;
        // SAFETY: the thread is suspended; the frames' room is taken before.
        unsafe {
            if GetThreadContext(thread, &mut context.0) != 0 {
                let depth = high.saturating_sub(context.0.Rsp as usize);
                if depth > deep {
                    unwind_into(context.0, low, high, &mut frames);
                    found = Some(depth);
                }
            }
        }
    }
    // SAFETY: suspended above.
    unsafe { ResumeThread(thread) };
    found.map(|depth| (depth, frames))
}

/// The thread's frames and its current and base scheduling priority (0-31).
pub(crate) fn frames_of(thread: HANDLE) -> (Vec<u64>, (i32, i32, usize)) {
    let mut frames = Vec::with_capacity(MAX_FRAMES);
    let mut info: BasicInfo = BasicInfo {
        exit_status: 0,
        teb: std::ptr::null(),
        client_id: [0; 2],
        affinity: 0,
        priority: 0,
        base_priority: 0,
    };
    // Suspended first: a running thread may exit and free its TEB between
    // the query and the read below.
    // SAFETY: a thread of this process that is not this one.
    if unsafe { SuspendThread(thread) } == u32::MAX {
        return (frames, (0, 0, 0));
    }
    // SAFETY: writes `info`, whose layout and size are the system's.
    let status = unsafe {
        NtQueryInformationThread(
            thread,
            ThreadBasicInformation,
            (&raw mut info).cast(),
            size_of::<BasicInfo>() as u32,
            std::ptr::null_mut(),
        )
    };
    if status != 0 || info.teb.is_null() {
        // SAFETY: suspended above.
        unsafe { ResumeThread(thread) };
        return (frames, (0, 0, 0));
    }
    // SAFETY: the TEB of a suspended thread of this process: its NT_TIB
    // holds the stack base at +8 and the stack limit at +0x10.
    let (high, low) = unsafe { (info.teb.add(1).read(), info.teb.add(2).read()) };
    // SAFETY: zeroed is a valid CONTEXT.
    let mut context = Aligned(unsafe { std::mem::zeroed() });
    context.0.ContextFlags = CONTEXT_FULL_AMD64;
    // SAFETY: reads and resumes the thread suspended above; nothing until
    // the resume allocates or takes a lock the thread could hold besides
    // the unwinder's own.
    unsafe {
        if GetThreadContext(thread, &mut context.0) != 0 {
            unwind_into(context.0, low, high, &mut frames);
        }
        ResumeThread(thread);
    }
    (frames, (info.priority, info.base_priority, info.affinity))
}

pub(crate) fn start_of(thread: HANDLE) -> usize {
    let mut start = 0usize;
    // SAFETY: writes one pointer-sized start address.
    unsafe {
        NtQueryInformationThread(
            thread,
            ThreadQuerySetWin32StartAddress,
            (&raw mut start).cast(),
            size_of::<usize>() as u32,
            std::ptr::null_mut(),
        )
    };
    start
}

pub(crate) fn name_of(thread: HANDLE) -> String {
    let mut name = std::ptr::null_mut();
    // SAFETY: the system allocates the string; we free it with LocalFree.
    unsafe {
        if GetThreadDescription(thread, &mut name) < 0 || name.is_null() {
            return String::new();
        }
        let len = (0..).take_while(|&i| *name.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(name, len));
        LocalFree(name.cast::<c_void>());
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn another_threads_stack_unwinds_to_its_start() {
        let (tx, rx) = std::sync::mpsc::channel();
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let parked = std::thread::spawn(move || {
            // SAFETY: plain query.
            tx.send(unsafe { GetCurrentThreadId() }).unwrap();
            stop_rx.recv().ok();
        });
        let id = rx.recv().unwrap();
        // SAFETY: a thread of this process.
        let thread = unsafe {
            OpenThread(
                THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION,
                0,
                id,
            )
        };
        assert!(!thread.is_null());
        let (frames, _) = frames_of(thread);
        // Waiting in the kernel, several frames down from the thread start.
        assert!(frames.len() >= 3, "{frames:?}");
        assert!(start_of(thread) != 0);
        // SAFETY: opened above.
        unsafe { CloseHandle(thread) };
        dump_stacks();
        stop_tx.send(()).unwrap();
        parked.join().unwrap();
    }

    /// Times no `Duration` holds are left out instead of panicking the dump
    /// thread (`from_secs_f64` panicked on `inf`); the rest come in order.
    #[test]
    fn schedules_skip_times_out_of_range() {
        let secs = |list| {
            schedule(list)
                .iter()
                .map(|d| d.as_secs_f64())
                .collect::<Vec<_>>()
        };
        assert_eq!(secs("30, 5,x,-2,inf,NaN,1e20"), [0.0, 5.0, 30.0]);
        assert!(schedule("").is_empty());
    }
}
