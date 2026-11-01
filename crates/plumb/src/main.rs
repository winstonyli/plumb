#![cfg_attr(not(test), windows_subsystem = "windows")]

fn main() {
    #[cfg(windows)]
    {
        let r = match plumb::window::run_selftest(std::env::args()) {
            Some(r) => r.map(|line| println!("{line}")),
            None => plumb::window::run(),
        };
        if let Err(e) = r {
            eprintln!("plumb: {e}");
            std::process::exit(1);
        }
    }
}
