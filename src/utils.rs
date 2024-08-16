
#[derive(Debug, Clone, Copy)]
pub struct Block4KPtr(*mut u8);
impl Block4KPtr {
  pub fn new(ptr: *mut ()) -> Self {
    debug_assert!(ptr.is_aligned_to(4096), "misaligned ptr given to Block4KPtr");
    return Self(ptr.cast())
  }
  pub fn get_ptr(&self) -> *mut u8 {
    self.0 as _
  }
}

pub trait PageSource {
  fn try_get_free_page(&self) -> Option<Block4KPtr>;
}

#[inline]
pub(crate) fn align_backward(ptr: *mut (), alignment: usize) -> *mut () {
  debug_assert!(alignment.is_power_of_two());
  let mask = alignment - 1;
  let ptr = ptr as usize & !mask;
  return ptr as _;
}

