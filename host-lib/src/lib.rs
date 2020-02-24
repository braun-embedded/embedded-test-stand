//! Library to support the test suite running on the host computer


pub mod error;
pub mod receive;
pub mod send;


pub use self::{
    error::{
        Error,
        Result,
    },
    receive::receive,
    send::send,
};