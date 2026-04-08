use crate::memory::{alloc_frame, phys_to_virt};
use x86_64::PhysAddr;
use x86_64::structures::paging::{PageTable, PageTableFlags};

// Just a mess and probably very buggy.
// This is only used to map the LAPIC registers.
// Obviously, we will need a proper VMM.
pub fn map_page(phys: u64, virt: u64) {
    let (cr3, _) = x86_64::registers::control::Cr3::read();
    let pml4 = unsafe { &mut *(phys_to_virt(cr3.start_address()).as_mut_ptr::<PageTable>()) };
    let pml4_index = (virt >> 39) & 0o777;
    let pml4_entry = &mut pml4[pml4_index as usize];

    let pdpt = unsafe { &mut *(phys_to_virt(pml4_entry.addr()).as_mut_ptr::<PageTable>()) };
    let pdpt_index = (virt >> 30) & 0o777;
    let pdpt_entry = &mut pdpt[pdpt_index as usize];

    if !pdpt_entry.flags().contains(PageTableFlags::PRESENT) {
        let new_page = alloc_frame().unwrap();

        pdpt_entry.set_addr(
            new_page.addr(),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );
    }

    let pd = unsafe { &mut *(phys_to_virt(pdpt_entry.addr()).as_mut_ptr::<PageTable>()) };
    let pd_index = (virt >> 21) & 0o777;
    let pd_entry = &mut pd[pd_index as usize];

    if !pd_entry.flags().contains(PageTableFlags::PRESENT) {
        let new_page = alloc_frame().unwrap();

        pd_entry.set_addr(
            new_page.addr(),
            PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
        );
    }

    let pt = unsafe { &mut *(phys_to_virt(pd_entry.addr()).as_mut_ptr::<PageTable>()) };
    let pt_index = (virt >> 12) & 0o777;
    let pt_entry = &mut pt[pt_index as usize];

    pt_entry.set_addr(
        PhysAddr::new(phys),
        PageTableFlags::PRESENT | PageTableFlags::WRITABLE,
    );
}
