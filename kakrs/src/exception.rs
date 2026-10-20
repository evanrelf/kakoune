//! Port of exception.hh
//!
//! C++ exceptions are mapped to `Result<T, Error>`. The `kind` field allows
//! distinguishing the various runtime_error subclasses that are caught
//! specifically somewhere.

use bstr::ByteSlice;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErrorKind {
    /// plain runtime_error (and subclasses that are never caught specifically)
    Runtime,
    /// shell failure
    Failure,
    /// user requested cancellation
    Cancel,
    ChildNotFound,
    NoSelectionsRemaining,
    ParameterError,
    RegexError,
    Disconnected,
    FileAccess,
    /// not a runtime_error: request to kill the session with given status
    KillSession(i32),
    /// not a runtime_error: used in main to switch to client mode
    ConvertToClientMode { session: Vec<u8>, client_name: Vec<u8>, buffer_name: Vec<u8>, selections: Vec<u8> },
    /// not a runtime_error: a logic error / assertion
    Logic,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Error {
    pub kind: ErrorKind,
    what: Vec<u8>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn new(kind: ErrorKind, what: impl Into<Vec<u8>>) -> Self {
        Error { kind, what: what.into() }
    }
    pub fn runtime(what: impl Into<Vec<u8>>) -> Self {
        Error::new(ErrorKind::Runtime, what)
    }
    pub fn failure(what: impl Into<Vec<u8>>) -> Self {
        Error::new(ErrorKind::Failure, what)
    }
    pub fn cancel() -> Self {
        Error::new(ErrorKind::Cancel, "cancellation requested")
    }
    pub fn kill_session(status: i32) -> Self {
        Error::new(ErrorKind::KillSession(status), "kill session")
    }
    pub fn what(&self) -> &[u8] {
        &self.what
    }
    pub fn set_what(&mut self, what: impl Into<Vec<u8>>) {
        self.what = what.into();
    }
    /// true for anything that derives from runtime_error in the C++ code
    pub fn is_runtime(&self) -> bool {
        !matches!(
            self.kind,
            ErrorKind::KillSession(_) | ErrorKind::ConvertToClientMode { .. } | ErrorKind::Logic
        )
    }
    pub fn is_cancel(&self) -> bool {
        self.kind == ErrorKind::Cancel
    }
    pub fn is_failure(&self) -> bool {
        self.kind == ErrorKind::Failure
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Error({:?}, {:?})", self.kind, self.what.as_bstr())
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.what.as_bstr())
    }
}

/// Return early with a runtime error built with kformat!
#[macro_export]
macro_rules! throw {
    ($($arg:tt)*) => {
        return Err($crate::exception::Error::runtime($crate::kformat!($($arg)*)))
    };
}

/// Build a runtime error with kformat!
#[macro_export]
macro_rules! rt_err {
    ($($arg:tt)*) => {
        $crate::exception::Error::runtime($crate::kformat!($($arg)*))
    };
}
