pub mod bitmap;
pub mod frame;
pub mod vmm;

use crate::boot::requests::HHDM_REQUEST;
use crate::memory::bitmap::BitmapFrameAllocator;
use crate::memory::frame::{Frame, FrameAllocError, FrameAllocator, FrameRange};
use core::sync::atomic::{AtomicU64, Ordering};
use x86_64::{PhysAddr, VirtAddr};

static FRAME_ALLOCATOR: BitmapFrameAllocator = BitmapFrameAllocator::new();
static HHDM_OFFSET: AtomicU64 = AtomicU64::new(0);

/// Initializes the global physical memory state. Must be called exactly
/// once during early kernel init, before anything tries to allocate frames
/// or translate addresses.
pub fn init() {
    let hhdm_resp = HHDM_REQUEST.get_response().unwrap();
    HHDM_OFFSET.store(hhdm_resp.offset(), Ordering::Relaxed);
    FRAME_ALLOCATOR.init();
}

pub fn alloc_frames(count: usize) -> Result<FrameRange, FrameAllocError> {
    FRAME_ALLOCATOR.alloc(count)
}

pub fn alloc_frame() -> Result<Frame, FrameAllocError> {
    FRAME_ALLOCATOR.alloc_one()
}

pub fn dealloc_frames(frames: FrameRange) {
    FRAME_ALLOCATOR.dealloc(frames);
}

pub fn dealloc_frame(frame: Frame) {
    FRAME_ALLOCATOR.dealloc_one(frame);
}

pub fn total_frames() -> usize {
    FRAME_ALLOCATOR.total_frames()
}

pub fn free_frames() -> usize {
    FRAME_ALLOCATOR.free_frames()
}

pub fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
    VirtAddr::new(addr.as_u64() + HHDM_OFFSET.load(Ordering::Relaxed))
}

pub fn virt_to_phys(addr: VirtAddr) -> PhysAddr {
    PhysAddr::new(addr.as_u64() - HHDM_OFFSET.load(Ordering::Relaxed))
}
