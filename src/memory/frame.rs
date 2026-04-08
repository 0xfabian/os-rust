use x86_64::PhysAddr;

pub const FRAME_SIZE: usize = 4096;
pub const FRAME_SHIFT: usize = 12;
pub const FRAME_MASK: u64 = FRAME_SIZE as u64 - 1;

/// A 4 KiB physical frame, identified by its base address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame(pub PhysAddr);

impl Frame {
    pub fn new(addr: PhysAddr) -> Self {
        assert!(
            addr.as_u64() & FRAME_MASK == 0,
            "frame address must be {}-byte aligned",
            FRAME_SIZE
        );
        Frame(addr)
    }

    pub fn addr(self) -> PhysAddr {
        self.0
    }
}

/// A contiguous run of physical frames.
#[derive(Debug, Clone, Copy)]
pub struct FrameRange {
    pub start: Frame,
    pub count: usize,
}

#[derive(Debug, Clone, Copy)]
pub enum FrameAllocError {
    OutOfMemory,
    NotInitialized,
}

/// Backend that manages physical frames.
///
/// Implementations must be safe to call from multiple CPUs concurrently;
/// any required locking lives inside the implementation.
pub trait FrameAllocator: Send + Sync {
    fn alloc(&self, count: usize) -> Result<FrameRange, FrameAllocError>;
    fn dealloc(&self, frames: FrameRange);

    fn total_frames(&self) -> usize;
    fn free_frames(&self) -> usize;

    fn alloc_one(&self) -> Result<Frame, FrameAllocError> {
        self.alloc(1).map(|r| r.start)
    }

    fn dealloc_one(&self, frame: Frame) {
        self.dealloc(FrameRange {
            start: frame,
            count: 1,
        });
    }
}
