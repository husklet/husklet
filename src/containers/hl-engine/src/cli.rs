//! Top-level route selection.

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Route {
    Guest,
    Config { path: String },
    Server,
    Client,
}

impl Route {
    /// Applies the retained route precedence. Only a complete leading
    /// `--configfile PATH` beats server/client flags; otherwise the first
    /// server/client flag wins.
    #[must_use]
    pub fn parse<I, S>(arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let arguments: Vec<String> = arguments.into_iter().map(Into::into).collect();
        if arguments.len() > 2 && arguments[1] == "--configfile" {
            return Self::Config {
                path: arguments[2].clone(),
            };
        }
        for argument in arguments.iter().skip(1) {
            match argument.as_str() {
                "--server" => return Self::Server,
                "--client" => return Self::Client,
                _ => {}
            }
        }
        Self::Guest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leading_complete_config() {
        assert_eq!(
            Route::parse(["hl-engine", "--configfile", "launch", "--server"]),
            Route::Config { path: "launch".into() }
        );
    }

    #[test]
    fn incomplete_or_late() {
        assert_eq!(Route::parse(["hl-engine", "--configfile"]), Route::Guest);
        assert_eq!(
            Route::parse(["hl-engine", "--server", "--configfile", "launch"]),
            Route::Server
        );
    }

    #[test]
    fn first_server_or() {
        assert_eq!(Route::parse(["hl-engine", "--client", "--server"]), Route::Client);
        assert_eq!(Route::parse(["hl-engine", "--server", "--client"]), Route::Server);
    }
}
