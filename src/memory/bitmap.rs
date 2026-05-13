//! Bitmap-based physical frame allocator.
//!
//! On `init`, we walk the Limine memory map and, for every USABLE entry,
//! build a `MemoryRegion` that owns a bitmap (one bit per 4 KiB frame,
//! 1 = used, 0 = free). All region structs and their bitmap bytes are
//! packed together inside the first usable region big enough to host
//! them, and those frames are then reserved so we never hand them out.
//!
//! `alloc(count)` does a linear first-fit scan over each region looking
//! for `count` consecutive zero bits, sets them, and returns the
//! corresponding `FrameRange`. `dealloc` clears the bits in the region
//! that contains the range. All state lives behind a single `SpinLock`
//! (inside locking), so the allocator is `Send + Sync` and safe to call
//! from any CPU.

use crate::boot::requests::{HHDM_REQUEST, MEMORY_MAP_REQUEST};
use crate::memory::frame::{FRAME_SIZE, FrameAllocError, FrameAllocator, FrameRange};
use crate::println;
use crate::sync::SpinLock;
use x86_64::PhysAddr;

struct BitmapView<'a> {
    // in bits
    size: usize,
    data: &'a mut [u8],
}

impl<'a> BitmapView<'a> {
    fn new(size: usize, data: &'a mut [u8]) -> Self {
        BitmapView { size, data }
    }

    fn set(&mut self, index: usize) {
        self.data[index / 8] |= 1 << (index % 8);
    }

    fn clear(&mut self, index: usize) {
        self.data[index / 8] &= !(1 << (index % 8));
    }

    fn is_set(&self, index: usize) -> bool {
        (self.data[index / 8] & (1 << (index % 8))) != 0
    }
}

struct MemoryRegion<'a> {
    start: PhysAddr,
    end: PhysAddr,
    bitmap: BitmapView<'a>,
}

impl<'a> MemoryRegion<'a> {
    fn new(start: u64, length: u64, data: &'a mut [u8]) -> Self {
        let count = length as usize / FRAME_SIZE;
        Self {
            start: PhysAddr::new(start),
            end: PhysAddr::new(start + length),
            bitmap: BitmapView::new(count, data),
        }
    }

    fn alloc(&mut self, count: usize) -> Option<FrameRange> {
        let mut consecutive_free = 0;
        let mut start_index = 0;

        for i in 0..self.bitmap.size {
            if !self.bitmap.is_set(i) {
                if consecutive_free == 0 {
                    start_index = i;
                }
                consecutive_free += 1;

                if consecutive_free == count {
                    for j in start_index..start_index + count {
                        self.bitmap.set(j);
                    }
                    let addr = self.start + (start_index as u64 * FRAME_SIZE as u64);
                    return Some(FrameRange::new(addr, count));
                }
            } else {
                consecutive_free = 0;
            }
        }

        None
    }

    fn dealloc(&mut self, frames: FrameRange) {
        let offset = frames.start.as_u64() - self.start.as_u64();
        let start_index = offset as usize / FRAME_SIZE;
        for i in start_index..start_index + frames.count {
            self.bitmap.clear(i);
        }
    }

    // Mark a range as used without going through the allocator. Used
    // during init to reserve frames the allocator must never hand out
    // (e.g. the bitmap data itself).
    fn reserve(&mut self, frames: FrameRange) {
        let offset = frames.start.as_u64() - self.start.as_u64();
        let start_index = offset as usize / FRAME_SIZE;
        for i in start_index..start_index + frames.count {
            self.bitmap.set(i);
        }
    }

    fn contains(&self, addr: PhysAddr) -> bool {
        addr >= self.start && addr < self.end
    }

    fn total_frames(&self) -> usize {
        self.bitmap.size
    }

    fn free_frames(&self) -> usize {
        let mut n = 0;
        for i in 0..self.bitmap.size {
            if !self.bitmap.is_set(i) {
                n += 1;
            }
        }
        n
    }
}

struct Inner {
    regions: &'static mut [MemoryRegion<'static>],
}

pub struct BitmapFrameAllocator {
    inner: SpinLock<Option<Inner>>,
}

impl BitmapFrameAllocator {
    pub const fn new() -> Self {
        Self {
            inner: SpinLock::new(None),
        }
    }

