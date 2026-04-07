#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]
#![feature(const_array)]
#![feature(never_type)]

mod logger;
mod panic;
mod pmm;
mod requests;
mod sync;
mod terminal;

use logger::*;
use pmm::*;
use requests::{BASE_REVISION, BOOTLOADER_INFO_REQUEST, MP_REQUEST};
use x86_64::instructions::segmentation::{CS, DS, ES, FS, GS, SS};
use x86_64::registers::model_specific::GsBase;
use x86_64::registers::segmentation::Segment;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::structures::paging::page;
use x86_64::{PhysAddr, PrivilegeLevel, VirtAddr};

use crate::panic::idle;
use crate::requests::FRAMEBUFFER_REQUEST;
use crate::sync::SpinLock;

// We need a way for CPUs to access each other.
// Maybe we should inline the tls data here and
// allocate an array based on the number of CPUs.
struct CpuData {
    online: bool,
    tls: u64,
}

static CPUS: SpinLock<[CpuData; 256]> = SpinLock::new(
    [const {
        CpuData {
            online: false,
            tls: 0,
        }
    }; 256],
);

fn get_cpu_tls(cpu_id: u32) -> Option<&'static TlsData> {
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
struct TlsData {
    cpu_id: u32,
    idt_addr: u64, // pointer to IDT structure
    gdt_addr: u64, // pointer to GDT structure

    // pointer to the currently running thread struct
    // very unsafe, very much a hack, I need more time with Rust
    thread_addr: u64,

    idle_thread: Thread,               // per-cpu idle thread
    ready_queue: SpinLock<Queue<u64>>, // queue of pointers to ready threads
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

macro_rules! current {
    () => {
        unsafe { &mut *(GsBase::read().as_mut_ptr::<TlsData>()) }
    };
}

// Each CPU gets it's own TLS page.
fn setup_tls(cpu: &limine::mp::Cpu) {
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

// This should only be called after TLS is set up, otherwise it will panic.
fn cpu_id() -> u32 {
    // Temporary check, will be removed eventually.
    let gs_base = GsBase::read();
    if gs_base.is_null() {
        panic!("TLS not set up yet");
    }

    let cpu_id: u32;
    unsafe {
        core::arch::asm!("mov {0:e}, gs:[0]", out(reg) cpu_id);
    }
    cpu_id
}

fn thread_id() -> u64 {
    let thread = current!().thread_addr as *const Thread;
    unsafe { (*thread).id }
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

    idt.breakpoint
        .set_handler_fn(breakpoint_handler)
        .set_privilege_level(PrivilegeLevel::Ring3);

    idt.page_fault.set_handler_fn(page_fault_handler);
    idt[32].set_handler_fn(timer_handler);
    // TODO: set up more handlers, at least for the exceptions.

    idt.load();

    // We should use `offset_of!` macro everywhere.
    unsafe {
        core::arch::asm!("mov gs:[8], {0}", in(reg) idt as *const _ as u64);
    }
}

extern "x86-interrupt" fn breakpoint_handler(_stack_frame: InterruptStackFrame) {
    // This is unsafe and could deadlock, but for now, it's ok.
    // Eventually, we should use per CPU buffers and a background thread to print them.
    panic!("Breakpoint Exception triggered on CPU {}", cpu_id());
}

extern "x86-interrupt" fn page_fault_handler(
    _stack_frame: InterruptStackFrame,
    error: PageFaultErrorCode,
) {
    panic!(
        "Page Fault Exception triggered on CPU {}: {:?}",
        cpu_id(),
        error
    );
}

const KERNEL_CS: SegmentSelector = SegmentSelector::new(1, PrivilegeLevel::Ring0);
const KERNEL_DS: SegmentSelector = SegmentSelector::new(2, PrivilegeLevel::Ring0);
const _USER_DS: SegmentSelector = SegmentSelector::new(3, PrivilegeLevel::Ring3);
const _USER_CS: SegmentSelector = SegmentSelector::new(4, PrivilegeLevel::Ring3);

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

    // At this point, the segment registers index bad entries,
    // since the underlying table has changed, isn't this a problem?

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
        core::arch::asm!("mov gs:[16], {0}", in(reg) gdt as *const _ as u64);
    }
}

