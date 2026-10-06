//! Phoenix terminal styling and ANSI helpers.

pub fn dim(s: &str) -> String {
    format!("\x1b[2m{s}\x1b[0m")
}

pub fn bold(s: &str) -> String {
    format!("\x1b[1m{s}\x1b[0m")
}

pub fn italic(s: &str) -> String {
    format!("\x1b[3m{s}\x1b[0m")
}

pub fn red(s: &str) -> String {
    format!("\x1b[1;31m{s}\x1b[0m")
}

pub fn green(s: &str) -> String {
    format!("\x1b[1;32m{s}\x1b[0m")
}

pub fn cyan(s: &str) -> String {
    format!("\x1b[1;36m{s}\x1b[0m")
}

pub fn magenta(s: &str) -> String {
    format!("\x1b[1;35m{s}\x1b[0m")
}

pub fn rule(width: usize) -> String {
    dim(&"─".repeat(width.min(72)))
}
