#![cfg_attr(not(feature = "host"), no_std)]


#[cfg(feature = "host")]
use std::io;

#[cfg(feature = "firmware")]
use lpc8xx_hal::{
    prelude::*,
    USART,
    usart,
};

#[cfg(feature = "firmware")]
use nb::block;

use serde::{
    Deserialize,
    Serialize,
};


/// A request sent from the test suite to the firmware on the target
#[derive(Deserialize, Serialize)]
pub enum Request<'r> {
    /// Instruct the device to send a message via USART
    SendUsart(&'r [u8]),
}

impl<'r> Request<'r> {
    /// Send a request to the target, via the provided writer
    ///
    /// - `writer` is where the serialized request is written to.
    /// - `buf` is a buffer used for serialization. It needs to be big enough to
    ///   hold the serialized form of the request.
    ///
    /// This method is only available, if the `host` feature is enabled.
    #[cfg(feature = "host")]
    pub fn send<W: io::Write>(&self, mut writer: W, buf: &mut [u8]) -> Result {
        let serialized = postcard::to_slice_cobs(self, buf)?;
        writer.write_all(serialized)?;
        Ok(())
    }

    /// Receive a request from the target, via the provided USART
    ///
    /// - `usart` is a USART instance that will be used to receive the request.
    /// - `buf` is a buffer that the request is read into, before it is
    ///   deserialized. It needs to be big enough to hold the request.
    ///
    /// This method is only available, if the `firmware` feature is enabled.
    #[cfg(feature = "firmware")]
    pub fn receive<I>(usart: &mut USART<I>, buf: &'r mut [u8]) -> Result<Self>
        where I: usart::Instance
    {
        let mut i = 0;

        // These messages are using COBS encoding, so we know a full message has
        // been read once we receive a `0`.
        loop {
            if i >= buf.len() {
                return Err(Error::BufferTooSmall.into());
            }

            buf[i] = block!(usart.rx().read())?;

            if buf[i] == 0 {
                break;
            }

            i += 1;
        }

        let request = postcard::from_bytes_cobs(buf)?;
        Ok(request)
    }
}


pub type Result<T = ()> = core::result::Result<T, Error>;


#[derive(Debug)]
pub enum Error {
    /// An I/O error occured
    ///
    /// This error is only available, if the `host` feature is enabled.
    #[cfg(feature = "host")]
    Io(io::Error),

    /// An error occured while using USART
    ///
    /// This error is only available, if the `firmware` feature is enabled.
    #[cfg(feature = "firmware")]
    Usart(usart::Error),

    /// An error originated from Postcard
    ///
    /// The `postcard` crate is used for (de-)serialization.
    Postcard(postcard::Error),

    /// The receive buffer is too small to receive a message
    BufferTooSmall,
}

#[cfg(feature = "host")]
impl From<io::Error> for Error {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

#[cfg(feature = "firmware")]
impl From<usart::Error> for Error {
    fn from(err: usart::Error) -> Self {
        Self::Usart(err)
    }
}

impl From<postcard::Error> for Error {
    fn from(err: postcard::Error) -> Self {
        Self::Postcard(err)
    }
}
