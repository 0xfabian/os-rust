use crate::memory::pmm::phys_to_virt;
use x86_64::PhysAddr;

pub struct Lapic {
    address: u64, // virtual address
}

impl Lapic {
    pub fn new(address: u64) -> Self {
        Lapic { address }
    }

    pub fn read(&self, offset: u64) -> u32 {
        unsafe { core::ptr::read_volatile((self.address + offset) as *const u32) }
    }

    pub fn write(&self, offset: u64, value: u32) {
        unsafe { core::ptr::write_volatile((self.address + offset) as *mut u32, value) }
    }

    pub fn send_eoi(&self) {
        self.write(0xB0, 0);
    }
}

pub fn lapic_phys_addr() -> u64 {
    const IA32_APIC_BASE_MSR: u32 = 0x1b;
    let msr = x86_64::registers::model_specific::Msr::new(IA32_APIC_BASE_MSR);
    unsafe { msr.read() & !0xfff }
}

pub fn current_lapic() -> Lapic {
    Lapic::new(phys_to_virt(PhysAddr::new(lapic_phys_addr())).as_u64())
}

pub fn setup_lapic_timer() {
    let lapic = current_lapic();
    // Enable it, should be already enabled by limine.
    let svr = lapic.read(0xf0);
    lapic.write(0xf0, svr | (1 << 8));
    // Set timer divider to 16
    lapic.write(0x3e0, 3);

    // Use one-shot mode for now; we manually reset it after each timer interrupt,
    // so threads run consistently for the same amount of time and aren't affected
    // by the time spent in the scheduler waiting on locks and whatnot.
    lapic.write(0x320, 32);

    // Random value that gives decent result on my machine.
    lapic.write(0x380, 0x0fff);
}
