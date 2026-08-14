#[macro_export]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        eprintln!($($arg)*)
    };
}

/// Installs a process-local tracing subscriber so hudhook's diagnostics are
/// written to the same stderr stream as the overlay's startup messages.
pub fn init_tracing() -> bool {
    use std::sync::OnceLock;
    use tracing_subscriber::EnvFilter;

    static TRACING_READY: OnceLock<bool> = OnceLock::new();
    *TRACING_READY.get_or_init(|| {
        let filter = EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("warn,hudhook=debug,er_overlay=debug"));

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_target(true)
            .with_thread_ids(true)
            .with_thread_names(true)
            .with_file(true)
            .with_line_number(true)
            .with_writer(std::io::stderr)
            .try_init()
            .is_ok()
    })
}

pub fn attach_console() -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};

    static CONSOLE_READY: AtomicBool = AtomicBool::new(false);
    if CONSOLE_READY.load(Ordering::Acquire) {
        return true;
    }

    unsafe {
        use std::io::{self, Write};
        use windows_sys::Win32::{
            Foundation::{GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE},
            Storage::FileSystem::{
                CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE,
                OPEN_EXISTING,
            },
            System::Console::{
                ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, STD_ERROR_HANDLE,
                STD_OUTPUT_HANDLE, SetConsoleOutputCP, SetStdHandle,
            },
        };

        const CONOUT: [u16; 8] = [
            'C' as u16, 'O' as u16, 'N' as u16, 'O' as u16, 'U' as u16, 'T' as u16, '$' as u16, 0,
        ];

        fn open_console_output() -> Option<HANDLE> {
            let handle = unsafe {
                CreateFileW(
                    CONOUT.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    std::ptr::null_mut(),
                )
            };
            (handle != INVALID_HANDLE_VALUE).then_some(handle)
        }

        let (handle, source) = if let Some(handle) = open_console_output() {
            (handle, "existing console")
        } else if AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            let Some(handle) = open_console_output() else {
                return false;
            };
            (handle, "parent console")
        } else if AllocConsole() != 0 {
            let Some(handle) = open_console_output() else {
                return false;
            };
            (handle, "new console")
        } else {
            return false;
        };

        if SetStdHandle(STD_OUTPUT_HANDLE, handle) == 0
            || SetStdHandle(STD_ERROR_HANDLE, handle) == 0
        {
            return false;
        }
        SetConsoleOutputCP(65001);
        CONSOLE_READY.store(true, Ordering::Release);

        let mut stderr = io::stderr().lock();
        let _ = writeln!(stderr, "--- Ignite Overlay Console ({source}) ---");
        let _ = stderr.flush();
        true
    }
}
