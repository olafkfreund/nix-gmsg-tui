//! Command-line options, parsed before touching state.

pub const USAGE: &str = "gmessages-tui serve | --version";

#[derive(Debug, PartialEq)]
pub enum Command {
    Serve,
    Version,
    InternalSignin,
}

pub fn parse<'a>(args: impl IntoIterator<Item = &'a str>) -> Result<Command, &'static str> {
    match args.into_iter().collect::<Vec<_>>().as_slice() {
        ["serve"] => Ok(Command::Serve),
        ["--version"] => Ok(Command::Version),
        ["--internal-google-signin"] if cfg!(feature = "wry-signin") => Ok(Command::InternalSignin),
        _ => Err(USAGE),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepted_commands() {
        assert_eq!(parse(["serve"]), Ok(Command::Serve));
        assert_eq!(parse(["--version"]), Ok(Command::Version));
        if cfg!(feature = "wry-signin") {
            assert_eq!(
                parse(["--internal-google-signin"]),
                Ok(Command::InternalSignin)
            );
        }
    }

    #[test]
    fn unsupported_commands_are_rejected() {
        for args in [
            vec![],
            vec!["--manual"],
            vec!["--qr"],
            vec!["--unpair"],
            vec!["--uninstall"],
            vec!["serve", "--version"],
        ] {
            assert!(parse(args).is_err());
        }
    }
}
