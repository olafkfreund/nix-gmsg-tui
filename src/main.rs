//! Google Messages backend.
// Step 6 wires these modules into the daemon and removes this temporary allowance.
#![allow(dead_code)]

mod cli;
mod key;
mod pair;
mod paths;
mod phone;
mod proto;
mod session;
#[cfg(any(feature = "wry-signin", test))]
mod signin;
mod store;
mod sync;
mod web;

/// The last release tag, `+` the commit when built beyond it (see build.rs).
const VERSION: &str = env!("GMESSAGES_TUI_VERSION");

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match cli::parse(args.iter().map(String::as_str)) {
        Ok(cli::Command::Serve) => {
            eprintln!("serve: not yet");
            std::process::exit(2);
        }
        Ok(cli::Command::Version) => println!("{VERSION}"),
        #[cfg(feature = "wry-signin")]
        Ok(cli::Command::InternalSignin) => signin::run_child(),
        #[cfg(not(feature = "wry-signin"))]
        Ok(cli::Command::InternalSignin) => unreachable!(),
        Err(usage) => {
            eprintln!("usage: {usage}");
            std::process::exit(2);
        }
    }
}
