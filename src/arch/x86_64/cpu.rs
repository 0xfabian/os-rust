use crate::memory::pmm::{alloc_frames, phys_to_virt};
use crate::sched::thread::{Queue, Thread};
use crate::sync::SpinLock;
use x86_64::VirtAddr;
use x86_64::instructions::hlt;
use x86_64::registers::model_specific::GsBase;

pub fn idle() -> ! {
    loop {
        hlt();
    }
}

// We need a way for CPUs to access each other.
// Maybe we should inline the tls data here and
// allocate an array based on the number of CPUs.
pub struct CpuData {
    pub online: bool,
    pub tls: u64,
}

pub static CPUS: SpinLock<[CpuData; 256]> = SpinLock::new(
    [const {
        CpuData {
            online: false,
            tls: 0,
        }
    }; 256],
);

pub fn get_cpu_tls(cpu_id: u32) -> Option<&'static TlsData> {
    let cpus = CPUS.lock();
    if cpu_id as usize >= cpus.len() {
        return None;
    }
    if !cpus[cpu_id as usize].online {
        return None;
    }
    Some(unsafe { &*(cpus[cpu_id as usize].tls as *const TlsData) })
}

#[repr(C)]
pub struct TlsData {
    pub cpu_id: u32,
    pub idt_addr: u64, // pointer to IDT structure
    pub gdt_addr: u64, // pointer to GDT structure

    // pointer to the currently running thread struct
    // very unsafe, very much a hack, I need more time with Rust...
    pub thread_addr: u64,

    pub idle_thread: Thread,               // per-cpu idle thread
    pub ready_queue: SpinLock<Queue<u64>>, // queue of pointers to ready threads
}

fn alloc_tls() -> &'static mut TlsData {
    let tls_addr = alloc_frames(1)
        .map(|f| phys_to_virt(f.start))
        .expect("Out of memory");

    assert!(core::mem::size_of::<TlsData>() <= 4096);

    let tls_data = unsafe { &mut *(tls_addr.as_mut_ptr::<TlsData>()) };
    *tls_data = unsafe { core::mem::zeroed() };

    tls_data
}

/// Returns a mutable reference to the current CPU's TLS data via GsBase.
///
/// Must only be called after `setup_tls` has run on this CPU.
pub fn current_cpu() -> &'static mut TlsData {
    unsafe { &mut *(GsBase::read().as_mut_ptr::<TlsData>()) }
}

// Each CPU gets it's own TLS page.
pub fn setup_tls(cpu: &limine::mp::Cpu) {
    let tls = alloc_tls();
    *tls = TlsData {
        cpu_id: cpu.id,
        idt_addr: 0,
        gdt_addr: 0,
        thread_addr: 0,
        idle_thread: Thread::new_idle(),
        ready_queue: SpinLock::new(Queue::new()),
    };

    GsBase::write(VirtAddr::new(tls as *const _ as u64));

    // This is clearly not ideal, but I'm not fighting the borrow checker right now.
    let mut cpus = CPUS.lock();

    cpus[cpu.id as usize].online = true;
    cpus[cpu.id as usize].tls = tls as *const _ as u64;
}

/// Returns the current CPU's id by reading directly from the TLS page via gs.
///
/// This should only be called after TLS is set up.
pub fn current_cpu_id() -> u32 {
    let gs_base = GsBase::read();
    if gs_base.is_null() {
        panic!("TLS not set up yet");
    }

    let cpu_id: u32;
    unsafe {
        core::arch::asm!(
            "mov {0:e}, gs:[{off}]",
            out(reg) cpu_id,
            off = const core::mem::offset_of!(TlsData, cpu_id),
        );
    }
    cpu_id
}
