// ponytail: the core modules are not wired to an OS backend until A2
#![allow(dead_code)]

mod crossing;
mod keymap;
mod link;
mod msg;

fn main() {
    eprintln!("yunta {}: core only, nothing to run yet", env!("CARGO_PKG_VERSION"));
}