fn read_tsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") low, out("edx") high);
    }
    ((high as u64) << 32) | (low as u64)
}

fn random_u64() -> u64 {
    // this should be enough entropy
    let mut x = read_tsc();

    // very fast xorshift64
    x ^= x >> 13;
    x ^= x << 7;
    x ^= x >> 17;

    x
}

fn setup_lapic_timer_simple() {
    // Again, this really needs to be cleaned up, maybe store the LAPIC struct in the TLS?
    // Or just have static methods on it.
    const IA32_APIC_BASE_MSR: u32 = 0x1b;
    let lapic_base_msr = x86_64::registers::model_specific::Msr::new(IA32_APIC_BASE_MSR);
    let lapic_addr = unsafe { lapic_base_msr.read() & !0xfff };
    let lapic_vrit_addr = phys_to_virt(PhysAddr::new(lapic_addr)).as_u64();
    let lapic = Lapic::from_address(lapic_vrit_addr);
    // Enable it, should be already enabled by limine.
    let svr = lapic.read(0xf0);
    lapic.write(0xf0, svr | (1 << 8));
    // Set timer divier to 16
    // This is what I've seen on the wiki,
    // doesn't really matter though since we will calibrate this in the future.
    lapic.write(0x3e0, 3);

    // Make it periodic, and use the first vector (32) for the timer interrupt.
    // lapic.write(0x320, (1 << 17) | (32 + 0));

    // Nevermind, let's use the one-shot mode for now.
    // We will manually reset it after each timer interrupt,
    // So threads run consistently for the same amount of time and
    // are not affected by the time spent in the scheduler waiting on locks and whatnot.
    lapic.write(0x320, 32 + 0);

    // Random value that gives decent result on my machine.
    lapic.write(0x380, 0x0fff);
}

