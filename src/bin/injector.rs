#![cfg_attr(not(windows), allow(dead_code))]

#[cfg(not(all(windows, target_pointer_width = "64")))]
compile_error!("The ELDEN RING overlay injector must be built as a 64-bit Windows executable");

use std::{
    ffi::{OsStr, OsString, c_void},
    io,
    mem::size_of,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    ptr::{null, null_mut},
    thread,
    time::{Duration, Instant},
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_BAD_LENGTH, ERROR_NO_MORE_FILES, HANDLE, INVALID_HANDLE_VALUE,
        WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT,
    },
    System::{
        Diagnostics::{
            Debug::WriteProcessMemory,
            ToolHelp::{
                CreateToolhelp32Snapshot, MODULEENTRY32W, Module32FirstW, Module32NextW,
                PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPMODULE,
                TH32CS_SNAPMODULE32, TH32CS_SNAPPROCESS,
            },
        },
        LibraryLoader::{GetModuleHandleW, GetProcAddress},
        Memory::{
            MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx,
        },
        Threading::{
            CreateRemoteThread, GetExitCodeThread, OpenProcess, PROCESS_CREATE_THREAD,
            PROCESS_QUERY_INFORMATION, PROCESS_VM_OPERATION, PROCESS_VM_READ, PROCESS_VM_WRITE,
            WaitForSingleObject,
        },
    },
};

const DEFAULT_PROCESS: &str = "eldenring.exe";
const DEFAULT_DLL: &str = "er_overlay.dll";
const DEFAULT_PROCESS_WAIT_SECONDS: u64 = 60;
const REMOTE_THREAD_TIMEOUT_MS: u32 = 30_000;

#[derive(Debug, PartialEq, Eq)]
struct Options {
    dll_path: PathBuf,
    process_name: String,
    wait_seconds: u64,
}

enum ParseResult {
    Run(Options),
    Help,
}

struct WinHandle(HANDLE);

impl WinHandle {
    fn new(handle: HANDLE, operation: &str) -> Result<Self, String> {
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            Err(last_error(operation))
        } else {
            Ok(Self(handle))
        }
    }

    fn get(&self) -> HANDLE {
        self.0
    }
}

impl Drop for WinHandle {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

struct RemoteAllocation {
    process: HANDLE,
    address: *mut c_void,
}

impl RemoteAllocation {
    fn leak(mut self) {
        self.address = null_mut();
    }
}

impl Drop for RemoteAllocation {
    fn drop(&mut self) {
        if !self.address.is_null() {
            unsafe {
                VirtualFreeEx(self.process, self.address, 0, MEM_RELEASE);
            }
        }
    }
}

fn main() {
    println!(
        "--- Ignite Overlay Injector {} ---",
        env!("CARGO_PKG_VERSION")
    );
    println!("Use only with ELDEN RING running offline/EAC-disabled.\n");

    let exe_dir = match std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf))
    {
        Some(path) => path,
        None => {
            fail("Could not determine the injector directory");
        }
    };

    let options = match parse_options(std::env::args_os().skip(1), &exe_dir) {
        Ok(ParseResult::Run(options)) => options,
        Ok(ParseResult::Help) => {
            print_help();
            return;
        }
        Err(error) => {
            eprintln!("Error: {error}\n");
            print_help();
            std::process::exit(2);
        }
    };

    if let Err(error) = run(options) {
        fail(&error);
    }
}

fn fail(message: &str) -> ! {
    eprintln!("Injection failed: {message}");
    eprintln!("If access was denied, run the injector at the same privilege level as ELDEN RING.");
    std::process::exit(1);
}

fn print_help() {
    println!(
        "Usage: injector.exe [--dll PATH] [--process NAME] [--wait SECONDS]\n\
         \n\
         Defaults:\n\
           --dll      er_overlay.dll beside injector.exe\n\
           --process  eldenring.exe\n\
           --wait     60 seconds\n\
         \n\
         The DLL must not also be configured as a ModEngine native DLL."
    );
}

