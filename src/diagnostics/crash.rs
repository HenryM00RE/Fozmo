/// Install panic + native crash diagnostics so abrupt exits leave a clue in the terminal.
pub fn install_crash_handlers() {
    // Rust panic hook captures panics from any thread, including audio workers.
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        eprintln!("\n!!! PANIC !!!");
        eprintln!("{}", info);
        if let Some(loc) = info.location() {
            eprintln!("at {}:{}:{}", loc.file(), loc.line(), loc.column());
        }
        eprintln!();
        default(info);
    }));

    #[cfg(target_os = "windows")]
    install_seh_handler();
}

#[cfg(target_os = "windows")]
fn install_seh_handler() {
    use windows::Win32::System::Diagnostics::Debug::{
        EXCEPTION_CONTINUE_SEARCH, EXCEPTION_POINTERS, SetUnhandledExceptionFilter,
    };

    unsafe extern "system" fn handler(info: *const EXCEPTION_POINTERS) -> i32 {
        if !info.is_null() {
            let rec = unsafe { (*info).ExceptionRecord };
            if !rec.is_null() {
                eprintln!(
                    "\n!!! NATIVE CRASH !!! code=0x{:08x} addr={:p}",
                    unsafe { (*rec).ExceptionCode.0 as u32 },
                    unsafe { (*rec).ExceptionAddress }
                );
            } else {
                eprintln!("\n!!! NATIVE CRASH (no record) !!!");
            }
        }
        EXCEPTION_CONTINUE_SEARCH
    }

    unsafe {
        SetUnhandledExceptionFilter(Some(handler));
    }
}
