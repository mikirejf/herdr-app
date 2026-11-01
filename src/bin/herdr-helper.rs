//! CEF's helper process: Chromium starts its renderer, GPU, network and other
//! processes from it. The dev bundle puts copies of it under
//! `Contents/Frameworks/Herdr Helper*.app` (scripts/browser-dev-bundle); it is
//! never run by hand.

#[cfg(target_os = "macos")]
fn main() {
    let args = cef::args::Args::new();
    // The sandbox comes first, before the framework is loaded
    // (cef_sandbox_mac.h). It stays alive until the process ends.
    let mut sandbox = cef::sandbox::Sandbox::new();
    sandbox.initialize(args.as_main_args());
    // `true`: the framework is three levels up, beside the helper apps.
    let exe = std::env::current_exe().expect("helper path");
    let loader = cef::library_loader::LibraryLoader::new(&exe, true);
    if !loader.load() {
        eprintln!("herdr-helper: cannot load the CEF framework");
        std::process::exit(1);
    }
    // Before any other CEF call: the API version these bindings speak.
    if cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0).is_null() {
        eprintln!("herdr-helper: the CEF framework is a different build than herdr-app expects");
        std::process::exit(1);
    }
    let code = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
    drop(loader);
    drop(sandbox);
    std::process::exit(code.max(0));
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("herdr-helper: the browser is macOS only for now");
    std::process::exit(1);
}
