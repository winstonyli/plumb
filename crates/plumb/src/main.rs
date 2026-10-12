#![cfg_attr(not(test), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        let r = if std::env::args().any(|a| a == "--selftest") {
            plumb::window::selftest().map(|a| println!("selftest ok: {a}"))
        } else {
            plumb::window::run()
        };
        if let Err(e) = r {
            eprintln!("plumb: {e}");
            std::process::exit(1);
        }
    }
}
