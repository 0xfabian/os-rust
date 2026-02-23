#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

mod logger;
mod panic;
mod pmm;
mod requests;
mod sync;
mod terminal;

use logger::*;
use pmm::*;
use requests::MEMORY_MAP_REQUEST;
use requests::{BASE_REVISION, MP_REQUEST};
use sync::SpinLock;
use x86_64::PhysAddr;
use x86_64::registers::model_specific::GsBase;
use x86_64::structures::idt::InterruptDescriptorTable;

use crate::panic::idle;

struct TlsData {
    cpu_id: u32,
}

static IDT: SpinLock<InterruptDescriptorTable> = SpinLock::new(InterruptDescriptorTable::new());

fn cpu_id() -> u32 {
    // We are in kernel space, GS base should point to TLS data
    let gs_base = GsBase::read();

    if gs_base.is_null() {
        panic!("GS base is null, TLS data not set up");
    }

    let tls_data = unsafe { &*(gs_base.as_u64() as *const TlsData) };
    tls_data.cpu_id
}

// Each CPU gets it's own TLS page, where currently only the CPU ID is stored.
fn setup_tls(cpu: &limine::mp::Cpu) {
    let tls_addr = alloc_frames(1)
        .map(|frames| phys_to_virt(frames.start))
        .expect("Out of memory");

    assert!(core::mem::size_of::<TlsData>() <= 4096);

    let tls_data = unsafe { &mut *(tls_addr.as_mut_ptr::<TlsData>()) };
    *tls_data = TlsData { cpu_id: cpu.id };

    GsBase::write(tls_addr);
}

extern "x86-interrupt" fn handler(_stack_frame: x86_64::structures::idt::InterruptStackFrame) {
    // This is unsafe and could deadlock, but for now, it's ok.
    // Eventually, we should use per CPU buffers and a background thread to print them.
    println!("Breakpoint Exception Handler called on CPU {}", cpu_id());
}

extern "C" fn common_entry(cpu: &limine::mp::Cpu) -> ! {
    // After some experimentation, at this point we know this:
    //  - interrupts are disabled
    //  - IDTR is set to 0
    //  - CR3 is set, is shared among all CPUs, it points to BOOTLOADER_RECLAIMABLE memory
    //  - GDTR, the same as CR3
    //  - GS base and kernel GS base are set to 0

    println!("CPU {} starting up", cpu.id);

    setup_tls(cpu);
    // Only from this point on, the CPU ID can be obtained using the `cpu_id()` function.

    unsafe {
        IDT.lock().load_unsafe();
    }

    println!("CPU {} entering idle loop", cpu_id());
    idle();
}

// Temporary function used to understand the system's state during early boot.
fn which_mem_region(addr: PhysAddr) -> &'static str {
    let regions = MEMORY_MAP_REQUEST.get_response().unwrap().entries();

    for entry in regions {
        if addr.as_u64() >= entry.base && addr.as_u64() < entry.base + entry.length {
            return match entry.entry_type {
                limine::memory_map::EntryType::USABLE => "USABLE",
                limine::memory_map::EntryType::RESERVED => "RESERVED",
                limine::memory_map::EntryType::ACPI_RECLAIMABLE => "ACPI_RECLAIMABLE",
                limine::memory_map::EntryType::ACPI_NVS => "ACPI_NVS",
                limine::memory_map::EntryType::BAD_MEMORY => "BAD_MEMORY",
                limine::memory_map::EntryType::BOOTLOADER_RECLAIMABLE => "BOOTLOADER_RECLAIMABLE",
                _ => "UNKNOWN",
            };
        }
    }

    "NOT FOUND"
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());

    assert!(logger_init());

    pmm_init();

    IDT.lock().breakpoint.set_handler_fn(handler);

    let mp_resp = MP_REQUEST.get_response().unwrap();
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
