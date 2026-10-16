#![cfg_attr(not(test), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        let r = if std::env::args().any(|a| a == "--selftest") {
            plumb::window::selftest().map(|a| println!("selftest ok: {a}"))
        } else if std::env::args().any(|a| a == "--selftest-watch") {
            plumb::window::selftest_watch().map(|a| println!("selftest-watch ok: {a}"))
        } else if std::env::args().any(|a| a == "--selftest-delete") {
            plumb::window::selftest_delete().map(|a| println!("selftest-delete ok: {a}"))
        } else if std::env::args().any(|a| a == "--selftest-live") {
            plumb::window::selftest_live().map(|a| println!("selftest-live ok: {a}"))
        } else {
            plumb::window::run()
        };
        if let Err(e) = r {
            eprintln!("plumb: {e}");
            std::process::exit(1);
        }
    }
}
