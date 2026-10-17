// Test helper: behaves exactly like the GUI's `main()` hook. Built for the
// GUI subsystem so the tests prove stdout still reaches git through the pipe.
#![windows_subsystem = "windows"]

fn main() {
    if let Some(code) = gk_askpass::client_main() {
        std::process::exit(code);
    }
    // Not launched as a helper: nothing to do.
    std::process::exit(2);
}
