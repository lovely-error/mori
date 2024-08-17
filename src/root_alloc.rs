
use core::{mem::{align_of, size_of}, ptr::addr_of_mut, sync::atomic::AtomicUsize};
use std::{cell::UnsafeCell, ptr::null_mut, sync::atomic::Ordering};

use crate::utils::{Block4KPtr, PageSource};
use libc;

macro_rules! static_assert {
    ($cond:expr) => {
      const _ : () = if !$cond { std::panic!("Comptime assert failed!") } ;
    };
    ($cond:expr, $msg:expr) => {
      const _ : () = if !$cond { panic!($msg) } ;
    };
}

const PAGE_2MB_SIZE: usize = 1 << 21;
const PAGE_2MB_ALIGN: usize = 1 << 21;
const SMALL_PAGE_LIMIT: usize = PAGE_2MB_SIZE / 4096;

#[repr(align(2097152))]
struct Superpage([Page4K;SMALL_PAGE_LIMIT]);
static_assert!(align_of::<Superpage>() == PAGE_2MB_ALIGN);
static_assert!(size_of::<Superpage>() == PAGE_2MB_SIZE);

#[repr(align(4096))] #[allow(dead_code)]
struct Page4K([u8;4096]);

#[derive(Debug, Clone, Copy)]
pub enum AllocationFailure {
  WouldRetry, NoMem
}

///
pub struct RootAllocator(UnsafeCell<RootAllocatorInner>);
struct RootAllocatorInner {
  super_page: *mut Superpage,
  index: AtomicUsize,
}
impl RootAllocator {
  fn inner(&self) -> &mut RootAllocatorInner {
    unsafe { &mut*self.0.get() }
  }
  fn alloc_superpage() -> Result<*mut Superpage, AllocationFailure> {
    let mut mem = null_mut();
    let out;
    unsafe {
      out = libc::posix_memalign(&mut mem, PAGE_2MB_ALIGN, PAGE_2MB_SIZE);
    };
    if out != 0 {
      return Err(AllocationFailure::NoMem);
    }
    return Ok(mem.cast())
  }
  pub fn new() -> Self {
    Self(
      UnsafeCell::new(RootAllocatorInner {
        super_page: null_mut(),
        index: AtomicUsize::new(SMALL_PAGE_LIMIT << 1),
      })
    )
  }
  #[inline(never)]
  pub fn try_get_page_fast_bailout(&self) -> Result<Block4KPtr, AllocationFailure> {
    let this = self.inner();
    let offset = this.index.fetch_add(1 << 1, Ordering::Relaxed);
    let locked = offset & 1 == 1;
    if locked { return Err(AllocationFailure::WouldRetry) }
    let mut index = offset >> 1;
    let did_overshoot = index >= SMALL_PAGE_LIMIT;
    if did_overshoot {
      let item = this.index.fetch_or(1, Ordering::Relaxed);
      let already_locked = item & 1 == 1;
      if already_locked {
        return Err(AllocationFailure::WouldRetry);
      }
      else { // we gotta provide new page
        let page = match Self::alloc_superpage() {
            Ok(mem) => mem,
            Err(err) => {
              this.index.fetch_and((!0) << 1, Ordering::Relaxed);
              return Err(err);
            },
        };
        this.super_page = page.cast();
        this.index.store(1 << 1, Ordering::Release);
        index = 0;
      }
    };
    let ptr = unsafe {
      core::ptr::addr_of_mut!((*this.super_page).0[index])
    };
    return Ok(Block4KPtr::new(ptr.cast()));
  }
  pub fn try_get_page_wait_tolerable(&self) -> Result<Block4KPtr, AllocationFailure> {
    loop {
      match self.try_get_page_fast_bailout() {
        Ok(mem) => return Ok(mem),
        Err(err) => {
          match err {
            AllocationFailure::WouldRetry => continue,
            _ => return Err(err)
          }
        },
      }
    }
  }
  pub fn destroy(&self) {
    let inner = self.inner();
    let data = inner.index.compare_exchange(
      0, 0, Ordering::Relaxed, Ordering::Relaxed).unwrap_err();
    let index = data >> 1;
    if index >= SMALL_PAGE_LIMIT { return }
    for i in index  .. SMALL_PAGE_LIMIT {
      unsafe {
        let ptr = addr_of_mut!((*inner.super_page).0[i]);
        libc::munmap(ptr.cast(), size_of::<Page4K>());
      }
    }
  }
}

unsafe impl Sync for RootAllocator {}

impl PageSource for RootAllocator {
    fn try_get_free_page(&self) -> Option<Block4KPtr> {
      match self.try_get_page_wait_tolerable() {
        Ok(mem) => Some(mem),
        Err(err) => match err {
          AllocationFailure::WouldRetry => unreachable!(),
          AllocationFailure::NoMem => return None,
        },
      }
    }
}

#[test]
fn alloc_works() {
  use std::{mem::size_of, ptr::{addr_of, null_mut, slice_from_raw_parts}, thread};
  // this will eat a lot of ram, fix it if not disposed properly
  const THREAD_COUNT:usize = 4096 * 4;
  let ralloc = RootAllocator::new();
  let ptrs: [*mut u32;THREAD_COUNT] = [null_mut(); THREAD_COUNT];

  thread::scope(|s|{
    for i in 0 .. THREAD_COUNT {
      let unique_ref = &ralloc;
      let fuck = addr_of!(ptrs) as u64 ;
      s.spawn(move || {
        let ptr;
        loop {
          if let Ok(ptr_) = unique_ref.try_get_page_wait_tolerable() {
            ptr = ptr_; break;
          };
        }
        let v = ptr.get_ptr();
        for ix in 0 .. (4096 / size_of::<u32>()) {
          unsafe { *v.cast::<u32>().add(ix) = i as u32; }
        }
        unsafe { *(fuck as *mut u64).add(i) = v as u64 };
      });
    }
  });
  for i in 0 .. THREAD_COUNT {
    let ptr = ptrs[i];
    let sl : &[u32] = unsafe { &*slice_from_raw_parts(ptr, 4096 / size_of::<u32>()) };
    for s in sl {
        assert!(*s == i as u32, "threads got same memory region!!!");
    }
  }
}

