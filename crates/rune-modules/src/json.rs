//! The native `json` module for the [Rune Language].
//!
//! [Rune Language]: https://rune-rs.github.io
//!
//! ## Usage
//!
//! Add the following to your `Cargo.toml`:
//!
//! ```toml
//! rune-modules = { version = "0.14.0", features = ["json"] }
//! ```
//!
//! Install it into your context:
//!
//! ```rust
//! let mut context = rune::Context::with_default_modules()?;
//! context.install(rune_modules::json::module(true)?)?;
//! # Ok::<_, rune::support::Error>(())
//! ```
//!
//! Use it in Rune:
//!
//! ```rust,ignore
//! use json;
//!
//! fn main() {
//!     let data = json::from_string("{\"key\": 42}");
//!     dbg(data);
//! }
//! ```

use rune::alloc::fmt::TryWrite;
use rune::alloc::{self, String, Vec};
use rune::runtime::{Bytes, Formatter, Value};
use rune::{nested_try, Any, ContextError, Module};

#[rune::module(::json)]
/// Module for processing JSON.
///
/// # Examples
///
/// ```rune
/// let object = #{"number": 42, "string": "Hello World"};
/// let object = json::from_string(json::to_string(object)?)?;
/// assert_eq!(object, #{"number": 42, "string": "Hello World"});
/// ```
pub fn module(_stdio: bool) -> Result<Module, ContextError> {
    let mut m = Module::from_meta(self::module__meta)?;
    m.ty::<Error>()?;
    m.function_meta(Error::display)?;
    m.function_meta(Error::debug)?;
    m.function_meta(from_bytes)?;
    m.function_meta(from_string)?;
    m.function_meta(to_string)?;
    m.function_meta(to_bytes)?;
    Ok(m)
}

#[derive(Any)]
#[rune(item = ::json)]
/// Error type raised during JSON serialization.
struct Error {
    error: serde_json::Error,
}

impl Error {
    #[rune::function(protocol = DISPLAY_FMT)]
    pub(crate) fn display(&self, f: &mut Formatter) -> alloc::Result<()> {
        write!(f, "{}", self.error)
    }

    #[rune::function(protocol = DEBUG_FMT)]
    pub(crate) fn debug(&self, f: &mut Formatter) -> alloc::Result<()> {
        write!(f, "{:?}", self.error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self { error }
    }
}

/// Convert JSON bytes into a rune value.
///
/// # Examples
///
/// ```rune
/// let object = json::from_bytes(b"{\"number\": 42, \"string\": \"Hello World\"}")?;
/// assert_eq!(object, #{"number": 42, "string": "Hello World"});
/// ```
#[rune::function]
fn from_bytes(bytes: &[u8]) -> Result<Value, Error> {
    Ok(serde_json::from_slice(bytes)?)
}

/// Convert a JSON string into a rune value.
///
/// # Examples
///
/// ```rune
/// let object = json::from_string("{\"number\": 42, \"string\": \"Hello World\"}")?;
/// assert_eq!(object, #{"number": 42, "string": "Hello World"});
/// ```
#[rune::function]
fn from_string(string: &str) -> Result<Value, Error> {
    Ok(serde_json::from_str(string)?)
}

/// Convert any value to a json string.
///
/// # Examples
///
/// ```rune
/// let object = #{"number": 42, "string": "Hello World"};
/// let object = json::from_string(json::to_string(object)?)?;
/// assert_eq!(object, #{"number": 42, "string": "Hello World"});
/// ```
#[rune::function]
fn to_string(value: Value) -> alloc::Result<Result<String, Error>> {
    Ok(Ok(String::try_from(nested_try!(serde_json::to_string(
        &value
    )))?))
}

/// Convert any value to json bytes.
///
/// # Examples
///
/// ```rune
/// let object = #{"number": 42, "string": "Hello World"};
/// let object = json::from_bytes(json::to_bytes(object)?)?;
/// assert_eq!(object, #{"number": 42, "string": "Hello World"});
/// ```
#[rune::function]
fn to_bytes(value: Value) -> alloc::Result<Result<Bytes, Error>> {
    Ok(Ok(Bytes::from_vec(Vec::try_from(nested_try!(
        serde_json::to_vec(&value)
    ))?)))
}
