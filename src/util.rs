pub fn read_tsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        core::arch::asm!("rdtsc", out("eax") low, out("edx") high);
    }
    ((high as u64) << 32) | (low as u64)
}

pub fn random_u64() -> u64 {
    // this should be enough entropy
    let mut x = read_tsc();

    // very fast xorshift64
    x ^= x >> 13;
    x ^= x << 7;
    x ^= x >> 17;

    x
}
