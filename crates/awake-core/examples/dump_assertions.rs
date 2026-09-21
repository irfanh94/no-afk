//! Dev tool: print every power assertion held on the system.
//!
//! Handy for comparing this crate's view against the OS:
//!
//! ```text
//! cargo run -p awake-core --example dump_assertions
//! pmset -g assertions          # should agree, row for row
//! ```

fn main() {
    let backend = awake_core::default_backend();
    println!("backend: {}\n", backend.name());

    match backend.system_assertions() {
        Ok(list) => {
            println!("{} assertions held system-wide\n", list.len());
            for a in &list {
                println!("  pid {:<7} {:<26} {:<30} {}", a.pid, a.process, a.kind, a.name);
            }
        }
        Err(err) => eprintln!("could not read assertions: {err}"),
    }
}
