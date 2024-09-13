#![no_std]
#![no_main]

use defmt::*;
use {defmt_rtt as _, panic_probe as _};

use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_rp::{adc, bind_interrupts, peripherals, usb};
use embassy_usb::class::{cdc_acm::CdcAcmClass, hid::HidReaderWriter};
use static_cell::StaticCell;
use usbd_hid::descriptor::{KeyboardReport, MouseReport, SerializedDescriptor};

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => usb::InterruptHandler<peripherals::USB>;
    ADC_IRQ_FIFO => adc::InterruptHandler;
});

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let peripherals = embassy_rp::init(Default::default());
    let usb_driver = usb::Driver::new(peripherals.USB, Irqs);

    info!("Start");

    let mut usb_config = embassy_usb::Config::new(0xc0de, 0xcafe);
    usb_config.manufacturer = Some("theLegy");
    usb_config.product = Some("HidBridge");

    let mut usb_builder = {
        static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
        static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
        static MSOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
        static CONTROL_BUF: StaticCell<[u8; 64]> = StaticCell::new();
        embassy_usb::Builder::new(
            usb_driver,
            usb_config,
            CONFIG_DESCRIPTOR.init([0; 256]),
            BOS_DESCRIPTOR.init([0; 256]),
            MSOS_DESCRIPTOR.init([0; 256]),
            CONTROL_BUF.init([0; 64]),
        )
    };

    let usb_serial = {
        static STATE: StaticCell<embassy_usb::class::cdc_acm::State> = StaticCell::new();
        let state = STATE.init(Default::default());
        CdcAcmClass::new(&mut usb_builder, state, 64)
    };
    let (mut usb_serial_sender, mut usb_serial_receiver, mut usb_serial_control) =
        usb_serial.split_with_control();

    let mut usb_keyboard = {
        static STATE: StaticCell<embassy_usb::class::hid::State> = StaticCell::new();
        let state = STATE.init(Default::default());
        let config = embassy_usb::class::hid::Config {
            report_descriptor: KeyboardReport::desc(),
            request_handler: None,
            poll_ms: 10,
            max_packet_size: 64,
        };
        HidReaderWriter::<_, 1, 8>::new(&mut usb_builder, state, config)
    };

    let mut usb_mouse = {
        static STATE: StaticCell<embassy_usb::class::hid::State> = StaticCell::new();
        let state = STATE.init(Default::default());
        let config = embassy_usb::class::hid::Config {
            report_descriptor: MouseReport::desc(),
            request_handler: None,
            poll_ms: 10,
            max_packet_size: 64,
        };
        HidReaderWriter::<_, 1, 8>::new(&mut usb_builder, state, config)
    };

    let mut usb = usb_builder.build();

    let usb_fut = usb.run();

    let serial_fut = async {
        let mut line_buf = [0u8; 64];
        let mut line_len = 0;
        loop {
            let mut data = [0u8; 64];
            let len = unwrap!(usb_serial_receiver.read_packet(&mut data).await);
            info!("Received {} bytes: {}", len, data[..len]);
            for i in 0..len {
                if data[i] == 13 {
                    let line = &line_buf[..line_len];
                    info!("Received line of length {}: {}", line_len, line);
                    line_len = 0;
                    unwrap!(usb_serial_sender.write_packet(b"> ").await);
                    unwrap!(usb_serial_sender.write_packet(line).await);
                    unwrap!(usb_serial_sender.write_packet(b"\r\n").await);
                    if let Ok(line) = core::str::from_utf8(line) {
                        info!("Received line: {}", line);
                    }
                    send_q(&mut usb_keyboard).await;
                } else if line_len < 64 {
                    line_buf[line_len] = data[i];
                    line_len += 1;
                }
            }
        }
    };

    join(usb_fut, serial_fut).await;
}

async fn send_q<'d, D, const READ_N: usize, const WRITE_N: usize>(
    hid: &mut HidReaderWriter<'d, D, READ_N, WRITE_N>,
) where
    D: embassy_usb::driver::Driver<'d>,
{
    let mut report = KeyboardReport::default();
    report.keycodes[0] = 0x14;
    unwrap!(hid.write_serialize(&report).await);
    unwrap!(hid.write_serialize(&KeyboardReport::default()).await);
}
