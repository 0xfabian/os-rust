use crate::arch::x86_64::cpu::idle;
use crate::println;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    if let Some(location) = info.location() {
        println!(
            "\x1b[91mPANIC at {}:{}\x1b[39m: {}",
            location.file(),
            location.line(),
            info.message()
        );
    } else {
        println!("\x1b[91mPANIC\x1b[39m: {}", info.message());
    }

    idle();
}
