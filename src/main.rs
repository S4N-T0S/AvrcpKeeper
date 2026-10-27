#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(windows))]
compile_error!("AvrcpKeeper currently supports Windows only.");

mod background;
mod config;
mod gui;
mod logging;
mod platform;

use std::process::ExitCode;
use std::time::Duration;

use platform::{console, ipc};

const USAGE: &str = "\
Usage: AvrcpKeeper [OPTION]

Without options, opens the settings window.

  --background   Run without a window using the saved settings (used at startup)
  --verbose      With --background, write a detailed log even if logging is off
  --stop         Stop the instance running in the background
  --version      Print the version and exit
  --help         Print this help and exit
";

fn main() -> ExitCode {
    platform::restrict_dll_search();
    logging::init(&platform::exe_dir());
    std::panic::set_hook(Box::new(|info| log!("Panic: {info}")));

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        // glutin loads whatever DLL this names as the OpenGL driver.
        std::env::remove_var("GLUTIN_WGL_OPENGL_DLL");
        return gui::run();
    }

    let mut background = false;
    let mut verbose = false;
    for arg in &args {
        match arg.as_str() {
            "--background" => background = true,
            "--verbose" => verbose = true,
            "--stop" => return stop(),
            "--version" => {
                console::print(concat!("AvrcpKeeper ", env!("CARGO_PKG_VERSION"), "\n"));
                return ExitCode::SUCCESS;
            }
            "--help" | "-h" | "/?" => {
                console::print(USAGE);
                return ExitCode::SUCCESS;
            }
            other => {
                console::print(&format!("Unknown option: {other}\n\n{USAGE}"));
                return ExitCode::from(2);
            }
        }
    }
    if !background {
        console::print(&format!("--verbose needs --background\n\n{USAGE}"));
        return ExitCode::from(2);
    }
    background::run(verbose)
}

fn stop() -> ExitCode {
    if !ipc::background_running() {
        console::print("AvrcpKeeper is not running.\n");
        return ExitCode::SUCCESS;
    }
    if ipc::request_stop() && ipc::wait_until_stopped(Duration::from_secs(5)) {
        console::print("Stopped.\n");
        ExitCode::SUCCESS
    } else {
        console::print("AvrcpKeeper did not stop in time.\n");
        ExitCode::FAILURE
    }
}
