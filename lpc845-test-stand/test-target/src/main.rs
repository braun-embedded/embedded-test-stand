//! Firmware for the LPC845 HAL test suite
//!
//! Needs to be downloaded to the LPC845-BRK board before the test cases can be
//! run.


#![no_main]
#![no_std]


extern crate panic_rtt_target;


use core::marker::PhantomData;

use heapless::spsc;
use lpc8xx_hal::{
    prelude::*,
    Peripherals,
    cortex_m::{
        interrupt,
        peripheral::SYST,
    },
    dma::{
        self,
        transfer::state::Started,
    },
    gpio::{
        GpioPin,
        Level,
        direction::{
            Input,
            Output,
        },
    },
    i2c,
    init_state::Enabled,
    nb::{
        self,
        block,
    },
    pac::{
        I2C0,
        SPI0,
        USART0,
        USART1,
        USART2,
        USART3,
    },
    pinint::{
        self,
        PININT0,
    },
    pins::{
        self,
        Pin,
        PIO0_8,
        PIO0_9,
        PIO0_19,
        PIO1_0,
        PIO1_1,
        PIO1_2,
    },
    spi::{
        self,
        SPI,
    },
    swm::{
        self,
        U1_CTS,
        U1_RTS,
        state::{
            Assigned,
            Unassigned,
        },
    },
    syscon::{
        IOSC,
        frg,
    },
    usart::{
        self,
        state::{
            AsyncMode,
            SyncMode,
        },
    },
};
use rtt_target::rprintln;

#[cfg(feature = "sleep")]
use lpc8xx_hal::cortex_m::asm;

use firmware_lib::usart::{
    RxIdle,
    RxInt,
    Tx,
    Usart,
};
use lpc845_messages::{
    DmaMode,
    HostToTarget,
    TargetToHost,
    UsartMode,
    pin,
};


