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
use x86_64::PrivilegeLevel;
use x86_64::instructions::segmentation::{CS, DS, ES, FS, GS, SS};
use x86_64::registers::model_specific::GsBase;
use x86_64::registers::segmentation::Segment;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::idt::InterruptDescriptorTable;

use crate::panic::idle;

#[repr(C)]
#[derive(Default)]
struct TlsData {
    cpu_id: u32,
    idt_addr: u64,
    gdt_addr: u64,
}

fn cpu_id() -> u32 {
    let cpu_id: u32;
    unsafe {
        core::arch::asm!("mov {0:e}, gs:[0]", out(reg) cpu_id);
    }
    cpu_id
}

fn get_tls() -> &'static mut TlsData {
    let tls_addr = GsBase::read();
    unsafe { &mut *(tls_addr.as_u64() as *mut TlsData) }
}

// Each CPU gets it's own TLS page, where currently only the CPU ID is stored.
fn setup_tls(cpu: &limine::mp::Cpu) {
    let tls_addr = alloc_frames(1)
        .map(|f| phys_to_virt(f.start))
        .expect("Out of memory");

    assert!(core::mem::size_of::<TlsData>() <= 4096);

    let tls_data = unsafe { &mut *(tls_addr.as_mut_ptr::<TlsData>()) };
    *tls_data = TlsData::default();
    tls_data.cpu_id = cpu.id;

    GsBase::write(tls_addr);
}

// The IDT structure fits perfectly in one page,
// so we allocate one page and store the IDT there.
fn alloc_idt() -> &'static mut InterruptDescriptorTable {
    let idt_addr = alloc_frames(1)
        .map(|f| phys_to_virt(f.start))
        .expect("Out of memory");

    assert!(core::mem::size_of::<InterruptDescriptorTable>() == 4096);

    let idt = unsafe { &mut *(idt_addr.as_mut_ptr::<InterruptDescriptorTable>()) };
    // Effectively zero out the supporting page.
    idt.reset();

    idt
}

fn setup_idt() {
    let idt = alloc_idt();

    populate_idt(idt);
    idt.load();

    let tls = get_tls();
    tls.idt_addr = idt as *const _ as u64;
}

fn populate_idt(idt: &mut InterruptDescriptorTable) {
    unsafe {
        idt.breakpoint
            .set_handler_fn(handler)
            .set_code_selector(KERNEL_CS)
            .set_privilege_level(PrivilegeLevel::Ring3);
    }

    // TODO: set up more handlers, at least for the exceptions.
}

extern "x86-interrupt" fn handler(_stack_frame: x86_64::structures::idt::InterruptStackFrame) {
    // This is unsafe and could deadlock, but for now, it's ok.
    // Eventually, we should use per CPU buffers and a background thread to print them.
    println!("Breakpoint Exception Handler called on CPU {}", cpu_id());
    idle();
}

const KERNEL_CS: SegmentSelector = SegmentSelector::new(1, PrivilegeLevel::Ring0);
const KERNEL_DS: SegmentSelector = SegmentSelector::new(2, PrivilegeLevel::Ring0);
const USER_DS: SegmentSelector = SegmentSelector::new(3, PrivilegeLevel::Ring3);
const USER_CS: SegmentSelector = SegmentSelector::new(4, PrivilegeLevel::Ring3);

// Compared to the IDT, the GDT is usually tiny,
// still, we allocate one page for it, to keep things simple.
fn alloc_gdt() -> &'static mut GlobalDescriptorTable<7> {
    let gdt_addr = alloc_frames(1)
        .map(|f| phys_to_virt(f.start))
        .expect("Out of memory");

    // We need 7 entries:
    //  1. null entry
    //  2. kernel code
    //  3. kernel data
    //  4. user data (not code, due to sysret working in both 32 and 64 bit)
    //  5. user code
    //  6-7. TSS
    assert!(core::mem::size_of::<GlobalDescriptorTable<7>>() < 4096);

    let gdt = unsafe { &mut *(gdt_addr.as_mut_ptr::<GlobalDescriptorTable<7>>()) };
    *gdt = GlobalDescriptorTable::<7>::empty();

    gdt
}

fn setup_gdt() {
    let gdt = alloc_gdt();

    // Null entry is already set.
    gdt.append(Descriptor::kernel_code_segment());
    gdt.append(Descriptor::kernel_data_segment());
    gdt.append(Descriptor::user_data_segment());
    gdt.append(Descriptor::user_code_segment());
    // TODO: set up TSS and it's descriptor.

    gdt.load();

    // Update the segment registers accordingly.
    unsafe {
        CS::set_reg(KERNEL_CS);
        DS::set_reg(KERNEL_DS);
        SS::set_reg(KERNEL_DS);
        ES::set_reg(KERNEL_DS);
        FS::set_reg(KERNEL_DS);

        // Turns out setting GS resets GS base and kernel GS base...
        let gs_base = GsBase::read();
        GS::set_reg(KERNEL_DS);
        GsBase::write(gs_base);
    }

    let tls = get_tls();
    tls.gdt_addr = gdt as *const _ as u64;
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

    // This does a couple of this:
    //  - sets up the GDT structure per CPU
    //  - sets up the segment registers
    setup_gdt();

    // Do it after the GDT, so that we have the right segment registers.
    setup_idt();

    println!("CPU {} entering idle loop", cpu_id());
    idle();
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());

    assert!(logger_init());

    pmm_init();

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
