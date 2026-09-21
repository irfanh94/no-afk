//! Dev tool: hold a keep-awake session so you can inspect it with the OS.
//!
//! ```text
//! cargo run -p awake-core --example hold -- 30
//! # then, in another shell:
//! pmset -g assertions | grep no-afk
//! ```

use std::sync::Arc;
use std::time::Duration;

use awake_core::session::{Kind, Manager, SystemClock};
use awake_core::{default_backend, Flags};

fn main() {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(15);

    let backend = default_backend();
    println!("backend: {}", backend.name());

    let mut mgr = Manager::new(backend, Arc::new(SystemClock));
    mgr.start(Kind::For(Duration::from_secs(secs)), Flags::display_and_system(), "example hold")
        .expect("failed to acquire");

    println!("holding for {secs}s — check: pmset -g assertions | grep no-afk");

    while !mgr.tick().expect("tick failed") {
        if let Some(left) = mgr.remaining() {
            print!("\r  {:>4}s remaining ", left.as_secs());
            use std::io::Write;
            let _ = std::io::stdout().flush();
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    println!("\nsession auto-ended; assertions released");
}
