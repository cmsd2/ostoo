#![no_std]
#![no_main]

extern crate alloc;
extern crate ostoo_rt;

use alloc::format;
use ostoo_rt::println;

#[no_mangle]
fn main() -> i32 {
    println!("Hello from Rust on ostoo!");

    // Quick test that the allocator works.
    let msg = format!("Heap works: {}", 42);
    println!("{}", msg);

    0
}
