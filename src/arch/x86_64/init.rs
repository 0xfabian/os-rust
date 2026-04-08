use crate::arch::x86_64::cpu::setup_tls;
use crate::arch::x86_64::gdt::setup_gdt;
use crate::arch::x86_64::idt::setup_idt;
use crate::arch::x86_64::lapic::setup_lapic_timer;
use crate::println;
use crate::sched::force_switch_to_idle;

pub extern "C" fn common_entry(cpu: &limine::mp::Cpu) -> ! {
    // After some experimentation, at this point we know this:
    //  - interrupts are disabled
    //  - IDTR is set to 0
    //  - CR3 is set, is shared among all CPUs, it points to BOOTLOADER_RECLAIMABLE memory
    //  - GDTR, the same as CR3
    //  - GS base and kernel GS base are set to 0

    println!("CPU {} (lapic_id: {}) starting up", cpu.id, cpu.lapic_id);

    setup_tls(cpu);
    // Only from this point on, the CPU id can be obtained via current_cpu_id().

    setup_gdt();
    setup_idt();
    setup_lapic_timer();

    // After this call, the CPU will run in a context suitable for context switching.
    // This will also enable interrupts!
    force_switch_to_idle();
}
