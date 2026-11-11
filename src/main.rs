//! Google Messages backend.

mod cli;
#[allow(dead_code, reason = "key deletion belongs to later stages")]
mod key;
#[allow(dead_code, reason = "pairing helpers are retained for later stages")]
mod pair;
mod paths;
#[allow(dead_code, reason = "phone APIs also serve later stages")]
mod phone;
mod proto;
mod serve;
#[allow(dead_code, reason = "session reset belongs to later stages")]
mod session;
#[cfg(any(feature = "wry-signin", test))]
mod signin;
#[allow(dead_code, reason = "media APIs belong to later stages")]
mod store;
mod sync;
#[allow(dead_code, reason = "pairing page status APIs belong to later stages")]
mod web;

/// The last release tag, `+` the commit when built beyond it (see build.rs).
const VERSION: &str = env!("GMESSAGES_TUI_VERSION");

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match cli::parse(args.iter().map(String::as_str)) {
        Ok(cli::Command::Serve) => serve::run(),
        Ok(cli::Command::Version) => {
            println!("{VERSION}");
            std::process::ExitCode::SUCCESS
        }
        #[cfg(feature = "wry-signin")]
        Ok(cli::Command::InternalSignin) => {
            signin::run_child();
            std::process::ExitCode::SUCCESS
        }
        #[cfg(not(feature = "wry-signin"))]
        Ok(cli::Command::InternalSignin) => unreachable!(),
        Err(usage) => {
            eprintln!("usage: {usage}");
            std::process::ExitCode::from(2)
        }
    }
}
