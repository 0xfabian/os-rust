use crate::println;
use crate::requests::MEMORY_MAP_REQUEST;
use crate::sync::SpinLock;
use x86_64::{PhysAddr, VirtAddr};

const FRAME_SIZE: usize = 4096;

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
        let byte_index = index / 8;
        let bit_index = index % 8;
        self.data[byte_index] |= 1 << bit_index;
    }

    fn clear(&mut self, index: usize) {
        let byte_index = index / 8;
        let bit_index = index % 8;
        self.data[byte_index] &= !(1 << bit_index);
    }

    fn is_set(&self, index: usize) -> bool {
        let byte_index = index / 8;
        let bit_index = index % 8;
        (self.data[byte_index] & (1 << bit_index)) != 0
    }
}

struct MemoryRegion<'a> {
    start: PhysAddr,
    end: PhysAddr,
    bitmap: BitmapView<'a>,
}

pub struct FrameRange {
    pub start: PhysAddr,
    pub count: usize,
}

impl<'a> MemoryRegion<'a> {
    fn new(start: u64, length: u64, data: &'a mut [u8]) -> Self {
        let num_pages = length as usize / FRAME_SIZE;

        Self {
            start: PhysAddr::new(start),
            end: PhysAddr::new(start + length),
            bitmap: BitmapView::new(num_pages, data),
        }
    }

    fn alloc(&mut self, num_pages: usize) -> Option<FrameRange> {
        let mut consecutive_free = 0;
        let mut start_index = 0;

        for i in 0..self.bitmap.size {
            if !self.bitmap.is_set(i) {
                if consecutive_free == 0 {
                    start_index = i;
                }
                consecutive_free += 1;

                if consecutive_free == num_pages {
                    for j in start_index..start_index + num_pages {
                        self.bitmap.set(j);
                    }
                    let addr = self.start + (start_index as u64 * FRAME_SIZE as u64);
                    return Some(FrameRange {
                        start: addr,
                        count: num_pages,
                    });
                }
            } else {
                consecutive_free = 0;
            }
        }

        None
    }

    fn lock(&mut self, frames: FrameRange) {
        let offset = frames.start.as_u64() - self.start.as_u64();
        let start_index = offset as usize / FRAME_SIZE;

        for i in start_index..start_index + frames.count {
            self.bitmap.set(i);
        }
    }

    fn free(&mut self, frames: FrameRange) {
        let offset = frames.start.as_u64() - self.start.as_u64();
        let start_index = offset as usize / FRAME_SIZE;

        for i in start_index..start_index + frames.count {
            self.bitmap.clear(i);
        }
    }
}

pub struct PhysicalMemoryManager<'a> {
    regions: &'a mut [MemoryRegion<'a>],
    hhdm_offset: u64,
}

impl<'a> PhysicalMemoryManager<'a> {
    fn new() -> Self {
        let mm_resp = MEMORY_MAP_REQUEST.get_response().unwrap();
        let hhdm_resp = crate::requests::HHDM_REQUEST.get_response().unwrap();

        // layout will be like this global PMM
        // MemoryRegion[] | Bitmap data | Bitmap data | ...
        let mut initial_size = 0;
        let mut num_regions = 0;

        for entry in mm_resp.entries() {
            if entry.entry_type == limine::memory_map::EntryType::USABLE {
                let frames = entry.length as usize / FRAME_SIZE;
                let bitmap_bytes = frames.div_ceil(8);
                println!(
                    "{:#018x} - {:#018x} {} frames, {} bytes",
                    entry.base,
                    entry.base + entry.length,
                    frames,
                    bitmap_bytes
                );
                // align to byte
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

        println!("total frames needed {}", initial_size.div_ceil(FRAME_SIZE));
        let frames_needed = initial_size.div_ceil(FRAME_SIZE);

        // OK, we computed the size, now we need to loop again
        // and find a region to store our PMM data

        // index into usable regions
        let mut ri = 0;
        for entry in mm_resp.entries() {
            if entry.entry_type == limine::memory_map::EntryType::USABLE {
                if entry.length >= initial_size as u64 {
                    // NOTE + hhdm offset, so we can write to it
                    let region_start = (entry.base + hhdm_resp.offset()) as usize;
                    let mut region_addr = region_start;
                    let mut bitmap_addr = region_addr + num_regions * size_of::<MemoryRegion>();

                    for entry in mm_resp.entries() {
                        if entry.entry_type == limine::memory_map::EntryType::USABLE {
                            let ptr = region_addr as *mut MemoryRegion;
                            let frames = entry.length as usize / FRAME_SIZE;
                            let bitmap_bytes = frames.div_ceil(8);
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

                    // clear all bitmaps
                    for region in region_slice.iter_mut() {
                        region.bitmap.data.fill(0);
                    }

                    // lock frames used by PMM itself
                    println!(
                        "Locking PMM data frames at physical address: {:#018x}",
                        entry.base
                    );
                    region_slice[ri].lock(FrameRange {
                        start: PhysAddr::new(entry.base),
                        count: frames_needed,
                    });

                    return PhysicalMemoryManager {
                        regions: region_slice,
                        hhdm_offset: hhdm_resp.offset(),
                    };
                }
                ri += 1;
            }
        }

        panic!("didn't find space for PMM data");
    }

    pub fn alloc(&mut self, num_frames: usize) -> Option<FrameRange> {
        for region in self.regions.iter_mut() {
            if let Some(frames) = region.alloc(num_frames) {
                return Some(frames);
            }
        }
        None
    }

    pub fn free(&mut self, frames: FrameRange) {
        for region in self.regions.iter_mut() {
            if frames.start >= region.start && frames.start < region.end {
                region.free(frames);
                return;
            }
        }
    }
}

pub static PMM: SpinLock<Option<PhysicalMemoryManager>> = SpinLock::new(None);

pub fn pmm_init() {
    *PMM.lock() = Some(PhysicalMemoryManager::new());
}

pub fn alloc_frames(num_frames: usize) -> Option<FrameRange> {
    PMM.lock().as_mut().and_then(|pmm| pmm.alloc(num_frames))
}

pub fn free_frames(frames: FrameRange) {
    PMM.lock().as_mut().map(|pmm| pmm.free(frames));
}

pub fn phys_to_virt(addr: PhysAddr) -> VirtAddr {
    VirtAddr::new(addr.as_u64() + PMM.lock().as_ref().unwrap().hhdm_offset)
}
