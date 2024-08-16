use core::cell::UnsafeCell;
use core::mem::size_of;
use core::ptr::null_mut;

use crate::{Block4KPtr, PageSource};

macro_rules! static_assert {
  ($cond:expr) => {
    const _ : () = if !$cond { std::panic!("Comptime assert failed!") } ;
  };
  ($cond:expr, $msg:expr) => {
    const _ : () = if !$cond { panic!($msg) } ;
  };
}

const SMALL_PAGE_SIZE : usize = 4096;

#[repr(C)]
struct FreePageList {
  next_page: *mut FreePageList,
  bytes: [u8; SMALL_PAGE_SIZE - size_of::<*mut FreePageList>()]
}
static_assert!(size_of::<FreePageList>() == 4096, "Invalid size");

struct PageStorageInner {
  free_pages: *mut FreePageList,
  page_count: usize
}
pub struct PageStorage(UnsafeCell<PageStorageInner>);
impl PageStorage {
  pub fn new() -> Self {
    Self(UnsafeCell::new(PageStorageInner {
      free_pages: null_mut(),
      page_count: 0
    }))
  }
  fn inner(&self) -> &mut PageStorageInner {
    unsafe { &mut *self.0.get() }
  }
  pub fn available_page_count(&self) -> usize {
    let this = self.inner();
    this.page_count
  }
  pub fn store_page(&self, page:Block4KPtr) { unsafe {
    let this = self.inner();
    this.page_count += 1;
    let page = page.get_ptr().cast::<FreePageList>();
    (*page).next_page = null_mut();
    if !this.free_pages.is_null() {
      (*this.free_pages).next_page = page;
    }
    this.free_pages = page;
  } }
  pub fn try_get_page(&self) -> Option<Block4KPtr> {
    let this = self.inner();
    let head = this.free_pages;
    if head.is_null() { return None }
    let next = unsafe { (*head).next_page };
    this.free_pages = next;
    return Some(Block4KPtr::new(head.cast()))
  }
  pub fn dispose_mem(self) { unsafe {
    let this = self.inner();
    let mut page = this.free_pages;
    loop {
      if page.is_null() { return; }
      let next = (*page).next_page;
      let out = libc::munmap(page.cast(), SMALL_PAGE_SIZE);
      debug_assert!(out == 0, "Failed to unmap mem?? 0_o\naddress was {:?}", page);
      page = next;
    }
  } }
}
impl PageSource for PageStorage {
  fn try_get_free_page(&self) -> Option<Block4KPtr> {
    self.try_get_page()
  }
}