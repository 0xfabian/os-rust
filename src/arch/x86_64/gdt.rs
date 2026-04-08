use crate::arch::x86_64::cpu::TlsData;
use crate::memory::pmm::{alloc_frames, phys_to_virt};
use x86_64::PrivilegeLevel;
use x86_64::instructions::segmentation::{CS, DS, ES, FS, GS, SS};
use x86_64::registers::model_specific::GsBase;
use x86_64::registers::segmentation::Segment;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};

pub const KERNEL_CS: SegmentSelector = SegmentSelector::new(1, PrivilegeLevel::Ring0);
pub const KERNEL_DS: SegmentSelector = SegmentSelector::new(2, PrivilegeLevel::Ring0);
pub const _USER_DS: SegmentSelector = SegmentSelector::new(3, PrivilegeLevel::Ring3);
pub const _USER_CS: SegmentSelector = SegmentSelector::new(4, PrivilegeLevel::Ring3);

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

pub fn setup_gdt() {
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

    unsafe {
        core::arch::asm!(
            "mov gs:[{off}], {0}",
            in(reg) gdt as *const _ as u64,
            off = const core::mem::offset_of!(TlsData, gdt_addr),
        );
    }
}
