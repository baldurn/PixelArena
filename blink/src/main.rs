#![no_std]
#![no_main]

use esp_hal::{
    gpio::{Level, Output, OutputConfig},
    main,
    time::{Duration, Instant},
};
use esp_println::println;

esp_bootloader_esp_idf::esp_app_desc!();

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("panic: {}", info);
    loop {}
}

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());

    // External LED on IO32 (via resistor to GND)
    let mut led = Output::new(peripherals.GPIO32, Level::Low, OutputConfig::default());

    println!("blink: started, LED on IO32");
    let mut count: u32 = 0;
    loop {
        led.toggle();
        count += 1;
        println!("blink {}", count);
        let start = Instant::now();
        while start.elapsed() < Duration::from_millis(500) {}
    }
}