    /// Initializes the bitmap from the Limine memory map. Must be called
    /// exactly once during early kernel init.
    pub fn init(&self) {
        let mm_resp = MEMORY_MAP_REQUEST.get_response().unwrap();
        let hhdm_resp = HHDM_REQUEST.get_response().unwrap();

        // Layout of the allocator metadata block:
        // MemoryRegion[] | Bitmap data | Bitmap data | ...
        let mut initial_size = 0;
        let mut num_regions = 0;

        for entry in mm_resp.entries() {
            if entry.entry_type == limine::memory_map::EntryType::USABLE {
                let count = entry.length as usize / FRAME_SIZE;
                let bitmap_bytes = count.div_ceil(8);
                println!(
                    "{:#018x} - {:#018x} {} frames, {} bytes",
                    entry.base,
                    entry.base + entry.length,
                    count,
                    bitmap_bytes
                );
                initial_size += bitmap_bytes;
                num_regions += 1;
            }
        }

        println!(
            "total bitmap size {} bytes + {} region bytes",
            initial_size,
            num_regions * core::mem::size_of::<MemoryRegion>()
        );

        initial_size += num_regions * size_of::<MemoryRegion>();

        let frames_needed = initial_size.div_ceil(FRAME_SIZE);
        println!("total frames needed {}", frames_needed);

        // Find a usable region big enough to host our metadata.
        let mut ri = 0;
        for entry in mm_resp.entries() {
            if entry.entry_type == limine::memory_map::EntryType::USABLE {
                if entry.length >= initial_size as u64 {
                    let region_start = (entry.base + hhdm_resp.offset()) as usize;
                    let mut region_addr = region_start;
                    let mut bitmap_addr = region_addr + num_regions * size_of::<MemoryRegion>();

                    for entry in mm_resp.entries() {
                        if entry.entry_type == limine::memory_map::EntryType::USABLE {
                            let ptr = region_addr as *mut MemoryRegion;
                            let count = entry.length as usize / FRAME_SIZE;
                            let bitmap_bytes = count.div_ceil(8);
                            unsafe {
                                ptr.write(MemoryRegion::new(
                                    entry.base,
                                    entry.length,
                                    core::slice::from_raw_parts_mut(
                                        bitmap_addr as *mut u8,
                                        bitmap_bytes,
                                    ),
                                ));
                            }
                            region_addr += size_of::<MemoryRegion>();
                            bitmap_addr += bitmap_bytes;
                        }
                    }

                    let region_slice = unsafe {
                        core::slice::from_raw_parts_mut(
                            region_start as *mut MemoryRegion,
                            num_regions,
                        )
                    };

                    for region in region_slice.iter_mut() {
                        region.bitmap.data.fill(0);
                    }

                    println!(
                        "Reserving allocator data frames at physical address: {:#018x}",
                        entry.base
                    );
                    region_slice[ri]
                        .reserve(FrameRange::new(PhysAddr::new(entry.base), frames_needed));

                    let mut slot = self.inner.lock();
                    assert!(slot.is_none(), "BitmapFrameAllocator already initialized");
                    *slot = Some(Inner {
                        regions: region_slice,
                    });
                    return;
                }
                ri += 1;
            }
        }

        panic!("didn't find space for frame allocator data");
    }
}

impl FrameAllocator for BitmapFrameAllocator {
    fn alloc(&self, count: usize) -> Result<FrameRange, FrameAllocError> {
        let mut slot = self.inner.lock();
        let inner = slot.as_mut().ok_or(FrameAllocError::NotInitialized)?;
        for region in inner.regions.iter_mut() {
            if let Some(frames) = region.alloc(count) {
                return Ok(frames);
            }
        }
        Err(FrameAllocError::OutOfMemory)
    }

    fn dealloc(&self, frames: FrameRange) {
        let mut slot = self.inner.lock();
        let Some(inner) = slot.as_mut() else { return };
        for region in inner.regions.iter_mut() {
            if region.contains(frames.start) {
                region.dealloc(frames);
                return;
            }
        }
    }

    fn total_frames(&self) -> usize {
        let slot = self.inner.lock();
        let Some(inner) = slot.as_ref() else { return 0 };
        inner.regions.iter().map(|r| r.total_frames()).sum()
    }

    fn free_frames(&self) -> usize {
        let slot = self.inner.lock();
        let Some(inner) = slot.as_ref() else { return 0 };
        inner.regions.iter().map(|r| r.free_frames()).sum()
    }
}
