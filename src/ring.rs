//! 单生产者/单消费者环形缓冲。
//!
//! PipeWire 线程写，DAW 音频线程读。除了两个原子游标之外不共享任何可变状态，
//! 两边都不会阻塞、不会分配内存。

use std::cell::UnsafeCell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

#[derive(Clone, Copy, Default, Debug)]
pub struct Frame {
    pub l: f32,
    pub r: f32,
}

pub struct Ring {
    buf: Box<[UnsafeCell<Frame>]>,
    mask: usize,
    write: AtomicUsize,
    read: AtomicUsize,
    dropped: AtomicU64,
}

// 只有生产者碰 write 游标，只有消费者碰 read 游标，buf 的访问范围由两者共同保证。
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

impl Ring {
    pub fn new(frames: usize) -> Self {
        let frames = frames.next_power_of_two();
        let buf = (0..frames)
            .map(|_| UnsafeCell::new(Frame::default()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            buf,
            mask: frames - 1,
            write: AtomicUsize::new(0),
            read: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// 生产者：交错立体声。缓冲满了就丢弃多出来的部分（宁可丢掉，也不阻塞实时线程）。
    pub fn write_interleaved(&self, data: &[f32]) {
        let frames = data.len() / 2;
        if frames == 0 {
            return;
        }
        let w = self.write.load(Ordering::Relaxed);
        let r = self.read.load(Ordering::Acquire);
        let space = self.capacity().saturating_sub(w.wrapping_sub(r));
        let n = frames.min(space);
        if n > 0 {
            for i in 0..n {
                let f = Frame {
                    l: data[2 * i],
                    r: data[2 * i + 1],
                };
                unsafe { *self.buf[(w + i) & self.mask].get() = f };
            }
            self.write.store(w.wrapping_add(n), Ordering::Release);
        }
        if n < frames {
            self.dropped
                .fetch_add((frames - n) as u64, Ordering::Relaxed);
        }
    }

    /// 消费者：读一帧，空返回 false。
    #[inline]
    pub fn read_one(&self, out: &mut Frame) -> bool {
        let r = self.read.load(Ordering::Relaxed);
        let w = self.write.load(Ordering::Acquire);
        if r == w {
            return false;
        }
        *out = unsafe { *self.buf[r & self.mask].get() };
        self.read.store(r.wrapping_add(1), Ordering::Release);
        true
    }

    #[inline]
    pub fn available(&self) -> usize {
        let w = self.write.load(Ordering::Acquire);
        let r = self.read.load(Ordering::Relaxed);
        w.wrapping_sub(r)
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub fn reset(&self) {
        self.write.store(0, Ordering::Release);
        self.read.store(0, Ordering::Release);
        self.dropped.store(0, Ordering::Relaxed);
    }
}