#[rtic::app(device = lpc8xx_hal::pac)]
const APP: () = {
    struct Resources {
        swm: Option<swm::Handle>,

        host_rx_int:  RxInt<'static, USART0, AsyncMode>,
        host_rx_idle: RxIdle<'static>,
        host_tx:      Tx<USART0, AsyncMode>,

        usart_rx_int:  RxInt<'static, USART1, AsyncMode>,
        usart_rx_idle: RxIdle<'static>,
        usart_tx:      Option<Tx<USART1, AsyncMode>>,
        usart_rts:     Option<swm::Function<U1_RTS, Unassigned>>,
        usart_rts_pin: Option<Pin<PIO0_9, pins::state::Swm<(), ()>>>,
        usart_cts:     Option<swm::Function<U1_CTS, Assigned<PIO0_8>>>,

        usart_sync_rx_int:  RxInt<'static, USART3, SyncMode>,
        usart_sync_rx_idle: RxIdle<'static>,
        usart_sync_tx:      Tx<USART3, SyncMode>,

        green: GpioPin<PIO1_0, Output>,
        blue:  GpioPin<PIO1_1, Output>,
        red:   GpioPin<PIO1_2, Input>,

        red_int: pinint::Interrupt<PININT0, PIO1_2, Enabled>,

        systick: SYST,
        i2c:     Option<i2c::Master<I2C0, Enabled<PhantomData<IOSC>>, Enabled>>,
        i2c_dma: Option<dma::Channel<dma::Channel15, Enabled>>,

        spi:        Option<SPI<SPI0, Enabled<spi::Master>>>,
        ssel:       GpioPin<PIO0_19, Output>,
        spi_rx_dma: Option<dma::Channel<dma::Channel10, Enabled>>,
        spi_tx_dma: Option<dma::Channel<dma::Channel11, Enabled>>,

        usart_dma_tx_channel: Option<dma::Channel<dma::Channel3, Enabled>>,
        usart_dma_rx_transfer: Option<
            dma::Transfer<
                Started,
                dma::Channel4,
                usart::Rx<USART2, usart::state::Enabled<u8, AsyncMode>>,
                &'static mut [u8],
            >
        >,

        dma_rx_prod: spsc::Producer<'static, u8, 32>,
        dma_rx_cons: spsc::Consumer<'static, u8, 32>,
    }

    #[init]
    fn init(context: init::Context) -> init::LateResources {
        // Normally, access to a `static mut` would be unsafe, but we know that
        // this method is only called once, which means we have exclusive access
        // here. RTFM knows this too, and by putting these statics right here,
        // at the beginning of the method, we're opting into some RTFM magic
        // that gives us safe access to them.
        static mut HOST:       Usart = Usart::new();
        static mut USART:      Usart = Usart::new();
        static mut USART_SYNC: Usart = Usart::new();

        static mut DMA_QUEUE: spsc::Queue<u8, 32> = spsc::Queue::new();
        static mut DMA_BUFFER: [u8; 13] = [0; 13];

        rtt_target::rtt_init_print!();
        rprintln!("Starting target.");

        // Get access to the device's peripherals. This can't panic, since this
        // is the only place in this program where we call this method.
        let p = Peripherals::take().unwrap_or_else(|| unreachable!());

        let systick = context.core.SYST;

        let mut syscon = p.SYSCON.split();
        let     swm    = p.SWM.split();
        let     gpio   = p.GPIO.enable(&mut syscon.handle);
        let     pinint = p.PININT.enable(&mut syscon.handle);

        let mut swm_handle = swm.handle.enable(&mut syscon.handle);

        // Configure GPIO pins
        let green = p.pins.pio1_0
            .into_output_pin(gpio.tokens.pio1_0, Level::High);
        let blue = p.pins.pio1_1
            .into_output_pin(gpio.tokens.pio1_1, Level::High);
        let red = p.pins.pio1_2
            .into_input_pin(gpio.tokens.pio1_2);

        // Set up interrupt for input pin
        let mut red_int = pinint
            .interrupts
            .pinint0
            .select(red.inner(), &mut syscon.handle);
        red_int.enable_rising_edge();
        red_int.enable_falling_edge();

        // Configure the clock for USART0, using the Fractional Rate Generator
        // (FRG) and the USART's own baud rate divider value (BRG). See user
        // manual, section 17.7.1.
        //
        // This assumes a system clock of 12 MHz (which is the default and, as
        // of this writing, has not been changed in this program). The resulting
        // rate is roughly 115200 baud.
        let clock_config = {
            syscon.frg0.select_clock(frg::Clock::FRO);
            syscon.frg0.set_mult(22);
            syscon.frg0.set_div(0xFF);
            usart::Clock::new(&syscon.frg0, 5, 16)
        };

        // Assign pins to USART0 for RX/TX functions. On the LPC845-BRK, those
        // are the pins connected to the programmer, and bridged to the host via
        // USB.
        //
        // Careful, the LCP845-BRK documentation uses the opposite designations
        // (i.e. from the perspective of the on-board programmer, not the
        // microcontroller).
        let (u0_rxd, _) = swm.movable_functions.u0_rxd.assign(
            p.pins.pio0_24.into_swm_pin(),
            &mut swm_handle,
        );
        let (u0_txd, _) = swm.movable_functions.u0_txd.assign(
            p.pins.pio0_25.into_swm_pin(),
            &mut swm_handle,
        );

        // Use USART0 to communicate with the test suite
        let mut host = p.USART0.enable_async(
            &clock_config,
            &mut syscon.handle,
            u0_rxd,
            u0_txd,
            usart::Settings::default(),
        );
        host.enable_interrupts(usart::Interrupts {
            RXRDY: true,
            .. usart::Interrupts::default()
        });

        // Assign pins to USART1.
        let (u1_rxd, _) = swm.movable_functions.u1_rxd.assign(
            p.pins.pio0_26.into_swm_pin(),
            &mut swm_handle,
        );
        let (u1_txd, _) = swm.movable_functions.u1_txd.assign(
            p.pins.pio0_27.into_swm_pin(),
            &mut swm_handle,
        );
        let (u1_cts, _) = swm.movable_functions.u1_cts.assign(
            p.pins.pio0_8.into_swm_pin(),
            &mut swm_handle,
        );

        // Use USART1 as the test subject.
        let mut usart = p.USART1.enable_async(
            &clock_config,
            &mut syscon.handle,
            u1_rxd,
            u1_txd,
            usart::Settings::default(),
        );
        usart.enable_interrupts(usart::Interrupts {
            RXRDY: true,
            .. usart::Interrupts::default()
        });

        // Assign pins to USART3.
        let (u3_rxd, _) = swm.movable_functions.u3_rxd.assign(
            p.pins.pio0_13.into_swm_pin(),
            &mut swm_handle,
        );
        let (u3_txd, _) = swm.movable_functions.u3_txd.assign(
            p.pins.pio0_14.into_swm_pin(),
            &mut swm_handle,
        );
        let (u3_sclk, _) = swm.movable_functions.u3_sclk.assign(
            p.pins.pio0_15.into_swm_pin(),
            &mut swm_handle,
        );

        // Use USART3 as secondary test subject for sync mode.
        let mut usart_sync = p.USART3.enable_sync_as_master(
            &usart::Clock::new(&syscon.iosc, 0x03ff, 16),
            &mut syscon.handle,
            u3_rxd,
            u3_txd,
            u3_sclk,
            usart::Settings::default(),
        );
        usart_sync.enable_interrupts(usart::Interrupts {
            RXRDY: true,
            .. usart::Interrupts::default()
        });

        // Assign pins to USART2
        let (u2_rxd, _) = swm.movable_functions.u2_rxd.assign(
            p.pins.pio0_28.into_swm_pin(),
            &mut swm_handle,
        );
        let (u2_txd, _) = swm.movable_functions.u2_txd.assign(
            p.pins.pio0_29.into_swm_pin(),
            &mut swm_handle,
        );

        // Use USART2 as tertiary test subject, for receiving via DMA.
        let usart2 = p.USART2.enable_async(
            &clock_config,
            &mut syscon.handle,
            u2_rxd,
            u2_txd,
            usart::Settings::default(),
        );

        let (host_rx_int,  host_rx_idle,  host_tx)  = HOST.init(host);
        let (usart_rx_int, usart_rx_idle, usart_tx) = USART.init(usart);
        let (usart_sync_rx_int, usart_sync_rx_idle, usart_sync_tx) =
            USART_SYNC.init(usart_sync);

        let (i2c0_sda, _) = swm
            .fixed_functions
            .i2c0_sda
            .assign(p.pins.pio0_11.into_swm_pin(), &mut swm_handle);
        let (i2c0_scl, _) = swm
            .fixed_functions
            .i2c0_scl
            .assign(p.pins.pio0_10.into_swm_pin(), &mut swm_handle);

        let i2c = p.I2C0
            .enable(
                &syscon.iosc,
                i2c0_scl,
                i2c0_sda,
                &mut syscon.handle,
            )
            .enable_master_mode(
                &i2c::Clock::new_400khz(),
            );

        let (spi0_sck, _) = swm
            .movable_functions
            .spi0_sck
            .assign(p.pins.pio0_16.into_swm_pin(), &mut swm_handle);
        let (spi0_mosi, _) = swm
            .movable_functions
            .spi0_mosi
            .assign(p.pins.pio0_17.into_swm_pin(), &mut swm_handle);
        let (spi0_miso, _) = swm
            .movable_functions
            .spi0_miso
            .assign(p.pins.pio0_18.into_swm_pin(), &mut swm_handle);
        let ssel = p.pins.pio0_19.into_output_pin(
            gpio.tokens.pio0_19,
            Level::High,
        );

        let spi = p.SPI0.enable_as_master(
            &spi::Clock::new(&syscon.iosc, 0x0fff),
            &mut syscon.handle,
            spi::MODE_0,
            spi0_sck,
            spi0_mosi,
            spi0_miso,
        );

        let dma = p.DMA.enable(&mut syscon.handle);

        let mut dma_rx_channel = dma.channels.channel4;
        dma_rx_channel.enable_interrupts();
        let mut usart_dma_rx_transfer = usart2.rx
            .read_all(&mut DMA_BUFFER[..], dma_rx_channel);
        usart_dma_rx_transfer.set_a_when_complete();
        let usart_dma_rx_transfer =  usart_dma_rx_transfer.start();

        let (dma_rx_prod, dma_rx_cons) = DMA_QUEUE.split();

        init::LateResources {
            swm: Some(swm_handle),

            host_rx_int,
            host_rx_idle,
            host_tx,

            usart_rx_int,
            usart_rx_idle,
            usart_tx:  Some(usart_tx),
            usart_rts: Some(swm.movable_functions.u1_rts),
            usart_rts_pin: Some(p.pins.pio0_9.into_swm_pin()),
            usart_cts: Some(u1_cts),

            usart_sync_rx_int,
            usart_sync_rx_idle,
            usart_sync_tx,

            green,
            blue,
            red,

            red_int,

            systick,
            i2c:     Some(i2c.master),
            i2c_dma: Some(dma.channels.channel15),

            spi: Some(spi),
            ssel,
            spi_rx_dma: Some(dma.channels.channel10),
            spi_tx_dma: Some(dma.channels.channel11),

            usart_dma_tx_channel:  Some(dma.channels.channel3),
            usart_dma_rx_transfer: Some(usart_dma_rx_transfer),

            dma_rx_prod,
            dma_rx_cons,
        }
    }

    #[idle(resources = [
        swm,
        host_rx_idle, host_tx,
        usart_rx_int, usart_rx_idle, usart_tx,
        usart_rts, usart_rts_pin, usart_cts,
        usart_sync_rx_idle, usart_sync_tx,
        green,
        red,
        systick,
        i2c,
        i2c_dma,
        spi,
        ssel,
        spi_rx_dma,
        spi_tx_dma,
        usart_dma_tx_channel,
        dma_rx_cons,
    ])]
    fn idle(cx: idle::Context) -> ! {
        let swm            = cx.resources.swm;
        let usart_rx       = cx.resources.usart_rx_idle;
        let usart_tx       = cx.resources.usart_tx;
        let usart_rts      = cx.resources.usart_rts;
        let usart_rts_pin  = cx.resources.usart_rts_pin;
        let usart_cts      = cx.resources.usart_cts;
        let usart_sync_rx  = cx.resources.usart_sync_rx_idle;
        let usart_sync_tx  = cx.resources.usart_sync_tx;
        let host_rx        = cx.resources.host_rx_idle;
        let host_tx        = cx.resources.host_tx;
        let green          = cx.resources.green;
        let red            = cx.resources.red;
        let systick        = cx.resources.systick;
        let i2c            = cx.resources.i2c;
        let i2c_dma        = cx.resources.i2c_dma;
        let spi            = cx.resources.spi;
        let ssel           = cx.resources.ssel;
        let spi_rx_dma     = cx.resources.spi_rx_dma;
        let spi_tx_dma     = cx.resources.spi_tx_dma;
        let usart_dma_chan = cx.resources.usart_dma_tx_channel;
        let usart_dma_cons = cx.resources.dma_rx_cons;

        let mut usart_rx_int = cx.resources.usart_rx_int;

        let mut buf = [0; 256];

        loop {
            usart_rx
                .process_raw(|data| {
                    host_tx.send_message(
                        &TargetToHost::UsartReceive {
                            mode: UsartMode::Regular,
                            data,
                        },
                        &mut buf,
                    )
                })
                .expect("Error processing USART data");
            usart_sync_rx
                .process_raw(|data| {
                    host_tx.send_message(
                        &TargetToHost::UsartReceive {
                            mode: UsartMode::Sync,
                            data,
                        },
                        &mut buf,
                    )
                })
                .expect("Error processing USART data (sync)");

            while let Some(b) = usart_dma_cons.dequeue() {
                host_tx
                    .send_message(
                        &TargetToHost::UsartReceive {
                            mode: UsartMode::Dma,
                            data: &[b],
                        },
                        &mut buf,
                    )
                    .unwrap();
            }

            host_rx
                .process_message(|message| {
                    // We're working around two problems here:
                    // 1. We only have a mutable reference to resources we need
                    //    to own. Unfortunately RTIC doesn't allow us to move
                    //    stuff into `idle`, so we need to use the `take`/
                    //    `unwrap` trick to actually move them in here.
                    // 2. Usually we can move things out of variables and back
                    //    into them. As long as the compiler understands that
                    //    we've replaced what we moved out, it won't be a
                    //    problem. The closure prevents that understanding, thus
                    //    necessitating this little dance with the local
                    //    variables.
                    let mut swm_local = swm.take().unwrap();
                    let mut usart_tx_local = usart_tx.take().unwrap();
                    let mut usart_rts_local = usart_rts.take().unwrap();
                    let mut usart_rts_pin_local = usart_rts_pin.take().unwrap();
                    let mut usart_cts_local = usart_cts.take().unwrap();
                    let mut usart_dma_chan_local =
                        usart_dma_chan.take().unwrap();
                    let mut i2c_local = i2c.take().unwrap();
                    let mut i2c_dma_local = i2c_dma.take().unwrap();
                    let mut spi_local = spi.take().unwrap();
                    let mut spi_rx_dma_local = spi_rx_dma.take().unwrap();
                    let mut spi_tx_dma_local = spi_tx_dma.take().unwrap();

                    let result = match message {
                        HostToTarget::SendUsart {
                            mode: UsartMode::Regular,
                            data,
                        } => {
                            usart_tx_local.send_raw(data)
                        }
                        HostToTarget::SendUsart {
                            mode: UsartMode::Dma,
                            data,
                        } => {
                            static mut DMA_BUFFER: [u8; 16] = [0; 16];

                            {
                                // This is sound, as we know this closure is
                                // only ever executed once at a time, and the
                                // mutable reference is dropped at the end of
                                // this block.
                                let dma_buffer = unsafe {
                                    &mut DMA_BUFFER
                                };

                                dma_buffer[..data.len()].copy_from_slice(data);
                            }

                            let payload = {
                                // Sound, as we know this closure is only ever
                                // executed once at a time, and the only other
                                // reference has been dropped already.
                                let dma_buffer = unsafe {
                                    &DMA_BUFFER
                                };

                                let transfer = usart_tx_local.usart.write_all(
                                    &dma_buffer[..data.len()],
                                    usart_dma_chan_local,
                                );
                                transfer
                                    .start()
                                    .wait()
                                    .unwrap()
                            };

                            usart_dma_chan_local = payload.channel;
                            usart_tx_local.usart = payload.dest;

                            Ok(())
                        }
                        HostToTarget::SendUsart {
                            mode: UsartMode::FlowControl,
                            data,
                        } => {
                            rprintln!("USART: Sending with flow control");

                            rprintln!("USART: Enable flow control");
                            let mut usart = usart_tx_local.usart;
                            let (rts, rts_pin) = usart.enable_rts(
                                usart_rts_local,
                                usart_rts_pin_local,
                                &mut swm_local,
                            );
                            let mut usart = usart.enable_cts_throttling(
                                usart_cts_local,
                            );

                            rprintln!("USART: Writing data");
                            usart.bwrite_all(data)
                                .unwrap();

                            rprintln!("USART: Disable flow control");
                            let (rts, rts_pin) = usart.disable_rts(
                                rts,
                                rts_pin,
                                &mut swm_local,
                            );
                            let (usart, cts) = usart
                                .disable_cts_throttling();
                            usart_rts_local = rts;
                            usart_rts_pin_local = rts_pin;
                            usart_cts_local = cts;
                            usart_tx_local.usart = usart;

                            Ok(())
                        }
                        HostToTarget::SendUsart {
                            mode: UsartMode::Sync,
                            data,
                        } => {
                            usart_sync_tx.send_raw(data)
                        }
                        HostToTarget::WaitForAddress(address) => {
                            usart_rx_int.lock(|rx| {
                                rx.usart.start_address_detection(address);
                                block!(rx.usart.read())
                                    .unwrap();
                                rx.usart.stop_address_detection();
                            });
                            Ok(())
                        }
                        HostToTarget::SetPin(
                            pin::SetLevel { level: pin::Level::High, .. }
                        ) => {
                            Ok(green.set_high())
                        }
                        HostToTarget::SetPin(
                            pin::SetLevel { level: pin::Level::Low, .. }
                        ) => {
                            Ok(green.set_low())
                        }
                        HostToTarget::ReadPin(pin::ReadLevel { pin: () }) => {
                            let level = match red.is_high() {
                                true  => pin::Level::High,
                                false => pin::Level::Low,
                            };

                            let result = pin::ReadLevelResult {
                                pin: (),
                                level,
                                period_ms: None,
                            };

                            host_tx
                                .send_message(
                                    &TargetToHost::ReadPinResult(Some(result)),
                                    &mut buf,
                                )
                                .unwrap();

                            Ok(())
                        }
                        HostToTarget::StartTimerInterrupt { period_ms } => {
                            // By default (and we haven't changed that setting)
                            // the SysTick timer runs at half the system
                            // frequency. The system frequency runs at 12 MHz by
                            // default (again, we haven't changed it), meaning
                            // the SysTick timer runs at 6 MHz.
                            //
                            // At 6 MHz, 1 ms are 6000 timer ticks.
                            let reload = period_ms * 6000;
                            systick.set_reload(reload);

                            systick.clear_current();
                            systick.enable_interrupt();
                            systick.enable_counter();

                            Ok(())
                        }
                        HostToTarget::StopTimerInterrupt => {
                            systick.disable_interrupt();
                            systick.disable_counter();

                            Ok(())
                        }
                        HostToTarget::StartI2cTransaction {
                            mode: DmaMode::Regular,
                            address,
                            data,
                        } => {
                            rprintln!("I2C: Write");
                            i2c_local.write(address, &[data])
                                .unwrap();

                            rprintln!("I2C: Read");
                            let mut rx_buf = [0u8; 1];
                            i2c_local.read(address, &mut rx_buf)
                                .unwrap();

                            rprintln!("I2C: Done");

                            host_tx
                                .send_message(
                                    &TargetToHost::I2cReply(rx_buf[0]),
                                    &mut buf,
                                )
                                .unwrap();

                            Ok(())
                        }
                        HostToTarget::StartI2cTransaction {
                            mode: DmaMode::Dma,
                            address,
                            data,
                        } => {
                            static mut TX_BUF: [u8; 1] = [0; 1];
                            static mut RX_BUF: [u8; 1] = [0; 1];

                            // Sound, as we have exclusive access to these
                            // statics here.
                            let tx_buf = unsafe { &mut TX_BUF };
                            let mut rx_buf = unsafe { &mut RX_BUF[..] };


                            tx_buf[0] = data;

                            // Write data to slave
                            let payload = i2c_local
                                .write_all(address, tx_buf, i2c_dma_local)
                                .unwrap()
                                .start()
                                .wait()
                                .unwrap();

                            i2c_dma_local = payload.channel;
                            i2c_local = payload.dest;

                            rx_buf[0] = 0;

                            // Read data from slave
                            let payload = i2c_local
                                .read_all(address, rx_buf, i2c_dma_local)
                                .unwrap()
                                .start()
                                .wait()
                                .unwrap();

                            i2c_dma_local = payload.channel;
                            i2c_local = payload.source;
                            rx_buf = payload.dest;

                            host_tx
                                .send_message(
                                    &TargetToHost::I2cReply(rx_buf[0]),
                                    &mut buf,
                                )
                                .unwrap();

                            Ok(())
                        }
                        HostToTarget::StartSpiTransaction {
                            mode: DmaMode::Regular,
                            data,
                        } => {
                            rprintln!("SPI: Start transaction");
                            ssel.set_low();

                            // Clear receive buffer. Otherwise the following
                            // series of operations won't work as intended.
                            loop {
                                if let Err(nb::Error::WouldBlock) =
                                    spi_local.read()
                                {
                                    break;
                                }
                            }

                            rprintln!("SPI: Write");
                            block!(spi_local.send(data))
                                .unwrap();
                            let _ = block!(spi_local.read())
                                .unwrap();

                            rprintln!("SPI: Read");
                            block!(spi_local.send(0xff))
                                .unwrap();
                            let reply = block!(spi_local.read())
                                .unwrap();

                            ssel.set_high();
                            rprintln!("SPI: Done");

                            host_tx
                                .send_message(
                                    &TargetToHost::SpiReply(reply),
                                    &mut buf,
                                )
                                .unwrap();

                            Ok(())
                        }
                        HostToTarget::StartSpiTransaction {
                            mode: DmaMode::Dma,
                            data,
                        } => {
                            static mut SPI_BUF: [u8; 2] = [0; 2];

                            // Sound, as we have exclusive access to the static
                            // here.
                            let mut spi_buf = unsafe { &mut SPI_BUF[..] };

                            rprintln!("SPI/DMA: Start transaction");
                            ssel.set_low();

                            spi_buf[0] = data;
                            let payload = spi_local
                                .transfer_all(
                                    spi_buf,
                                    spi_rx_dma_local,
                                    spi_tx_dma_local,
                                )
                                .start()
                                .wait();

                            ssel.set_high();

                            spi_local        = payload.0;
                            spi_buf          = payload.1;
                            spi_rx_dma_local = payload.2;
                            spi_tx_dma_local = payload.3;

                            rprintln!(
                                "SPI/DMA: Transaction ended ({})",
                                spi_buf[1],
                            );

                            host_tx
                                .send_message(
                                    &TargetToHost::SpiReply(spi_buf[1]),
                                    &mut buf,
                                )
                                .unwrap();

                            Ok(())
                        }
                        message => {
                            panic!("Unsupported message: {:?}", message)
                        }
                    };

                    *swm = Some(swm_local);
                    *usart_tx = Some(usart_tx_local);
                    *usart_rts = Some(usart_rts_local);
                    *usart_rts_pin = Some(usart_rts_pin_local);
                    *usart_cts = Some(usart_cts_local);
                    *usart_dma_chan = Some(usart_dma_chan_local);
                    *i2c = Some(i2c_local);
                    *i2c_dma = Some(i2c_dma_local);
                    *spi = Some(spi_local);
                    *spi_rx_dma = Some(spi_rx_dma_local);
                    *spi_tx_dma = Some(spi_tx_dma_local);

                    result
                })
                .expect("Error processing host request");
            host_rx.clear_buf();

            // We need this critical section to protect against a race
            // conditions with the interrupt handlers. Otherwise, the following
            // sequence of events could occur:
            // 1. We check the queues here, they're empty.
            // 2. New data is received, an interrupt handler adds it to a queue.
            // 3. The interrupt handler is done, we're back here and going to
            //    sleep.
            //
            // This might not be observable, if something else happens to wake
            // us up before the test suite times out. But it could also lead to
            // spurious test failures.
            interrupt::free(|_| {
                if !host_rx.can_process() && !usart_rx.can_process() {
                    // On LPC84x MCUs, debug mode is not supported when
                    // sleeping. This interferes with RTT communication. Only
                    // sleep, if the user enables this through a compile-time
                    // flag.
                    #[cfg(feature = "sleep")]
                    asm::wfi();
                }
            });
        }
    }

    #[task(binds = USART0, resources = [host_rx_int])]
    fn usart0(cx: usart0::Context) {
        cx.resources.host_rx_int.receive()
            .expect("Error receiving from USART0");
    }

    #[task(binds = USART1, resources = [usart_rx_int])]
    fn usart1(cx: usart1::Context) {
        cx.resources.usart_rx_int.receive()
            .expect("Error receiving from USART1");
    }

    #[task(binds = PIN_INT6_USART3, resources = [usart_sync_rx_int])]
    fn usart3(cx: usart3::Context) {
        cx.resources.usart_sync_rx_int.receive()
            .expect("Error receiving from USART3");
    }

    #[task(binds = SysTick, resources = [blue])]
    fn syst(cx: syst::Context) {
        cx.resources.blue.toggle();
    }

    #[task(binds = PIN_INT0, resources = [red_int])]
    fn pinint0(context: pinint0::Context) {
        let red_int = context.resources.red_int;

        red_int.clear_rising_edge_flag();
        red_int.clear_falling_edge_flag();
    }

    #[task(
        binds = DMA0,
        resources = [
            usart_dma_rx_transfer,
            dma_rx_prod,
        ]
    )]
    fn dma0(context: dma0::Context) {
        let transfer = context.resources.usart_dma_rx_transfer;
        let queue    = context.resources.dma_rx_prod;

        // Process completed transfer.
        let payload = transfer
            .take()
            .unwrap()
            .wait()
            .unwrap();
        let channel = payload.channel;
        let usart   = payload.source;
        let buffer  = payload.dest;

        // Send received data to idle loop.
        for &b in buffer.iter() {
            queue.enqueue(b)
                .unwrap();
        }

        // Restart transfer.
        let mut transfer_ready = usart.read_all(buffer, channel);
        transfer_ready.set_a_when_complete();
        *transfer = Some(transfer_ready.start());
    }
};
