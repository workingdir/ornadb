//! `serve` argument parsing (ORNA-SERVE-006): the bind address and the explicit
//! exposure opt-in. Kept apart from `cli_args.rs` so the parser stays within its
//! file budget; the exposure policy itself lives in `cli_serve`.

use std::net::{IpAddr, Ipv4Addr};

use super::super::Diagnostic;
use super::{Command, Words};

pub(super) fn parse_serve(words: &mut Words<'_>) -> Result<Command, Diagnostic> {
    let mut port = 8080;
    let mut bind = IpAddr::V4(Ipv4Addr::LOCALHOST);
    let mut expose = false;
    let mut allow_push = false;
    while let Some(option) = words.next() {
        match option {
            "--allow-push" => allow_push = true,
            "--expose" => expose = true,
            "--port" => {
                let value = words.require(
                    "`serve --port` needs a value",
                    "supply a port from 0 through 65535 after `--port`",
                )?;
                port = value.parse().map_err(|_| {
                    Diagnostic::usage(
                        "ORNA-CMD-ARGUMENT-INVALID",
                        "`serve --port` is invalid",
                        "supply a port from 0 through 65535 after `--port`",
                    )
                })?;
            }
            "--bind" => {
                let value = words.require(
                    "`serve --bind` needs an address",
                    "supply an IP address after `--bind`, for example `--bind 192.0.2.10`",
                )?;
                bind = value.parse().map_err(|_| {
                    Diagnostic::usage(
                        "ORNA-CMD-ARGUMENT-INVALID",
                        "`serve --bind` is invalid",
                        "supply an IP address after `--bind`, for example `--bind 192.0.2.10`",
                    )
                })?;
            }
            _ => {
                return Err(Diagnostic::usage(
                    "ORNA-CMD-ARGUMENT-INVALID",
                    "unsupported `serve` option",
                    "use `serve [--port PORT] [--bind ADDR --expose] [--allow-push]`; the listener stays on loopback unless exposed",
                ));
            }
        }
    }
    Ok(Command::Serve {
        port,
        bind,
        expose,
        allow_push,
    })
}

#[cfg(test)]
mod tests {
    use super::super::{Command, parse_cli};

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    /// ORNA-SERVE-006: a non-loopback bind is parsed only with its address; the
    /// exposure itself is decided by `serve` before anything binds.
    #[test]
    fn serve_bind_and_expose_are_parsed_explicitly() {
        assert_eq!(
            parse_cli(&args(&["serve", "--bind", "192.0.2.10", "--expose"]))
                .unwrap()
                .command,
            Command::Serve {
                port: 8080,
                bind: "192.0.2.10".parse().unwrap(),
                expose: true,
                allow_push: false,
            }
        );
        assert_eq!(
            parse_cli(&args(&["serve", "--bind", "[2001:db8::1]", "--expose"]))
                .unwrap_err()
                .code,
            "ORNA-CMD-ARGUMENT-INVALID",
            "a bracketed IPv6 literal is not an IP address for --bind"
        );
        assert_eq!(
            parse_cli(&args(&["serve", "--bind", "2001:db8::1", "--expose"]))
                .unwrap()
                .command,
            Command::Serve {
                port: 8080,
                bind: "2001:db8::1".parse().unwrap(),
                expose: true,
                allow_push: false,
            }
        );
        assert_eq!(
            parse_cli(&args(&["serve", "--bind"])).unwrap_err().code,
            "ORNA-CMD-ARGUMENT-MISSING"
        );
        assert_eq!(
            parse_cli(&args(&["serve", "--bind", "not-an-address"]))
                .unwrap_err()
                .code,
            "ORNA-CMD-ARGUMENT-INVALID"
        );
    }
}
