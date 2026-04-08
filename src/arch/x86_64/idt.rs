use crate::arch::x86_64::cpu::{TlsData, current_cpu_id};
use crate::memory::{alloc_frame, phys_to_virt};
use crate::sched::timer_interrupt;
use x86_64::PrivilegeLevel;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

// The IDT structure fits perfectly in one page,
// so we allocate one page and store the IDT there.
fn alloc_idt() -> &'static mut InterruptDescriptorTable {
    let idt_addr = alloc_frame()
        .map(|f| phys_to_virt(f.addr()))
        .expect("Out of memory");

    assert!(core::mem::size_of::<InterruptDescriptorTable>() == 4096);

    let idt = unsafe { &mut *(idt_addr.as_mut_ptr::<InterruptDescriptorTable>()) };
    // Effectively zero out the supporting page.
    idt.reset();

    idt
}

pub fn setup_idt() {
    let idt = alloc_idt();

    idt.breakpoint
        .set_handler_fn(breakpoint_handler)
        .set_privilege_level(PrivilegeLevel::Ring3);

    idt.page_fault.set_handler_fn(page_fault_handler);
    idt[32].set_handler_fn(timer_interrupt);
    // TODO: set up more handlers, at least for the exceptions.

    idt.load();

    unsafe {
        core::arch::asm!(
            "mov gs:[{off}], {0}",
            in(reg) idt as *const _ as u64,
            off = const core::mem::offset_of!(TlsData, idt_addr),
        );
    }
}

extern "x86-interrupt" fn breakpoint_handler(_stack_frame: InterruptStackFrame) {
    panic!("Breakpoint Exception triggered on CPU {}", current_cpu_id());
}

extern "x86-interrupt" fn page_fault_handler(
    _stack_frame: InterruptStackFrame,
    error: PageFaultErrorCode,
) {
    panic!(
        "Page Fault Exception triggered on CPU {}: {:?}",
        current_cpu_id(),
        error
    );
}
