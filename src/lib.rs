pub mod er;
pub mod ingest;
pub mod overlay;
pub mod util;

use hudhook::{Hudhook, eject, hooks::dx12::ImguiDx12Hooks, tracing::error};
use overlay::ui::EROverlayUi;
use std::{
    any::Any,
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use crate::util::{debug::init_tracing, introspection::get_dll_path};

pub(crate) static RENDERER_INITIALIZED: AtomicBool = AtomicBool::new(false);
const RENDERER_INITIALIZATION_TIMEOUT: Duration = Duration::from_secs(30);

fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}

fn start_renderer_watchdog() {
    let result = thread::Builder::new()
        .name("ignite-overlay-watchdog".to_string())
        .spawn(|| {
            thread::sleep(RENDERER_INITIALIZATION_TIMEOUT);
            if !RENDERER_INITIALIZED.load(Ordering::Acquire) {
                debug_log!(
                    "[ignite_overlay] ⚠ DX12 hooks were applied, but the renderer did not initialize within {} seconds. Check preceding hudhook warnings/errors and disable competing graphics overlays before retrying.",
                    RENDERER_INITIALIZATION_TIMEOUT.as_secs()
                );
            }
        });

    if let Err(error) = result {
        debug_log!("[ignite_overlay] ⚠ Could not start renderer watchdog: {error}");
    }
}

fn initialize_overlay(hmodule: u64) {
    RENDERER_INITIALIZED.store(false, Ordering::Release);
    let render_loop = EROverlayUi::new();

    debug_log!(
        "[ignite_overlay] DLL process attach accepted: module=0x{hmodule:016X}, pid={}, version={}",
        std::process::id(),
        env!("CARGO_PKG_VERSION")
    );
    match get_dll_path() {
        Some(path) => debug_log!("[ignite_overlay] DLL path: '{}'", path.display()),
        None => debug_log!("[ignite_overlay] ⚠ Could not resolve the loaded DLL path"),
    }

    if init_tracing() {
        debug_log!(
            "[ignite_overlay] Tracing enabled (default: hudhook=debug; override with RUST_LOG)"
        );
    } else {
        debug_log!(
            "[ignite_overlay] ⚠ A tracing subscriber was already installed; hudhook will use it"
        );
    }

    debug_log!("[ignite_overlay] Creating DX12 hook targets...");
    let hooks = Hudhook::builder()
        .with::<ImguiDx12Hooks>(render_loop)
        .build();
    debug_log!("[ignite_overlay] DX12 hook targets created; applying hooks...");

    if let Err(error) = hooks.apply() {
        debug_log!("[ignite_overlay] ❌ Could not apply DX12 hooks: {error:?}");
        error!("Couldn't apply hooks: {error:?}");
        eject();
        return;
    }

    debug_log!(
        "[ignite_overlay] ✅ DX12 hooks applied; waiting for the first compatible swap chain and command queue"
    );
    start_renderer_watchdog();
}

#[unsafe(no_mangle)]
#[allow(non_snake_case)]
/// Windows DLL entry point.
///
/// # Safety
/// Must only be invoked by the Windows loader with a valid notification reason.
pub unsafe extern "C" fn DllMain(hmodule: u64, reason: u32) -> bool {
    const DLL_PROCESS_ATTACH: u32 = 1;

    if reason == DLL_PROCESS_ATTACH {
        let _ = thread::Builder::new()
            .name("ignite-overlay-startup".to_string())
            .spawn(move || {
                if let Err(payload) = std::panic::catch_unwind(|| initialize_overlay(hmodule)) {
                    debug_log!(
                        "[ignite_overlay] ❌ Startup panicked: {}",
                        panic_message(payload)
                    );
                }
            });
    }
    true
}
