//! # Common datastructures and methods
//!
//! This module implements core traits for this library
#![warn(missing_docs)]

pub mod borrow;
pub mod buffer;
#[doc(hidden)]
pub mod nom_ext;
pub mod parser;
pub mod reader;
pub mod types;

#[macro_use]
#[doc(hidden)]
pub extern crate num_derive;
#[doc(hidden)]
pub extern crate nom;
#[doc(hidden)]
pub use anyhow;
#[doc(hidden)]
pub use byteorder;
#[doc(hidden)]
pub use displaydoc;
//#[doc(hidden)]
//pub use encoding;
#[doc(hidden)]
pub use num_traits;
