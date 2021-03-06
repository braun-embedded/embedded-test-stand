use std::{
    thread::sleep,
    time::{
        Duration,
        Instant,
    },
};

use host_lib::{
    conn::{
        Conn,
        ConnReceiveError,
        ConnSendError,
    },
    pin::{
        Pin,
        ReadLevelError,
    },
};
use lpc845_messages::{
    DmaMode,
    HostToTarget,
    TargetToHost,
    UsartMode,
    pin,
};


/// The connection to the test target
pub struct Target {
    conn: Conn,
    pin: Pin<()>,
}

impl Target {
    pub(crate) fn new(conn: Conn) -> Self {
        Self {
            conn,
            pin: Pin::new(()),
        }
    }

    /// Instruct the target to set a GPIO pin high
    pub fn set_pin_high(&mut self) -> Result<(), TargetSetPinHighError> {
        self.pin
            .set_level::<HostToTarget>(
                pin::Level::High,
                &mut self.conn,
            )
            .map_err(|err| TargetSetPinHighError(err))
    }

    /// Instruct the target to set a GPIO pin low
    pub fn set_pin_low(&mut self) -> Result<(), TargetSetPinLowError> {
        self.pin
            .set_level::<HostToTarget>(
                pin::Level::Low,
                &mut self.conn,
            )
            .map_err(|err| TargetSetPinLowError(err))
    }

    /// Indicates whether the input pin is set high
    ///
    /// Uses `pin_state` internally.
    pub fn pin_is_high(&mut self) -> Result<bool, TargetPinReadError> {
        let pin_state = self.pin.read_level::<HostToTarget, TargetToHost>(
            Duration::from_millis(10),
            &mut self.conn,
        )?;
        Ok(pin_state.0 == pin::Level::High)
    }

    /// Indicates whether the input pin is set low
    ///
    /// Uses `pin_state` internally.
    pub fn pin_is_low(&mut self) -> Result<bool, TargetPinReadError> {
        let pin_state = self.pin.read_level::<HostToTarget, TargetToHost>(
            Duration::from_millis(10),
            &mut self.conn,
        )?;
        Ok(pin_state.0 == pin::Level::Low)
    }

    /// Instruct the target to send this message via USART
    pub fn send_usart(&mut self, data: &[u8])
        -> Result<(), TargetUsartSendError>
    {
        self.conn
            .send(&HostToTarget::SendUsart { mode: UsartMode::Regular, data })
            .map_err(|err| TargetUsartSendError(err))
    }

    /// Instruct the target to send this message via USART using DMA
    pub fn send_usart_dma(&mut self, data: &[u8])
        -> Result<(), TargetUsartSendError>
    {
        self.conn
            .send(&HostToTarget::SendUsart { mode: UsartMode::Dma, data })
            .map_err(|err| TargetUsartSendError(err))
    }

    /// Instruct the target to send this message via USART using DMA
    pub fn send_usart_with_flow_control(&mut self, data: &[u8])
        -> Result<(), TargetUsartSendError>
    {
        self.conn
            .send(&HostToTarget::SendUsart {
                mode: UsartMode::FlowControl,
                data,
            })
            .map_err(|err| TargetUsartSendError(err))
    }

    /// Wait to receive the provided data via USART
    ///
    /// Returns the receive buffer, once the data was received. Returns an
    /// error, if it times out before that, or an I/O error occurs.
    pub fn wait_for_usart_rx(&mut self, data: &[u8], timeout: Duration)
        -> Result<Vec<u8>, TargetUsartWaitError>
    {
        self.wait_for_usart_rx_inner(data, timeout, UsartMode::Regular)
    }

    /// Wait to receive the provided data via USART/DMA
    ///
    /// Returns the receive buffer, once the data was received. Returns an
    /// error, if it times out before that, or an I/O error occurs.
    pub fn wait_for_usart_rx_dma(&mut self, data: &[u8], timeout: Duration)
        -> Result<Vec<u8>, TargetUsartWaitError>
    {
        self.wait_for_usart_rx_inner(data, timeout, UsartMode::Dma)
    }

    fn wait_for_usart_rx_inner(&mut self,
        data:          &[u8],
        timeout:       Duration,
        expected_mode: UsartMode,
    )
        -> Result<Vec<u8>, TargetUsartWaitError>
    {
        let mut buf   = Vec::new();
        let     start = Instant::now();

        loop {
            if buf.windows(data.len()).any(|window| window == data) {
                return Ok(buf);
            }
            if start.elapsed() > timeout {
                return Err(TargetUsartWaitError::Timeout);
            }

            let mut tmp = Vec::new();
            let message = self.conn
                .receive::<TargetToHost>(timeout, &mut tmp)
                .map_err(|err| TargetUsartWaitError::Receive(err))?;

            match message {
                TargetToHost::UsartReceive { mode, data }
                    if mode == expected_mode =>
                {
                    buf.extend(data)
                }
                message => {
                    return Err(
                        TargetUsartWaitError::UnexpectedMessage(
                            format!("{:?}", message)
                        )
                    );
                }
            }
        }
    }

