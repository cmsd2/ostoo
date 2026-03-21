#![feature(restricted_std)]
#![no_main]

extern crate ostoo_rt;

use std::collections::HashMap;

#[no_mangle]
fn main() -> i32 {
    println!("Hello from Rust std on ostoo!");

    let mut map = HashMap::new();
    map.insert("key", 42);
    println!("HashMap works: {:?}", map);

    0
}
