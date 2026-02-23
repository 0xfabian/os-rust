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
use requests::{BASE_REVISION, MP_REQUEST};
use sync::SpinLock;

use crate::panic::idle;

const CPU_ID_OFFSET: u64 = 0;

fn get_cpu_id() -> u32 {
    let id: u32;
    unsafe {
        core::arch::asm!(
            "mov {0:e}, gs:[{1}]",
            out(reg) id,
            const CPU_ID_OFFSET,
        );
    }
    id
}

fn setup_cpu(cpu: &limine::mp::Cpu) {
    let per_core_page = alloc_frames(1)
        .map(|frames| phys_to_virt(frames.start))
        .expect("Out of memory");

    x86_64::registers::model_specific::KernelGsBase::write(per_core_page);

    unsafe {
        let cpu_id = (per_core_page.as_u64() + CPU_ID_OFFSET) as *mut u32;
        *cpu_id = cpu.id;
        core::arch::asm!("swapgs");

        IDT.lock().load_unsafe();
    }
}

extern "C" fn core_entry(cpu: &limine::mp::Cpu) -> ! {
    setup_cpu(cpu);

    println!("CPU {} entering idle loop", get_cpu_id());
    idle();
}

extern "x86-interrupt" fn handler(_stack_frame: x86_64::structures::idt::InterruptStackFrame) {
    println!(
        "Breakpoint Exception Handler called on CPU {}",
        get_cpu_id()
    );
}

use requests::MEMORY_MAP_REQUEST;
use x86_64::PhysAddr;
use x86_64::structures::idt::InterruptDescriptorTable;

static IDT: SpinLock<InterruptDescriptorTable> = SpinLock::new(InterruptDescriptorTable::new());

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

    let mp_resp = MP_REQUEST.get_response().unwrap();

    IDT.lock().breakpoint.set_handler_fn(handler);

    for cpu in mp_resp.cpus() {
        if cpu.lapic_id != mp_resp.bsp_lapic_id() {
            cpu.goto_address.write(core_entry);
        } else {
            setup_cpu(cpu);
        }
    }

    println!(
        "interrupt enabled: {}",
        x86_64::instructions::interrupts::are_enabled()
    );

    let cr3 = x86_64::registers::control::Cr3::read();
    println!(
        "CR3 address {:#018x} in {}",
        cr3.0.start_address(),
        which_mem_region(cr3.0.start_address()),
    );

    println!("CPU {} entering idle loop", get_cpu_id());
    idle();
}