// This is from my old kernel, we should have more state here eventually.
#[repr(C)]
struct CpuRegsOnStack {
    // callee-preserved registers
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    rbp: u64,
    rbx: u64,
    // caller-saved registers
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rax: u64,
    rcx: u64,
    rdx: u64,
    rsi: u64,
    rdi: u64,
    // interrupt stack frame
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

impl CpuRegsOnStack {
    fn new_inside_kernel(entry: u64, stack_top: u64) -> Self {
        assert!(stack_top & 0xf == 0, "Stack top must be 16-byte aligned");

        CpuRegsOnStack {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            rbp: stack_top,
            rbx: 0,
            r11: 0,
            r10: 0,
            r9: 0,
            r8: 0,
            rax: 0,
            rcx: 0,
            rdx: 0,
            rsi: 0,
            rdi: 0,
            rip: entry,
            cs: KERNEL_CS.0 as u64,
            rflags: 0x202, // Interrupt Enable flag
            rsp: stack_top,
            ss: KERNEL_DS.0 as u64,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Stack {
    base: u64,
    size: u64,
}

impl Stack {
    fn new(num_pages: usize) -> Self {
        let stack_addr = alloc_frames(num_pages)
            .map(|f| phys_to_virt(f.start))
            .expect("Out of memory");

        Stack {
            base: stack_addr.as_u64(),
            size: (num_pages * 4096) as u64,
        }
    }

    fn get_top_address(&self) -> u64 {
        self.base + self.size
    }
}

use core::arch::naked_asm;
use core::sync::atomic::{AtomicU64, Ordering};

static GLOBAL_THREAD_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

// At this point, I'm just pushing though the pain of not
// implementing a proper allocator.
struct ThreadTable {
    threads: [Option<Thread>; 256],
}

impl ThreadTable {
    const fn new() -> Self {
        ThreadTable {
            threads: [const { None }; 256],
        }
    }

    fn add_thread(&mut self, thread: Thread) -> bool {
        for slot in self.threads.iter_mut() {
            if slot.is_none() {
                *slot = Some(thread);
                return true;
            }
        }
        false
    }

    fn remove_thread(&mut self, id: u64) -> bool {
        for slot in self.threads.iter_mut() {
            if let Some(thread) = slot {
                if thread.id == id {
                    *slot = None;
                    return true;
                }
            }
        }
        false
    }

    fn get_thread(&self, id: u64) -> Option<&Thread> {
        for slot in self.threads.iter() {
            if let Some(thread) = slot {
                if thread.id == id {
                    return Some(thread);
                }
            }
        }
        None
    }

    fn get_thread_mut(&mut self, id: u64) -> Option<&mut Thread> {
        for slot in self.threads.iter_mut() {
            if let Some(thread) = slot {
                if thread.id == id {
                    return Some(thread);
                }
            }
        }
        None
    }
}

static GLOBAL_THREAD_TABLE: SpinLock<ThreadTable> = SpinLock::new(ThreadTable::new());

use core::mem::MaybeUninit;

// Simple ring buffer based queue, with a fixed cap. Temporary.
struct Queue<T> {
    data: [MaybeUninit<T>; 256],
    head: usize,
    tail: usize,
    len: usize,
}

impl<T> Queue<T> {
    pub const fn new() -> Self {
        Self {
            data: [const { MaybeUninit::uninit() }; 256],
            head: 0,
            tail: 0,
            len: 0,
        }
    }

    pub fn push(&mut self, item: T) -> Result<(), T> {
        if self.len == 256 {
            return Err(item); // full
        }

        self.data[self.tail].write(item);
        self.tail = (self.tail + 1) % 256;
        self.len += 1;
        Ok(())
    }

    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }

        let item = unsafe { self.data[self.head].assume_init_read() };
        self.head = (self.head + 1) % 256;
        self.len -= 1;

        Some(item)
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

#[derive(Debug)]
enum ThreadState {
    Born,
    Ready,
    Running,
    Blocked,
    Zombie,
}

#[derive(Debug)]
struct SchedState {
    cpu: Option<u32>,
    state: ThreadState,
}

// Right now this represents a kernel thread.
// Obivously in the future we'll want user space support too,
// So rename this to `Task` or something.
#[repr(C)]
#[derive(Debug)]
struct Thread {
    regs: u64,
    id: u64,
    kernel_stack: Stack,
    sched_state: SpinLock<SchedState>,
}

extern "C" fn idle_thread_entry() -> ! {
    idle();
}

// With the current design, the scheduler must always run in a valid thread context
// and the initial setup functions (kmain and common_entry) don't run in a thread context
// so we force a switch to the idle thread.
// In the process we lose the stack allocated by limine, we'll recover it later if we ever need it.
fn force_switch_to_idle() -> ! {
    let idle_thread = &current!().idle_thread;
    current!().thread_addr = idle_thread as *const _ as u64;
    unsafe {
        core::arch::asm!(
            "
            mov rsp, {0}
            pop r15
            pop r14
            pop r13
            pop r12
            pop rbp
            pop rbx
            pop r11
            pop r10
            pop r9
            pop r8
            pop rax
            pop rcx
            pop rdx
            pop rsi
            pop rdi
            iretq
            ",
            in(reg) idle_thread.regs,
        );
    }
    panic!("This should never be reached");
}

trait ThreadGasket {
    unsafe fn run_and_cleanup(&self, ptr: *mut ()) -> !;
}

impl<F> ThreadGasket for F
where
    F: FnOnce() -> !,
{
    unsafe fn run_and_cleanup(&self, ptr: *mut ()) -> ! {
        // 1. Cast the raw pointer back to the specific closure type
        let closure_ptr = ptr as *mut F;
        // 2. Move the closure out of the allocated memory onto the stack
        let closure = core::ptr::read(closure_ptr);
        // 3. Call it!
        closure();
    }
}

extern "C" fn thread_trampoline(arg: *mut ()) -> ! {
    unsafe {
        // 1. The fat pointer is stored at the start of the page.
        // It contains: [Pointer to Data, Pointer to VTable]
        let dyn_ptr = *(arg as *mut *mut dyn ThreadGasket);

        // 2. Call the gasket.
        // We pass 'arg' because that's where the actual closure 'F' lives.
        (*dyn_ptr).run_and_cleanup(arg);
    }

    // This is unreachable currently, but in the future,
    // the closure will return.

    let virt_addr = VirtAddr::new(arg as u64);
    let phys_addr = virt_to_phys(virt_addr);

    free_frames(FrameRange {
        start: phys_addr,
        count: 1,
    });

    // This should be actually exit()
    idle();
}

impl Thread {
    // Move this on the ready queue of the current CPU and mark it as ready.
    fn ready(&self) {
        let mut sched_state = self.sched_state.lock();
        sched_state.state = ThreadState::Ready;
        sched_state.cpu = Some(current!().cpu_id);
        let mut ready_queue = current!().ready_queue.lock();
        ready_queue
            .push(self as *const _ as u64)
            .expect("Ready queue is full");
    }

    // Spawns a new thread with the given entry point returning it's id.
    // The thread will then be marked as ready and scheduled to run on the current CPU.
    fn low_level_spawn(entry: extern "C" fn(*mut ()) -> !, arg: *mut ()) -> Option<u64> {
        let id = GLOBAL_THREAD_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        let stack = Stack::new(4); // 4 pages should be enough
        let mut regs = CpuRegsOnStack::new_inside_kernel(entry as u64, stack.get_top_address());
        regs.rdi = arg as u64;

        // write the register to the stack
        let regs_addr = stack.get_top_address() - core::mem::size_of::<CpuRegsOnStack>() as u64;
        unsafe {
            core::ptr::write_volatile(regs_addr as *mut CpuRegsOnStack, regs);
        }

        let thread = Thread {
            id,
            kernel_stack: stack,
            regs: regs_addr,
            sched_state: SpinLock::new(SchedState {
                cpu: None,
                state: ThreadState::Born,
            }),
        };

        if GLOBAL_THREAD_TABLE.lock().add_thread(thread) == false {
            return None;
        }

        GLOBAL_THREAD_TABLE
            .lock()
            .get_thread_mut(id)
            .unwrap()
            .ready();
        Some(id)
    }

    fn spawn<F>(f: F) -> Option<u64>
    where
        F: FnOnce() -> ! + Send + 'static,
    {
        assert!(core::mem::size_of::<F>() < 4096 - 64); // ensure the closure fits in one page with the vtable pointer
        let page_addr = alloc_frames(1)
            .map(|f| phys_to_virt(f.start))
            .expect("Out of memory");

        unsafe {
            let fat_ptr_slot = page_addr.as_mut_ptr::<*mut dyn ThreadGasket>();
            // High alignment???
            let closure_slot = (page_addr.as_u64() + 64) as *mut F;

            closure_slot.write(f);
            let dyn_ptr: *mut dyn ThreadGasket = closure_slot as *mut dyn ThreadGasket;
            fat_ptr_slot.write(dyn_ptr);
        }

        Self::low_level_spawn(thread_trampoline, page_addr.as_mut_ptr::<()>())
    }

    // Create an idle thread
    fn new_idle() -> Self {
        let stack = Stack::new(1);
        let regs = CpuRegsOnStack::new_inside_kernel(
            idle_thread_entry as *const () as u64,
            stack.get_top_address(),
        );

        // write the register to the stack
        let regs_addr = stack.get_top_address() - core::mem::size_of::<CpuRegsOnStack>() as u64;
        unsafe {
            core::ptr::write_volatile(regs_addr as *mut CpuRegsOnStack, regs);
        }

        Thread {
            id: 0,
            kernel_stack: stack,
            regs: regs_addr,
            sched_state: SpinLock::new(SchedState {
                cpu: None,
                state: ThreadState::Ready,
            }),
        }
    }
}

fn switch() {
    let cpu = current!();
    let idle_thread_addr = (&cpu.idle_thread) as *const _ as u64;

    let mut ready_queue = cpu.ready_queue.lock();

    let next_thread_addr = match ready_queue.pop() {
        Some(addr) => addr,
        None => return, // no ready thread, continue running the current one (including the idle thread)
    };

    if cpu.thread_addr != idle_thread_addr {
        // add it back to the ready queue since it got preempted
        ready_queue
            .push(cpu.thread_addr)
            .expect("Ready queue is full");
    }

    cpu.thread_addr = next_thread_addr;
}

fn do_load_balance(qa: &mut Queue<u64>, qb: &mut Queue<u64>, cpu_id: u32, other_cpu_id: u32) {
    let delta = qa.len as isize - qb.len as isize;

    if delta.abs() <= 1 {
        return; // balanced enough, also avoids oscillations
    }

    let to_move = (delta.abs() / 2) as usize;

    if delta > 0 {
        // move from a to b
        for _ in 0..to_move {
            if let Some(thread_addr) = qa.pop() {
                let thread = unsafe { &*(thread_addr as *const Thread) };
                thread.sched_state.lock().cpu = Some(other_cpu_id);
                qb.push(thread_addr).expect("Ready queue is full");
            }
        }
        println!(
            "Moved {} threads from CPU {} to CPU {}",
            to_move, cpu_id, other_cpu_id
        );
    } else {
        // move from b to a
        for _ in 0..to_move {
            if let Some(thread_addr) = qb.pop() {
                let thread = unsafe { &*(thread_addr as *const Thread) };
                thread.sched_state.lock().cpu = Some(cpu_id);
                qa.push(thread_addr).expect("Ready queue is full");
            }
        }
        println!(
            "Moved {} threads from CPU {} to CPU {}",
            to_move, other_cpu_id, cpu_id
        );
    }
}

fn get_random_online_cpu() -> Option<&'static TlsData> {
    // I have 12 cores,
    let id = random_u64() as u32 % 12 /* 256 */;
    get_cpu_tls(id)
}

// This is a very simple load balancing stratgy,
// randomly pick another cpu and then move
// half of the imbalance from the busier cpu to the less busy one.
// The idea is that after a couple of ticks, the load should be balanced enough.
//
// Right now, we assume:
//  - the cost of moving threads is negiligible
//  - thread weigh the same, so only the number of threads matters
//  - core affinity (though after the threads are balanced, they will not move much)
//
fn maybe_load_balance() {
    let other_cpu = match get_random_online_cpu() {
        Some(cpu) => cpu,
        None => return, // no other online CPU, can't load balance
    };

    let cpu = current!();

    if cpu.cpu_id == other_cpu.cpu_id {
        // we happned to pick ourselves, no load balancing needed
        return;
    }

    // to avoid deadlocks, we need to lock in a consistent order,
    // so let the cpu id decide the order.
    if cpu.cpu_id < other_cpu.cpu_id {
        let mut ready_queue = cpu.ready_queue.lock();
        let mut other_ready_queue = other_cpu.ready_queue.lock();
        do_load_balance(
            &mut ready_queue,
            &mut other_ready_queue,
            cpu.cpu_id,
            other_cpu.cpu_id,
        );
    } else {
        let mut other_ready_queue = other_cpu.ready_queue.lock();
        let mut ready_queue = cpu.ready_queue.lock();
        do_load_balance(
            &mut ready_queue,
            &mut other_ready_queue,
            cpu.cpu_id,
            other_cpu.cpu_id,
        );
    }
}

extern "C" fn actual_timer_handler() {
    // switch the context
    switch();
    // and perform load balancing.
    // This should definitely not be done on every tick.
    maybe_load_balance();

    let lapic = Lapic::from_address(phys_to_virt(PhysAddr::new(0xfee00000)).as_u64());
    // sched the next interrupt
    lapic.write(0x380, 0x0fff);
    lapic.send_eoi();
}

#[unsafe(naked)]
extern "x86-interrupt" fn timer_handler(_stack_frame: InterruptStackFrame) {
    // Very very basic thread switching.
    naked_asm!(
        "
        push rdi
        push rsi
        push rdx
        push rcx
        push rax
        push r8
        push r9
        push r10
        push r11
        push rbx
        push rbp
        push r12
        push r13
        push r14
        push r15
        mov rax, gs:[24]   // the address of the current thread struct
        mov [rax + 0], rsp // save the current stack pointer (pointer to regs struct)
        call {handler}
        mov rax, gs:[24]   // reload the thread struct address, since the handler might have switched the thread
        mov rsp, [rax + 0] // restore the stack pointer from the thread struct
        pop r15
        pop r14
        pop r13
        pop r12
        pop rbp
        pop rbx
        pop r11
        pop r10
        pop r9
        pop r8
        pop rax
        pop rcx
        pop rdx
        pop rsi
        pop rdi
        iretq
        ",
        handler = sym actual_timer_handler,
    );
}

fn setup_sched() {
    // empty for now
}

const NUM_THREADS: usize = 10;

// Used to split the screen in a grid of n x m, where n * m = NUM_THREADS and n and m are as close as possible.
fn split_closest_divisors(n: usize) -> (usize, usize) {
    let mut best_delta = n - 1;
    let mut best_pair = (1, n);

    for i in 2..n {
        if n % i == 0 {
            let m = n / i;

            if i > m {
                break;
            }

            let delta = m - i;

            if delta < best_delta {
                best_delta = delta;
                best_pair = (i, m);
            }
        }
    }

    best_pair
}

// Get the start and end indices for the given part of a range of total size `total` split in `parts` parts.
fn get_range_slice(total: usize, parts: usize, index: usize) -> (usize, usize) {
    let (part_size, remainder) = (total / parts, total % parts);
    let start = index * part_size + usize::min(index, remainder);
    let end = start + part_size + if index < remainder { 1 } else { 0 };
    (start, end)
}

// don't take this too seriously, it's just for testing the scheduler
static mut DRAW_AREA: [u32; 1920 * 1080] = [0; 1920 * 1080];

struct Rect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

fn fill_rect(rect: Rect, color: u32) {
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            let offset = y * 1920 + x;
            unsafe {
                core::ptr::write(&mut DRAW_AREA[offset], color);
            }
        }
    }
}

fn get_random(min: usize, max: usize) -> usize {
    min + (random_u64() as usize % (max - min))
}

fn get_random_rect_in_screen() -> Rect {
    let width = get_random(10, 11);
    let height = get_random(10, 11);
    let x = get_random(0, 1920 - width);
    let y = get_random(0, 1080 - height);
    Rect {
        x,
        y,
        width,
        height,
    }
}

fn example_thread() -> ! {
    loop {
        let rect = get_random_rect_in_screen();
        let color = random_u64() as u32;
        fill_rect(rect, color);
    }
}

// Is `rep movsq` really that fast?
#[unsafe(naked)]
pub unsafe extern "C" fn fast_blit(dst: *mut u8, src: *const u8, bytes: usize) {
    core::arch::naked_asm!(
        "
        mov rcx, rdx
        shr rcx, 3        // bytes / 8
        rep movsq
        ret
        "
    );
}

// This completely ignores the "terminal" logic but whatever.
fn blit_thread() -> ! {
    let fb_resp = FRAMEBUFFER_REQUEST
        .get_response()
        .expect("Framebuffer request failed");

    let fb = fb_resp.framebuffers().next().expect("No framebuffer found");
    let fb_addr = fb.addr() as *mut u32;
    let src = core::ptr::addr_of!(DRAW_AREA) as *const u8;

    loop {
        unsafe {
            fast_blit(fb_addr as *mut u8, src, 1920 * 1080 * 4);
        }
    }
}

extern "C" fn common_entry(cpu: &limine::mp::Cpu) -> ! {
    // After some experimentation, at this point we know this:
    //  - interrupts are disabled
    //  - IDTR is set to 0
    //  - CR3 is set, is shared among all CPUs, it points to BOOTLOADER_RECLAIMABLE memory
    //  - GDTR, the same as CR3
    //  - GS base and kernel GS base are set to 0

    println!("CPU {} (lapic_id: {}) starting up", cpu.id, cpu.lapic_id);

    setup_tls(cpu);
    // Only from this point on, the CPU ID can be obtained using the `cpu_id()` function.

    // This does a couple of this:
    //  - sets up the GDT structure per CPU
    //  - sets up the segment registers
    setup_gdt();

    // Do it after the GDT, so that we have the right segment registers.
    setup_idt();

    setup_lapic_timer_simple();

    setup_sched();

    if cpu_id() == 0 {
        for _ in 0..NUM_THREADS {
            Thread::spawn(|| {
                loop {
                    println!("Hello from thread {} on CPU {}", thread_id(), cpu_id());
                }
            });
        }
    }

    // At this point we lose the current stack allocated by limine,
    // but that's ok for now.
    // After this call, the CPU will run in a context suitable for context switching.
    // This will also enable interrupts!
    force_switch_to_idle();
}

struct Lapic {
    address: u64, // virtual address
}

impl Lapic {
    fn from_address(address: u64) -> Self {
        Lapic { address }
    }