    pub fn read_adc(&mut self) -> Result<u16, ReadAdcError> {
        let timeout = Duration::from_millis(10);

        // Wait for a bit, to give whatever event is expected to change the
        // level some time to happen.
        sleep(timeout);

        self.conn
            .send(&HostToTarget::ReadAdc)
            .map_err(|err| ReadAdcError::Send(err))?;

        let mut buf = Vec::new();
        let reply = self.conn.receive::<TargetToHost>(timeout, &mut buf)
            .map_err(|err| ReadAdcError::Receive(err))?;

        match reply {
            TargetToHost::AdcValue(value) => {
                Ok(value)
            }
            message => {
                Err(
                    ReadAdcError::UnexpectedMessage(
                        format!("{:?}", message)
                    )
                )
            }
        }
    }

    /// Start an I2C transaction
    ///
    /// Sends the provided `data` and returns the reply.
    pub fn start_i2c_transaction(&mut self, data: u8, timeout: Duration)
        -> Result<u8, TargetI2cError>
    {
        let address = 0x48;

        self.conn
            .send(
                &HostToTarget::StartI2cTransaction {
                    mode: DmaMode::Regular,
                    address,
                    data,
                }
            )
            .map_err(|err| TargetI2cError::Send(err))?;

        let mut tmp = Vec::new();
        let message = self.conn
            .receive::<TargetToHost>(timeout, &mut tmp)
            .map_err(|err| TargetI2cError::Receive(err))?;

        match message {
            TargetToHost::I2cReply(reply) => {
                Ok(reply)
            }
            message => {
                Err(
                    TargetI2cError::UnexpectedMessage(
                        format!("{:?}", message)
                    )
                )
            }
        }
    }

    /// Start an SPI transaction
    ///
    /// Sends the provided `data` and returns the reply.
    pub fn start_spi_transaction(&mut self, data: u8, timeout: Duration)
        -> Result<u8, TargetSpiError>
    {
        self.conn
            .send(
                &HostToTarget::StartSpiTransaction {
                    mode: DmaMode::Regular,
                    data,
                }
            )
            .map_err(|err| TargetSpiError::Send(err))?;

        let mut tmp = Vec::new();
        let message = self.conn.receive::<TargetToHost>(timeout, &mut tmp)
            .map_err(|err| TargetSpiError::Receive(err))?;

        match message {
            TargetToHost::SpiReply(reply) => {
                Ok(reply)
            }
            message => {
                Err(
                    TargetSpiError::UnexpectedMessage(
                        format!("{:?}", message)
                    )
                )
            }
        }
    }

    /// Start a timer interrupt with the given period in milliseconds
    pub fn start_timer_interrupt(&mut self, period_ms: u32)
        -> Result<TimerInterrupt, TargetStartTimerInterruptError>
    {
        self.conn
            .send(&HostToTarget::StartTimerInterrupt { period_ms })
            .map_err(|err| TargetStartTimerInterruptError(err))?;

        Ok(TimerInterrupt(self))
    }

    /// Start a PWM signal with the given period in milliseconds
    pub fn start_pwm_signal(&mut self)
        -> Result<PwmSignal, TargetStartPwmSignalError>
    {
        self.conn
            .send(&HostToTarget::StartPwmSignal)
            .map_err(|err| TargetStartPwmSignalError(err))?;

        Ok(PwmSignal(self))
    }
}


/// Represent a timer interrupt that's currently configured on the target
///
/// This timer interrupt will be stopped when this struct is dropped.
pub struct TimerInterrupt<'r>(&'r mut Target);

impl Drop for TimerInterrupt<'_> {
    fn drop(&mut self) {
        (self.0).conn.send(&HostToTarget::StopTimerInterrupt)
            .unwrap()
    }
}

/// Represent an ongoing PWM signal that's currently configured on the target
///
/// This PWM signal will be stopped when this struct is dropped.
pub struct PwmSignal<'r>(&'r mut Target);

impl Drop for PwmSignal<'_> {
    fn drop(&mut self) {
        (self.0).conn.send(&HostToTarget::StopPwmSignal)
            .unwrap()
    }
}


#[derive(Debug)]
pub struct TargetSetPinHighError(ConnSendError);

#[derive(Debug)]
pub struct TargetSetPinLowError(ConnSendError);

#[derive(Debug)]
pub struct TargetPinReadError(ReadLevelError);

impl From<ReadLevelError> for TargetPinReadError {
    fn from(err: ReadLevelError) -> Self {
        Self(err)
    }
}


#[derive(Debug)]
pub struct TargetUsartSendError(ConnSendError);

#[derive(Debug)]
pub enum TargetUsartWaitError {
    Receive(ConnReceiveError),
    Timeout,
    UnexpectedMessage(String),
}

#[derive(Debug)]
pub enum ReadAdcError {
    Send(ConnSendError),
    Receive(ConnReceiveError),
    UnexpectedMessage(String)
}

#[derive(Debug)]
pub enum TargetI2cError {
    Send(ConnSendError),
    Receive(ConnReceiveError),
    UnexpectedMessage(String),
}

#[derive(Debug)]
pub enum TargetSpiError {
    Send(ConnSendError),
    Receive(ConnReceiveError),
    UnexpectedMessage(String),
}

#[derive(Debug)]
pub struct TargetStartTimerInterruptError(ConnSendError);

#[derive(Debug)]
pub struct TargetStartPwmSignalError(ConnSendError);