fn parse_options(
    args: impl IntoIterator<Item = OsString>,
    exe_dir: &Path,
) -> Result<ParseResult, String> {
    let mut options = Options {
        dll_path: exe_dir.join(DEFAULT_DLL),
        process_name: DEFAULT_PROCESS.to_string(),
        wait_seconds: DEFAULT_PROCESS_WAIT_SECONDS,
    };
    let mut args = args.into_iter();

    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "-h" | "--help" => return Ok(ParseResult::Help),
            "--dll" => {
                options.dll_path = PathBuf::from(
                    args.next()
                        .ok_or_else(|| "--dll requires a path".to_string())?,
                );
            }
            "--process" => {
                options.process_name = args
                    .next()
                    .ok_or_else(|| "--process requires a name".to_string())?
                    .into_string()
                    .map_err(|_| "--process must be valid Unicode".to_string())?;
            }
            "--wait" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--wait requires a number of seconds".to_string())?
                    .into_string()
                    .map_err(|_| "--wait must be valid Unicode".to_string())?;
                options.wait_seconds = value
                    .parse()
                    .map_err(|_| format!("Invalid --wait value '{value}'"))?;
            }
            unknown => return Err(format!("Unknown argument '{unknown}'")),
        }
    }

    if options.process_name.trim().is_empty() {
        return Err("--process cannot be empty".to_string());
    }
    if !options.process_name.to_ascii_lowercase().ends_with(".exe") {
        options.process_name.push_str(".exe");
    }

    Ok(ParseResult::Run(options))
}

fn run(options: Options) -> Result<(), String> {
    let dll_path = options.dll_path.canonicalize().map_err(|error| {
        format!(
            "Could not resolve '{}': {error}",
            options.dll_path.display()
        )
    })?;
    if !dll_path.is_file() {
        return Err(format!("DLL path is not a file: '{}'", dll_path.display()));
    }
    if dll_path
        .extension()
        .and_then(OsStr::to_str)
        .is_none_or(|ext| !ext.eq_ignore_ascii_case("dll"))
    {
        return Err(format!(
            "Expected a .dll file, got '{}'",
            dll_path.display()
        ));
    }

    println!("DLL:     {}", dll_path.display());
    println!("Process: {}", options.process_name);
    if options.wait_seconds > 0 {
        println!(
            "Waiting up to {} seconds for the game...",
            options.wait_seconds
        );
    }

    let pid = wait_for_process(
        &options.process_name,
        Duration::from_secs(options.wait_seconds),
    )?;
    println!("Found {} (PID {pid})", options.process_name);

    let dll_name = dll_path
        .file_name()
        .ok_or_else(|| format!("DLL has no file name: '{}'", dll_path.display()))?;
    if let Some(loaded_path) = find_loaded_module(pid, dll_name)? {
        return Err(format!(
            "'{}' is already loaded from '{}'. Remove it from ModEngine before testing late injection.",
            dll_name.to_string_lossy(),
            loaded_path.display()
        ));
    }

    println!("Injecting with LoadLibraryW...");
    let remote_exit_code = inject(pid, &dll_path)?;

    match find_loaded_module(pid, dll_name) {
        Ok(Some(loaded_path)) => println!(
            "Injection succeeded: '{}' is loaded in PID {pid}.",
            loaded_path.display()
        ),
        Ok(None) if remote_exit_code == 0 => {
            return Err(
                "LoadLibraryW returned null. The DLL may have missing dependencies, the wrong architecture, or may have been blocked by Windows."
                    .to_string(),
            );
        }
        Ok(None) => println!(
            "Injection thread completed with code 0x{remote_exit_code:08X}; module enumeration could not confirm the DLL."
        ),
        Err(error) if remote_exit_code == 0 => {
            return Err(format!(
                "LoadLibraryW returned null and module verification failed: {error}"
            ));
        }
        Err(error) => println!(
            "Injection thread completed with code 0x{remote_exit_code:08X}; verification was unavailable: {error}"
        ),
    }

    Ok(())
}