    fn read(&self, offset: u64) -> u32 {
        unsafe { core::ptr::read_volatile((self.address + offset) as *const u32) }
    }

    fn write(&self, offset: u64, value: u32) {
        unsafe { core::ptr::write_volatile((self.address + offset) as *mut u32, value) }
    }

    fn send_eoi(&self) {
        self.write(0xB0, 0);
    }
}

// Just a mess and probably very buggy.
// This is only used to map the LAPIC registers.
// Obviously, we will need a proper VMM.
fn map_page(phys: u64, virt: u64) {
    let (cr3, _) = x86_64::registers::control::Cr3::read();
    let pml4 = unsafe {
        &mut *(phys_to_virt(cr3.start_address())
            .as_mut_ptr::<x86_64::structures::paging::PageTable>())
    };
    let pml4_index = (virt >> 39) & 0o777;
    let pml4_entry = &mut pml4[pml4_index as usize];
    // pml4_entry.set_addr(
    //     PhysAddr::new(phys),
    //     x86_64::structures::paging::PageTableFlags::PRESENT
    //         | x86_64::structures::paging::PageTableFlags::WRITABLE,
    // );

    let pdpt = unsafe {
        &mut *(phys_to_virt(pml4_entry.addr())
            .as_mut_ptr::<x86_64::structures::paging::PageTable>())
    };
    let pdpt_index = (virt >> 30) & 0o777;
    let pdpt_entry = &mut pdpt[pdpt_index as usize];

    if !pdpt_entry
        .flags()
        .contains(x86_64::structures::paging::PageTableFlags::PRESENT)
    {
        let new_page = alloc_frames(1).unwrap();

        pdpt_entry.set_addr(
            new_page.start,
            x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE,
        );
    }

    let pd = unsafe {
        &mut *(phys_to_virt(pdpt_entry.addr())
            .as_mut_ptr::<x86_64::structures::paging::PageTable>())
    };
    let pd_index = (virt >> 21) & 0o777;
    let pd_entry = &mut pd[pd_index as usize];

    if !pd_entry
        .flags()
        .contains(x86_64::structures::paging::PageTableFlags::PRESENT)
    {
        let new_page = alloc_frames(1).unwrap();

        pd_entry.set_addr(
            new_page.start,
            x86_64::structures::paging::PageTableFlags::PRESENT
                | x86_64::structures::paging::PageTableFlags::WRITABLE,
        );
    }

    let pt = unsafe {
        &mut *(phys_to_virt(pd_entry.addr()).as_mut_ptr::<x86_64::structures::paging::PageTable>())
    };
    let pt_index = (virt >> 12) & 0o777;
    let pt_entry = &mut pt[pt_index as usize];

    pt_entry.set_addr(
        PhysAddr::new(phys),
        x86_64::structures::paging::PageTableFlags::PRESENT
            | x86_64::structures::paging::PageTableFlags::WRITABLE,
    );
}

#[unsafe(no_mangle)]
extern "C" fn kmain() -> ! {
    assert!(BASE_REVISION.is_supported());

    assert!(logger_init());
    // From this point on, println! will actually print something.

    let bootloader_info_resp = BOOTLOADER_INFO_REQUEST.get_response();
    if let Some(resp) = bootloader_info_resp {
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

    pmm_init();

    const IA32_APIC_BASE_MSR: u32 = 0x1b;
    let lapic_base_msr = x86_64::registers::model_specific::Msr::new(IA32_APIC_BASE_MSR);
    // I also experminted with the ACPI RSDP table, and we should probably use that
    // to find the LAPIC address since I don't think the MSR is a portable way to get it.
    let lapic_addr = unsafe { lapic_base_msr.read() & !0xfff };
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
