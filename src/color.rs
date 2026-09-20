use std::{env, fmt::Display, io::IsTerminal};

use clap::ValueEnum;

#[derive(Clone, Copy, ValueEnum)]
pub enum ColorMode {
    Auto,
    Always,
    Never,
}

impl ColorMode {
    pub fn stdout(self) -> Colors {
        self.for_terminal(std::io::stdout().is_terminal())
    }

    pub fn stderr(self) -> Colors {
        self.for_terminal(std::io::stderr().is_terminal())
    }

    fn for_terminal(self, terminal: bool) -> Colors {
        Colors(match self {
            Self::Always => true,
            Self::Never => false,
            Self::Auto => {
                terminal
                    && env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
                    && env::var("TERM").as_deref() != Ok("dumb")
            }
        })
    }
}

#[derive(Clone, Copy)]
pub struct Colors(bool);

impl Colors {
    pub fn paint(self, code: &str, value: impl Display) -> String {
        if self.0 {
            format!("\x1b[{code}m{value}\x1b[0m")
        } else {
            value.to_string()
        }
    }
}