fn wait_for_process(name: &str, timeout: Duration) -> Result<u32, String> {
    let started = Instant::now();
    loop {
        if let Some(pid) = find_process(name)? {
            return Ok(pid);
        }
        if started.elapsed() >= timeout {
            return Err(format!(
                "Could not find '{name}' within {} seconds",
                timeout.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn find_process(name: &str) -> Result<Option<u32>, String> {
    let snapshot = WinHandle::new(
        unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) },
        "CreateToolhelp32Snapshot(processes)",
    )?;
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };

    if unsafe { Process32FirstW(snapshot.get(), &mut entry) } == 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            Ok(None)
        } else {
            Err(format!("Process32FirstW failed: {error}"))
        };
    }

    loop {
        if wide_to_os_string(&entry.szExeFile)
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            return Ok(Some(entry.th32ProcessID));
        }

        if unsafe { Process32NextW(snapshot.get(), &mut entry) } == 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                Ok(None)
            } else {
                Err(format!("Process32NextW failed: {error}"))
            };
        }
    }
}

fn find_loaded_module(pid: u32, dll_name: &OsStr) -> Result<Option<PathBuf>, String> {
    let snapshot = module_snapshot(pid)?;
    let mut entry = MODULEENTRY32W {
        dwSize: size_of::<MODULEENTRY32W>() as u32,
        ..Default::default()
    };

    if unsafe { Module32FirstW(snapshot.get(), &mut entry) } == 0 {
        let error = io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
            Ok(None)
        } else {
            Err(format!("Module32FirstW failed for PID {pid}: {error}"))
        };
    }

    let expected_name = dll_name.to_string_lossy();
    loop {
        if wide_to_os_string(&entry.szModule)
            .to_string_lossy()
            .eq_ignore_ascii_case(&expected_name)
        {
            return Ok(Some(PathBuf::from(wide_to_os_string(&entry.szExePath))));
        }

        if unsafe { Module32NextW(snapshot.get(), &mut entry) } == 0 {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                Ok(None)
            } else {
                Err(format!("Module32NextW failed for PID {pid}: {error}"))
            };
        }
    }
}

