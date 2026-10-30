use std::fmt;
use std::io::{self, Write};

#[derive(Debug)]
pub struct Error(io::Error);

impl Error {
    pub fn is_broken_pipe(&self) -> bool {
        self.0.kind() == io::ErrorKind::BrokenPipe
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Self(error)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.0)
    }
}

pub fn line(args: fmt::Arguments<'_>) -> Result<(), Error> {
    tracing_indicatif::suspend_tracing_indicatif(|| {
        writeln!(io::stdout().lock(), "{args}").map_err(Error::from)
    })
}

macro_rules! outputln {
    ($($arg:tt)*) => {
        $crate::output::line(format_args!($($arg)*))?
    };
}

pub(crate) use outputln;
