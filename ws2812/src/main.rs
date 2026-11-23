#![no_std]
#![no_main]

use esp_hal::{
    main,
    rmt::Rmt,
    time::{Duration, Instant, Rate},
};
use esp_hal_smartled::{smart_led_buffer, SmartLedsAdapter};
use esp_println::println;
use smart_leds::{
    brightness, gamma,
    hsv::{hsv2rgb, Hsv},
    SmartLedsWrite, RGB8,
};

esp_bootloader_esp_idf::esp_app_desc!();

const NUM_LEDS: usize = 144;
// Global brightness (0-255). Full white on 144 LEDs draws ~8.6 A at 5 V;
// 32 caps it at roughly 1 A worst case.
const BRIGHTNESS: u8 = 32;
const FRAME_TIME: Duration = Duration::from_millis(20);

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    println!("panic: {}", info);
    loop {}
}

#[main]
fn main() -> ! {
    let peripherals = esp_hal::init(esp_hal::Config::default());

    let rmt = Rmt::new(peripherals.RMT, Rate::from_mhz(80)).unwrap();
    let mut buffer = smart_led_buffer!(NUM_LEDS);
    let mut strip = SmartLedsAdapter::new(rmt.channel0, peripherals.GPIO2, &mut buffer);

    println!("ws2812: {} LEDs on GPIO2, brightness {}", NUM_LEDS, BRIGHTNESS);

    let mut pixels = [RGB8::default(); NUM_LEDS];
    let mut offset: u8 = 0;
    let mut frames: u32 = 0;
    loop {
        let start = Instant::now();

        // Rainbow spread over the whole strip, scrolling one hue step per frame
        for (i, px) in pixels.iter_mut().enumerate() {
            let hue = offset.wrapping_add((i * 256 / NUM_LEDS) as u8);
            *px = hsv2rgb(Hsv { hue, sat: 255, val: 255 });
        }
        if let Err(e) = strip.write(brightness(gamma(pixels.iter().cloned()), BRIGHTNESS)) {
            println!("write error: {:?}", e);
        }

        offset = offset.wrapping_add(1);
        frames += 1;
        if frames % 250 == 0 {
            println!("frames: {}", frames);
        }
        while start.elapsed() < FRAME_TIME {}
    }
}
