#![no_std]
#![no_main]

use defmt::*;
use embassy_executor::Spawner;
use embassy_futures::{
    poll_once,
    select::{select, Either},
    yield_now,
};
use embassy_rp::{
    adc, bind_interrupts, gpio,
    peripherals::{self, USB},
    spi, usb,
};
use embassy_time::{Duration, Ticker, Timer};
use futures::{future::Ready, FutureExt};
use {defmt_rtt as _, panic_probe as _};

bind_interrupts!(struct Irqs {
    USBCTRL_IRQ => usb::InterruptHandler<peripherals::USB>;
    ADC_IRQ_FIFO => adc::InterruptHandler;
});

// requires use of log, and this solution supresses defmt logs
//#[embassy_executor::task]
//async fn logger_task(driver: usb::Driver<'static, USB>) {
//    info!("Hello Logger!");
//    embassy_usb_logger::run!(1024, log::LevelFilter::Info, driver);
//}

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let peripherals = embassy_rp::init(Default::default());
    let _usb_driver = usb::Driver::new(peripherals.USB, Irqs);

    info!("Start");

    //spawner.must_spawn(logger_task(usb_driver));

    let mut ticker = Ticker::every(Duration::from_millis(50));

    loop {
        info!("Hello World!");
        ticker.next().await;
    }
}
