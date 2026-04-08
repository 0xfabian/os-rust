use crate::arch::x86_64::cpu::{current_cpu, idle};
use crate::arch::x86_64::gdt::{KERNEL_CS, KERNEL_DS};
use crate::memory::{alloc_frame, alloc_frames, phys_to_virt};
use crate::sync::SpinLock;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicU64, Ordering};

#[repr(C)]
pub struct CpuRegsOnStack {
    // callee-preserved registers
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    // caller-saved registers
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rsi: u64,
    pub rdi: u64,
    // interrupt stack frame
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

impl CpuRegsOnStack {
    pub fn new_inside_kernel(entry: u64, stack_top: u64) -> Self {
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
pub struct Stack {
    pub base: u64,
    pub size: u64,
}

impl Stack {
    pub fn new(num_pages: usize) -> Self {
        let stack_addr = alloc_frames(num_pages)
            .map(|f| phys_to_virt(f.start.addr()))
            .expect("Out of memory");

        Stack {
            base: stack_addr.as_u64(),
            size: (num_pages * 4096) as u64,
        }
    }

    pub fn get_top_address(&self) -> u64 {
        self.base + self.size
    }
}

static GLOBAL_THREAD_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

// At this point, I'm just pushing through the pain of not
// implementing a proper allocator.
pub struct ThreadTable {
    threads: [Option<Thread>; 256],
}

impl ThreadTable {
    pub const fn new() -> Self {
        ThreadTable {
            threads: [const { None }; 256],
        }
    }

    pub fn add_thread(&mut self, thread: Thread) -> bool {
        for slot in self.threads.iter_mut() {
            if slot.is_none() {
                *slot = Some(thread);
                return true;
            }
        }
        false
    }

    pub fn get_thread_mut(&mut self, id: u64) -> Option<&mut Thread> {
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

pub static GLOBAL_THREAD_TABLE: SpinLock<ThreadTable> = SpinLock::new(ThreadTable::new());

// Simple ring buffer based queue, with a fixed cap. Temporary.
pub struct Queue<T> {
    data: [MaybeUninit<T>; 256],
    head: usize,
    tail: usize,
    pub len: usize,
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
}

#[derive(Debug)]
#[allow(dead_code)]
pub enum ThreadState {
    Born,
    Ready,
    Running,
    Blocked,
    Zombie,
}

#[derive(Debug)]
pub struct SchedState {
    pub cpu: Option<u32>,
    pub state: ThreadState,
}

// Right now this represents a kernel thread.
// Eventually we'll want user space support too, so this should
// probably get renamed to `Task`.
#[repr(C)]
#[derive(Debug)]
pub struct Thread {
    pub regs: u64,
    pub id: u64,
    pub kernel_stack: Stack,
    pub sched_state: SpinLock<SchedState>,
}

extern "C" fn idle_thread_entry() -> ! {
    idle();
}

pub fn current_thread_id() -> u64 {
    let thread = current_cpu().thread_addr as *const Thread;
    unsafe { (*thread).id }
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
        let closure = unsafe { core::ptr::read(closure_ptr) };
        // 3. Call it!
        closure();
    }
}

extern "C" fn thread_trampoline(arg: *mut ()) -> ! {
    unsafe {
        // 1. The fat pointer is stored at the start of the page.
        let dyn_ptr = *(arg as *mut *mut dyn ThreadGasket);

        // 2. Call the gasket. We pass `arg` because that's where the actual
        //    closure 'F' lives.
        (*dyn_ptr).run_and_cleanup(arg);
    }
}

impl Thread {
    // Move this on the ready queue of the current CPU and mark it as ready.
    pub fn ready(&self) {
        let mut sched_state = self.sched_state.lock();
        sched_state.state = ThreadState::Ready;
        sched_state.cpu = Some(current_cpu().cpu_id);
        let mut ready_queue = current_cpu().ready_queue.lock();
        ready_queue
            .push(self as *const _ as u64)
            .expect("Ready queue is full");
    }

    // Spawns a new thread with the given entry point returning its id.
    fn spawn_raw(entry: extern "C" fn(*mut ()) -> !, arg: *mut ()) -> Option<u64> {
        let id = GLOBAL_THREAD_ID_COUNTER.fetch_add(1, Ordering::SeqCst);
        let stack = Stack::new(4); // 4 pages should be enough
        let mut regs = CpuRegsOnStack::new_inside_kernel(entry as u64, stack.get_top_address());
        regs.rdi = arg as u64;

        // Write the registers to the top of the stack.
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

        if !GLOBAL_THREAD_TABLE.lock().add_thread(thread) {
            return None;
        }

        GLOBAL_THREAD_TABLE
            .lock()
            .get_thread_mut(id)
            .unwrap()
            .ready();
        Some(id)
    }

    pub fn spawn<F>(f: F) -> Option<u64>
    where
        F: FnOnce() -> ! + Send + 'static,
    {
        // Ensure the closure fits in one page along with the vtable pointer.
        assert!(core::mem::size_of::<F>() < 4096 - 64);
        let page_addr = alloc_frame()
            .map(|f| phys_to_virt(f.addr()))
            .expect("Out of memory");

        unsafe {
            let fat_ptr_slot = page_addr.as_mut_ptr::<*mut dyn ThreadGasket>();
            // High alignment???
            let closure_slot = (page_addr.as_u64() + 64) as *mut F;

            closure_slot.write(f);
            let dyn_ptr: *mut dyn ThreadGasket = closure_slot as *mut dyn ThreadGasket;
            fat_ptr_slot.write(dyn_ptr);
        }

        Self::spawn_raw(thread_trampoline, page_addr.as_mut_ptr::<()>())
    }

    // Create an idle thread.
    pub fn new_idle() -> Self {
        let stack = Stack::new(1);
        let regs = CpuRegsOnStack::new_inside_kernel(
            idle_thread_entry as *const () as u64,
            stack.get_top_address(),
        );

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
