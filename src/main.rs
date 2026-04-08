#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(const_array)]
#![feature(never_type)]

mod arch;
mod boot;
mod dev;
mod memory;
mod panic;
mod sched;
mod sync;
mod util;

use crate::arch::x86_64::init::common_entry;
use crate::arch::x86_64::lapic::lapic_phys_addr;
use crate::boot::requests::{BASE_REVISION, BOOTLOADER_INFO_REQUEST, MP_REQUEST};
use crate::dev::console;
use crate::memory::phys_to_virt;
use crate::memory::vmm::map_page;
use x86_64::PhysAddr;

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());

    assert!(console::init());
    // From this point on, println! will actually print something.

    if let Some(resp) = BOOTLOADER_INFO_REQUEST.get_response() {
        println!(
            "\x1b[93mBootloader: {} {}\x1b[m",
            resp.name(),
            resp.version()
        );
    }

    println!(
        "\x1b[93mProtocol base revision: {}\x1b[m",
        BASE_REVISION.loaded_revision().unwrap()
    );

    memory::init();

    // I also experimented with the ACPI RSDP table, and we should probably use that
    // to find the LAPIC address since the MSR isn't a portable way to get it.
    let lapic_addr = lapic_phys_addr();
    map_page(lapic_addr, phys_to_virt(PhysAddr::new(lapic_addr)).as_u64());

    let mp_resp = MP_REQUEST.get_response().expect("No MP response");
    let mut bsp_cpu = None;

    for cpu in mp_resp.cpus() {
        if cpu.lapic_id != mp_resp.bsp_lapic_id() {
            cpu.goto_address.write(common_entry);
        } else {
            bsp_cpu = Some(cpu);
        }
    }

    common_entry(bsp_cpu.expect("BSP CPU not found"));
}
