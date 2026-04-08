pub mod thread;

pub use thread::Thread;

use crate::arch::x86_64::cpu::{TlsData, current_cpu, get_cpu_tls};
use crate::arch::x86_64::lapic::current_lapic;
use crate::println;
use crate::sched::thread::Queue;
use crate::util::random_u64;
use core::arch::naked_asm;
use x86_64::structures::idt::InterruptStackFrame;

// With the current design, the scheduler must always run in a valid thread context
// and the initial setup functions don't run in a thread context, so we force a switch
// to the idle thread.
pub fn force_switch_to_idle() -> ! {
    let cpu = current_cpu();
    let idle_thread = &cpu.idle_thread;
    cpu.thread_addr = idle_thread as *const Thread as u64;
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

fn switch() {
    let cpu = current_cpu();
    let idle_thread_addr = &cpu.idle_thread as *const Thread as u64;

    let mut ready_queue = cpu.ready_queue.lock();

    let next_thread_addr = match ready_queue.pop() {
        Some(addr) => addr,
        None => return, // no ready thread, continue running the current one
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

// Very simple load balancing: pick another CPU at random and move
// half of the imbalance from the busier CPU to the less busy one.
//
// Right now, we assume:
//  - the cost of moving threads is negligible
//  - threads weigh the same, so only the number of threads matters
//  - core affinity (after threads are balanced, they don't move much)
fn maybe_load_balance() {
    let other_cpu = match get_random_online_cpu() {
        Some(cpu) => cpu,
        None => return,
    };

    let cpu = current_cpu();

    if cpu.cpu_id == other_cpu.cpu_id {
        return;
    }

    // To avoid deadlocks, lock in cpu_id order.
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

extern "C" fn timer_handler() {
    switch();
    maybe_load_balance();

    let lapic = current_lapic();
    // Schedule the next interrupt.
    lapic.write(0x380, 0x0fff);
    lapic.send_eoi();
}

#[unsafe(naked)]
pub extern "x86-interrupt" fn timer_interrupt(_stack_frame: InterruptStackFrame) {
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
        mov rax, gs:[{tls_thread}]      // address of the current thread struct
        mov [rax + {regs}], rsp         // save rsp into thread.regs
        call {handler}
        mov rax, gs:[{tls_thread}]      // reload (handler may have switched threads)
        mov rsp, [rax + {regs}]         // restore rsp from thread.regs
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
        tls_thread = const core::mem::offset_of!(TlsData, thread_addr),
        regs = const core::mem::offset_of!(Thread, regs),
        handler = sym timer_handler,
    );
}
