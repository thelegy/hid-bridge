#![no_std]
#![no_main]
#![allow(clippy::needless_range_loop)]

use defmt::{todo, *};
use embedded_io_async::{Read, Write};
use heapless::Vec;
use input_events::{InputEvent, InputEventKind, Key, RelAxis};
use {defmt_rtt as _, panic_probe as _};

use core::result::Result;
use embassy_executor::Spawner;
use embassy_futures::join::join;
use embassy_rp::{adc, bind_interrupts, peripherals, uart, usb};
use embassy_usb::{
    class::{
        cdc_acm::{self, CdcAcmClass},
        hid::HidReaderWriter,
    },
    driver::EndpointError,
};
use static_cell::StaticCell;
use usbd_hid::descriptor::{KeyboardReport, MouseReport, SerializedDescriptor};

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => usb::InterruptHandler<peripherals::USB>;
    ADC_IRQ_FIFO => adc::InterruptHandler;
    UART1_IRQ => uart::BufferedInterruptHandler<peripherals::UART1>;
});

#[embassy_executor::task]
async fn relay_uart(
    mut tx: uart::BufferedUartTx<'static, peripherals::UART1>,
    mut rx: cdc_acm::Receiver<'static, usb::Driver<'static, peripherals::USB>>,
) {
    loop {
        let mut data = [0u8; 65];
        let len = rx.read_packet(&mut data).await.unwrap();
        if len > 0 {
            Write::write_all(&mut tx, &data[0..len]).await.unwrap();
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    let peripherals = embassy_rp::init(Default::default());
    let usb_driver = usb::Driver::new(peripherals.USB, Irqs);

    info!("Start");

    let mut usb_config = embassy_usb::Config::new(0xc0de, 0xcafe);
    usb_config.manufacturer = Some("theLegy");
    usb_config.product = Some("HidBridge");

    let (uart_tx, mut uart_rx) = {
        let config: uart::Config = Default::default();
        const RX_BUF_SIZE: usize = 2048;
        static TX_BUF: StaticCell<[u8; 256]> = StaticCell::new();
        static RX_BUF: StaticCell<[u8; RX_BUF_SIZE]> = StaticCell::new();
        uart::BufferedUart::new(
            peripherals.UART1,
            Irqs,
            peripherals.PIN_8,
            peripherals.PIN_9,
            TX_BUF.init([0; 256]),
            RX_BUF.init([0; RX_BUF_SIZE]),
            config,
        )
        .split()
    };

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

    let (_usb_serial_sender, usb_serial_receiver, _usb_serial_control) = {
        static STATE: StaticCell<embassy_usb::class::cdc_acm::State> = StaticCell::new();
        let state = STATE.init(Default::default());
        CdcAcmClass::new(&mut usb_builder, state, 64).split_with_control()
    };

    unwrap!(spawner.spawn(relay_uart(uart_tx, usb_serial_receiver)));

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

    let serial_fut = handle_serial(&mut uart_rx, &mut usb_keyboard, &mut usb_mouse);

    join(usb_fut, serial_fut).await;
}

async fn handle_serial<'d, T, D, const READ_N: usize, const WRITE_N: usize>(
    serial_receiver: &mut uart::BufferedUartRx<'d, T>,
    keyboard: &mut HidReaderWriter<'d, D, READ_N, WRITE_N>,
    mouse: &mut HidReaderWriter<'d, D, READ_N, WRITE_N>,
) -> Result<(), EndpointError>
where
    T: uart::Instance,
    D: embassy_usb::driver::Driver<'d>,
{
    const EMPTY_MOUSE_REPORT: MouseReport = MouseReport {
        buttons: 0,
        x: 0,
        y: 0,
        wheel: 0,
        pan: 0,
    };
    let mut line_buf = [0u8; 64];
    let mut line_len = 0;
    let mut pressed_keys = Vec::<Key, 32>::new();
    let mut mouse_report = EMPTY_MOUSE_REPORT;
    loop {
        let mut data = [0u8; 64];
        let len = Read::read(serial_receiver, &mut data).await.unwrap();
        info!("Received {} bytes: {:#?}", len, &data[..len]);
        for i in 0..len {
            let byte = data[i];
            if byte == 0 {
                info!("Now decoding: {:x}", &line_buf[..line_len]);
                if let event = (postcard::from_bytes_cobs(&mut line_buf[..line_len])
                    as Result<InputEvent, _>)
                    .unwrap()
                {
                    info!("Received event of length {}: {}", line_len, event);
                    line_len = 0;
                    match event.kind() {
                        InputEventKind::SynEvent(input_events::Syn::SynReport) => {
                            keyboard
                                .write_serialize(&build_keyboard_report(&pressed_keys))
                                .await?;
                            mouse.write_serialize(&mouse_report).await?;
                            mouse_report = EMPTY_MOUSE_REPORT;
                        }
                        InputEventKind::KeyEvent(key) => {
                            if key.is_btn() {
                                if event.value == 1 {
                                    match key {
                                        Key::BtnLeft => {
                                            mouse_report.buttons |= 1 << 0;
                                        }
                                        Key::BtnRight => {
                                            mouse_report.buttons |= 1 << 1;
                                        }
                                        Key::BtnMiddle => {
                                            mouse_report.buttons |= 1 << 2;
                                        }
                                        _ => {}
                                    }
                                } else if event.value == 0 {
                                    match key {
                                        Key::BtnLeft => {
                                            mouse_report.buttons &= !(1 << 0);
                                        }
                                        Key::BtnRight => {
                                            mouse_report.buttons &= !(1 << 1);
                                        }
                                        Key::BtnMiddle => {
                                            mouse_report.buttons &= !(1 << 2);
                                        }
                                        _ => {}
                                    }
                                }
                            } else if event.value == 1 {
                                if !pressed_keys.contains(&key) {
                                    pressed_keys.push(key).unwrap();
                                }
                            } else if event.value == 0 {
                                pressed_keys.retain(|x| x != &key);
                            }
                        }
                        InputEventKind::RelEvent(axis) => {
                            let value = event.value.clamp(i8::MIN as i32, i8::MAX as i32) as i8;
                            match axis {
                                RelAxis::RelX => {
                                    mouse_report.x = value;
                                }
                                RelAxis::RelY => {
                                    mouse_report.y = value;
                                }
                                RelAxis::RelWheel => {
                                    mouse_report.wheel = value;
                                }
                                _ => {}
                            }
                        }
                        _ => (),
                    }
                }
            } else if line_len < 64 {
                line_buf[line_len] = byte;
                line_len += 1;
            }
        }
    }
}

fn build_keyboard_report(keys: &[Key]) -> KeyboardReport {
    let mut report = KeyboardReport::default();
    let mut keys_in_report = 0;
    for key in keys {
        match key {
            Key::KeyLeftctrl => {
                report.modifier |= 1 << 0;
            }
            Key::KeyLeftshift => {
                report.modifier |= 1 << 1;
            }
            Key::KeyLeftalt => {
                report.modifier |= 1 << 2;
            }
            Key::KeyLeftmeta => {
                report.modifier |= 1 << 3;
            }
            Key::KeyRightctrl => {
                report.modifier |= 1 << 4;
            }
            Key::KeyRightshift => {
                report.modifier |= 1 << 5;
            }
            Key::KeyRightalt => {
                report.modifier |= 1 << 6;
            }
            Key::KeyRightmeta => {
                report.modifier |= 1 << 7;
            }
            key => {
                if let Some(code) = key_to_usb_code(key) {
                    if keys_in_report < 6 {
                        report.keycodes[keys_in_report] = code;
                        keys_in_report += 1;
                    } else {
                        report.keycodes = [1; 6];
                    }
                }
            }
        }
    }
    report
}

fn key_to_usb_code(key: &Key) -> Option<u8> {
    match key {
        Key::KeyA => Some(0x04),
        Key::KeyB => Some(0x05),
        Key::KeyC => Some(0x06),
        Key::KeyD => Some(0x07),
        Key::KeyE => Some(0x08),
        Key::KeyF => Some(0x09),
        Key::KeyG => Some(0x0A),
        Key::KeyH => Some(0x0B),
        Key::KeyI => Some(0x0C),
        Key::KeyJ => Some(0x0D),
        Key::KeyK => Some(0x0E),
        Key::KeyL => Some(0x0F),
        Key::KeyM => Some(0x10),
        Key::KeyN => Some(0x11),
        Key::KeyO => Some(0x12),
        Key::KeyP => Some(0x13),
        Key::KeyQ => Some(0x14),
        Key::KeyR => Some(0x15),
        Key::KeyS => Some(0x16),
        Key::KeyT => Some(0x17),
        Key::KeyU => Some(0x18),
        Key::KeyV => Some(0x19),
        Key::KeyW => Some(0x1A),
        Key::KeyX => Some(0x1B),
        Key::KeyY => Some(0x1C),
        Key::KeyZ => Some(0x1D),
        Key::Key1 => Some(0x1E),
        Key::Key2 => Some(0x1F),
        Key::Key3 => Some(0x20),
        Key::Key4 => Some(0x21),
        Key::Key5 => Some(0x22),
        Key::Key6 => Some(0x23),
        Key::Key7 => Some(0x24),
        Key::Key8 => Some(0x25),
        Key::Key9 => Some(0x26),
        Key::Key0 => Some(0x27),
        Key::KeyEnter => Some(0x28),
        Key::KeyEsc => Some(0x29),
        Key::KeyBackspace => Some(0x2A),
        Key::KeyTab => Some(0x2B),
        Key::KeySpace => Some(0x2C),
        Key::KeyMinus => Some(0x2D),
        Key::KeyEqual => Some(0x2E),
        Key::KeyLeftbrace => Some(0x2F),
        Key::KeyRightbrace => Some(0x30),
        Key::KeyBackslash => Some(0x31),
        Key::KeyNumericPound => Some(0x32),
        Key::KeySemicolon => Some(0x33),
        Key::KeyApostrophe => Some(0x34),
        Key::KeyGrave => Some(0x35),
        Key::KeyComma => Some(0x36),
        Key::KeyDot => Some(0x37),
        Key::KeySlash => Some(0x38),
        Key::KeyCapslock => Some(0x39),
        Key::KeyF1 => Some(0x3A),
        Key::KeyF2 => Some(0x3B),
        Key::KeyF3 => Some(0x3C),
        Key::KeyF4 => Some(0x3D),
        Key::KeyF5 => Some(0x3E),
        Key::KeyF6 => Some(0x3F),
        Key::KeyF7 => Some(0x40),
        Key::KeyF8 => Some(0x41),
        Key::KeyF9 => Some(0x42),
        Key::KeyF10 => Some(0x43),
        Key::KeyF11 => Some(0x44),
        Key::KeyF12 => Some(0x45),
        Key::KeySysrq => Some(0x46),
        Key::KeyScrolllock => Some(0x47),
        Key::KeyPause => Some(0x48),
        Key::KeyInsert => Some(0x49),
        Key::KeyHome => Some(0x4A),
        Key::KeyPageup => Some(0x4B),
        Key::KeyDelete => Some(0x4C),
        Key::KeyEnd => Some(0x4D),
        Key::KeyPagedown => Some(0x4E),
        Key::KeyRight => Some(0x4F),
        Key::KeyLeft => Some(0x50),
        Key::KeyDown => Some(0x51),
        Key::KeyUp => Some(0x52),
        Key::KeyNumlock => Some(0x53),
        Key::KeyKpslash => Some(0x54),
        Key::KeyKpasterisk => Some(0x55),
        Key::KeyKpminus => Some(0x56),
        Key::KeyKpplus => Some(0x57),
        Key::KeyKpenter => Some(0x58),
        Key::KeyKp1 => Some(0x59),
        Key::KeyKp2 => Some(0x5A),
        Key::KeyKp3 => Some(0x5B),
        Key::KeyKp4 => Some(0x5C),
        Key::KeyKp5 => Some(0x5D),
        Key::KeyKp6 => Some(0x5E),
        Key::KeyKp7 => Some(0x5F),
        Key::KeyKp8 => Some(0x60),
        Key::KeyKp9 => Some(0x61),
        Key::KeyKp0 => Some(0x62),
        Key::KeyKpdot => Some(0x63),
        Key::Key102nd => Some(0x64),
        Key::KeyCompose => Some(0x65),
        Key::KeyPower => Some(0x66),
        Key::KeyKpequal => Some(0x67),
        Key::KeyF13 => Some(0x68),
        Key::KeyF14 => Some(0x69),
        Key::KeyF15 => Some(0x6A),
        Key::KeyF16 => Some(0x6B),
        Key::KeyF17 => Some(0x6C),
        Key::KeyF18 => Some(0x6D),
        Key::KeyF19 => Some(0x6E),
        Key::KeyF20 => Some(0x6F),
        Key::KeyF21 => Some(0x70),
        Key::KeyF22 => Some(0x71),
        Key::KeyF23 => Some(0x72),
        Key::KeyF24 => Some(0x73),
        key => {
            info!("Processed unknown key: {}", key);
            None
        }
    }
}