fn module_snapshot(pid: u32) -> Result<WinHandle, String> {
    for _ in 0..5 {
        let handle =
            unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid) };
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE {
            return Ok(WinHandle(handle));
        }

        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_BAD_LENGTH as i32) {
            return Err(format!(
                "CreateToolhelp32Snapshot(modules) failed for PID {pid}: {error}"
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }

    Err(format!(
        "CreateToolhelp32Snapshot(modules) repeatedly returned ERROR_BAD_LENGTH for PID {pid}"
    ))
}

fn inject(pid: u32, dll_path: &Path) -> Result<u32, String> {
    let access = PROCESS_CREATE_THREAD
        | PROCESS_QUERY_INFORMATION
        | PROCESS_VM_OPERATION
        | PROCESS_VM_WRITE
        | PROCESS_VM_READ;
    let process = WinHandle::new(
        unsafe { OpenProcess(access, 0, pid) },
        &format!("OpenProcess(PID {pid})"),
    )?;

    let mut wide_path: Vec<u16> = dll_path.as_os_str().encode_wide().collect();
    if wide_path.contains(&0) {
        return Err("DLL path contains an embedded null character".to_string());
    }
    wide_path.push(0);
    let byte_len = wide_path.len() * size_of::<u16>();

    let remote_address = unsafe {
        VirtualAllocEx(
            process.get(),
            null(),
            byte_len,
            MEM_RESERVE | MEM_COMMIT,
            PAGE_READWRITE,
        )
    };
    if remote_address.is_null() {
        return Err(last_error("VirtualAllocEx"));
    }
    let remote = RemoteAllocation {
        process: process.get(),
        address: remote_address,
    };

    let mut written = 0usize;
    if unsafe {
        WriteProcessMemory(
            process.get(),
            remote.address,
            wide_path.as_ptr().cast(),
            byte_len,
            &mut written,
        )
    } == 0
    {
        return Err(last_error("WriteProcessMemory"));
    }
    if written != byte_len {
        return Err(format!(
            "WriteProcessMemory wrote {written} of {byte_len} bytes"
        ));
    }

    let kernel32 = wide_null(OsStr::new("kernel32.dll"));
    let kernel32_module = unsafe { GetModuleHandleW(kernel32.as_ptr()) };
    if kernel32_module.is_null() {
        return Err(last_error("GetModuleHandleW(kernel32.dll)"));
    }
    let load_library = unsafe { GetProcAddress(kernel32_module, c"LoadLibraryW".as_ptr().cast()) }
        .ok_or_else(|| last_error("GetProcAddress(LoadLibraryW)"))?;
    let load_library = Some(unsafe {
        std::mem::transmute::<
            unsafe extern "system" fn() -> isize,
            unsafe extern "system" fn(*mut c_void) -> u32,
        >(load_library)
    });

    let remote_thread = WinHandle::new(
        unsafe {
            CreateRemoteThread(
                process.get(),
                null(),
                0,
                load_library,
                remote.address,
                0,
                null_mut(),
            )
        },
        "CreateRemoteThread(LoadLibraryW)",
    )?;

    match unsafe { WaitForSingleObject(remote_thread.get(), REMOTE_THREAD_TIMEOUT_MS) } {
        WAIT_OBJECT_0 => {}
        WAIT_TIMEOUT => {
            remote.leak();
            return Err(format!(
                "LoadLibraryW did not finish within {} seconds; remote path memory was intentionally retained",
                REMOTE_THREAD_TIMEOUT_MS / 1000
            ));
        }
        WAIT_FAILED => return Err(last_error("WaitForSingleObject")),
        result => {
            return Err(format!(
                "WaitForSingleObject returned unexpected code {result}"
            ));
        }
    }

    let mut exit_code = 0u32;
    if unsafe { GetExitCodeThread(remote_thread.get(), &mut exit_code) } == 0 {
        return Err(last_error("GetExitCodeThread"));
    }
    Ok(exit_code)
}

fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn wide_to_os_string(value: &[u16]) -> OsString {
    let length = value
        .iter()
        .position(|character| *character == 0)
        .unwrap_or(value.len());
    OsString::from_wide(&value[..length])
}

fn last_error(operation: &str) -> String {
    format!("{operation} failed: {}", io::Error::last_os_error())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_distribution_defaults() {
        let root = Path::new(r"C:\overlay");
        let ParseResult::Run(options) = parse_options(Vec::new(), root).unwrap() else {
            panic!("expected run options");
        };

        assert_eq!(
            options,
            Options {
                dll_path: root.join(DEFAULT_DLL),
                process_name: DEFAULT_PROCESS.to_string(),
                wait_seconds: DEFAULT_PROCESS_WAIT_SECONDS,
            }
        );
    }

    #[test]
    fn parses_overrides_and_normalizes_process_extension() {
        let arguments = [
            "--dll",
            r"D:\mods\custom.dll",
            "--process",
            "game",
            "--wait",
            "5",
        ]
        .map(OsString::from);
        let ParseResult::Run(options) = parse_options(arguments, Path::new(r"C:\overlay")).unwrap()
        else {
            panic!("expected run options");
        };

        assert_eq!(options.dll_path, PathBuf::from(r"D:\mods\custom.dll"));
        assert_eq!(options.process_name, "game.exe");
        assert_eq!(options.wait_seconds, 5);
    }

    #[test]
    fn rejects_unknown_and_incomplete_arguments() {
        assert!(parse_options([OsString::from("--unknown")], Path::new(".")).is_err());
        assert!(parse_options([OsString::from("--dll")], Path::new(".")).is_err());
        assert!(
            parse_options(
                [OsString::from("--wait"), OsString::from("later")],
                Path::new(".")
            )
            .is_err()
        );
    }
}
