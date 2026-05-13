use x86_64::PhysAddr;

pub const FRAME_SIZE: usize = 4096;

/// A 4 KiB physical frame, identified by its base address.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame(pub PhysAddr);

impl Frame {
    pub fn new(addr: PhysAddr) -> Self {
        assert!(
            addr.is_aligned(FRAME_SIZE as u64),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRange {
    pub start: PhysAddr,
    pub count: usize,
}

impl FrameRange {
    pub fn new(start: PhysAddr, count: usize) -> Self {
        assert!(
            start.is_aligned(FRAME_SIZE as u64),
            "frame range start address must be {}-byte aligned",
            FRAME_SIZE
        );
        FrameRange { start, count }
    }

    pub fn start_addr(self) -> PhysAddr {
        self.start
    }

    pub fn end_addr(self) -> PhysAddr {
        self.start + (self.count as u64 * FRAME_SIZE as u64)
    }
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

    fn alloc_one(&self) -> Result<Frame, FrameAllocError> {
        self.alloc(1).map(|r| Frame::new(r.start_addr()))
    }

    fn dealloc_one(&self, frame: Frame) {
        self.dealloc(FrameRange::new(frame.addr(), 1));
    }

    fn total_frames(&self) -> usize;
    fn free_frames(&self) -> usize;
}
