use core::{cell::UnsafeCell, mem::{align_of, offset_of, size_of, MaybeUninit}, ptr::null_mut, sync::atomic::{AtomicU64, Ordering}};

use crate::utils::{align_backward, Block4KPtr, PageSource};

macro_rules! static_assert {
    ($cond:expr) => {
      const _ : () = if !$cond { core::panic!("Comptime assert failed!") } ;
    };
    ($cond:expr, $msg:expr) => {
      const _ : () = if !$cond { core::panic!($msg) } ;
    };
}

#[repr(align(64))]
struct CLCell(#[allow(dead_code)][u8;64]);
const CELL_MAX_COUNT: usize = (4096 - 1) / size_of::<CLCell>();
static_assert!(CELL_MAX_COUNT == 63);
struct OccupationMap(AtomicU64);
#[repr(C)] #[repr(align(64))]
struct Metadata {
  occupation_map: OccupationMap,
  next_page: *mut SlabAllocPage,
}
static_assert!(size_of::<Metadata>() <= 64);
static_assert!(align_of::<Metadata>() == 64);
#[repr(C)] #[repr(align(4096))]
struct SlabAllocPage {
  mtd: Metadata,
  slots: MaybeUninit<[CLCell;CELL_MAX_COUNT]>
}
static_assert!(size_of::<SlabAllocPage>() == 4096);
static_assert!(align_of::<SlabAllocPage>() == 4096);
static_assert!(offset_of!(SlabAllocPage, slots) == 64);

impl OccupationMap {
  const NEVER_VALID_INDEX: u64 = 63;
  fn new() -> Self {
    Self(AtomicU64::new(0))
  }
  // returns starting index for valid contiguous cell count.
  fn try_find_span(&self, cell_count: usize, alignment: usize) -> Option<usize> {
    debug_assert!(cell_count <= 63);
    debug_assert!(alignment.is_power_of_two());
    let mut map = self.0.load(Ordering::Acquire);
    let mut found_index = Self::NEVER_VALID_INDEX;
    let span = (1u64 << cell_count) - 1;
    let prop_align = (alignment >> 6).saturating_sub(1) as u64;
    if cell_count == 1 {
      loop {
        let index = map.trailing_ones() as u64;
        if index & prop_align == prop_align {
          found_index = index;
          break;
        } else {
          map |= 1 << index;
        }
      }
    } else {
      let tail_pat = ((1u64 << 16) - 1) << 48;
      let mut tail_mask = 0;
      let mut i = 0;
      'search:loop {
        let s:u64 = i * 16;
        let map = map >> s;
        let map = map | tail_mask;
        let mut founds = 0u64;
        for k in 0 .. 16 {
          let mut index = ((((span << k) & map) == 0) as u64) << k;
          index *= (k & prop_align == prop_align) as u64;
          founds |= index;
        }
        if founds != 0 {
          let inner_index = founds.trailing_zeros() as u64;
          let index = (i * 16) + inner_index;
          found_index = index;
          break 'search;
        }
        if i == 4 { break 'search }
        i += 1;
        tail_mask |= (tail_mask >> 16) | tail_pat;
      }
    }
    if found_index == Self::NEVER_VALID_INDEX {
      return None;
    }
    let mask = span << found_index;
    self.0.fetch_or(mask, Ordering::AcqRel);
    return Some(found_index as usize);
  }
  fn release_span(&self, span: usize, start: usize) {
    let mask = ((1u64 << span) - 1) << start;
    self.0.fetch_and(!mask, Ordering::Release);
  }
}

#[test]
fn ocup_map_t() {
  let om = OccupationMap(AtomicU64::new(605069386));
  om.try_find_span(3, 64);
  assert!(om.0.load(Ordering::Relaxed) == 605070282);

  let om = OccupationMap(AtomicU64::new(605069386));
  om.try_find_span(10, 64);
  assert!(om.0.load(Ordering::Relaxed) == 1099042955338);

  let om = OccupationMap(AtomicU64::new(u64::MAX >> 1));
  match om.try_find_span(1, 64) {
    Some(_) => panic!(),
    None => (),
  }

  let om = OccupationMap(AtomicU64::new(0));
  match om.try_find_span(63, 64) {
    Some(_) => (),
    None => panic!(),
  }
  assert!(om.0.load(Ordering::Relaxed) == u64::MAX >> 1);

  assert!(om.try_find_span(1, 64).is_none());
  assert!(om.try_find_span(1, 64).is_none());

  let om = OccupationMap(AtomicU64::new(1668));
  match om.try_find_span(2, 512) {
    Some(_) => (),
    None => panic!(),
  }
  assert!(om.0.load(Ordering::Relaxed) == 99972);

  let om = OccupationMap(AtomicU64::new(7));
  match om.try_find_span(1, 512) {
    Some(_) => (),
    None => panic!(),
  }
  assert!(om.0.load(Ordering::Relaxed) == 135);
}
#[test]
fn cyc() {
  let om = OccupationMap::new();
  let mut v = Vec::new();
  for _ in 0 .. 63 {
    let k = om.try_find_span(1, 1).unwrap();
    v.push(k);
  }
  assert!(om.0.load(Ordering::Relaxed) == u64::MAX >> 1);
  v.reverse();
  for item in v {
    om.release_span(1, item as usize);
    // println!("{:0b}", om.0.load(Ordering::Relaxed));
  }
  assert!(om.0.load(Ordering::Relaxed) == 0);
}

pub struct SlabAllocator(UnsafeCell<SlabAllocatorInner>);

struct SlabAllocatorInner {
  start_page: *mut SlabAllocPage,
  current_page: *mut SlabAllocPage,
  tail_page: *mut SlabAllocPage
}

impl SlabAllocator {
  pub fn new() -> Self {
    SlabAllocator(UnsafeCell::new(SlabAllocatorInner {
      start_page: null_mut(),
      current_page: null_mut(),
      tail_page: null_mut()
    }))
  }
  fn do_first_init(&self, page: Block4KPtr) {
    let inner = self.inner();
    let ptr = page.get_ptr().cast::<SlabAllocPage>();
    Self::setup_page(ptr);
    inner.current_page = ptr;
    inner.start_page = ptr;
    inner.tail_page = ptr;
  }
  fn inner(&self) -> &mut SlabAllocatorInner {
    unsafe { &mut *self.0.get() }
  }
  fn setup_page(page: *mut SlabAllocPage) {
    unsafe {
      (*page).mtd = Metadata {
        occupation_map: OccupationMap::new(),
        next_page: null_mut(),
      }
    }
  }
  pub const MAX_ALLOC_SIZE_IN_BYTES: usize = 4096 - size_of::<Metadata>();
  pub fn can_allocate(size:usize, alignment:usize) -> bool {
    debug_assert!(alignment.is_power_of_two());
    let size = size + (alignment > size) as usize * alignment;
    size <= Self::MAX_ALLOC_SIZE_IN_BYTES
  }
  #[inline(never)]
  pub fn smalloc(&self, size:usize, alignment:usize, page_source: &dyn PageSource) -> Result<RawMemoryPtr, AllocFailure> {
    if !Self::can_allocate(size, alignment) {
      return Err(AllocFailure::WontFit);
    }
    let inner = self.inner();
    if inner.start_page == null_mut() {
      let page = match page_source.try_get_free_page() {
        Some(page) => page,
        None => return Err(AllocFailure::NoMem),
      };
      self.do_first_init(page)
    }
    let block_span = (size + 63) / 64;
    let start_page = inner.current_page;

    let found_index ;
    let mut search_page = start_page;
    'traverse:loop {
      let mtd = unsafe { &mut (*search_page).mtd };
      let outcome = mtd.occupation_map.try_find_span(block_span, alignment);
      match outcome {
        Some(index) => { found_index = index; break 'traverse },
        None => {
          let next = mtd.next_page;
          if next.is_null() {
            let new_page = match page_source.try_get_free_page() {
              Some(new_page) => new_page,
              None => {
                search_page = inner.start_page;
                continue 'traverse;
              },
            };
            let ptr = new_page.get_ptr().cast();
            Self::setup_page(ptr);
            mtd.next_page = ptr;
            continue 'traverse;
          } else {
            if next == start_page { return Err(AllocFailure::NoMem) }
            search_page = next;
            continue 'traverse;
          }
        },
      }
    }
    let ptr = unsafe {
      (*search_page).slots.assume_init_mut().as_mut_ptr().add(found_index as usize).cast::<()>()
    };
    debug_assert!(ptr.is_aligned_to(64));
    let ptr = RawMemoryPtr::new(ptr, block_span);

    return Ok(ptr);
  }
}

#[derive(Debug, Clone, Copy)]
pub enum AllocFailure {
  NoMem,
  WontFit
}
#[repr(transparent)] #[derive(Debug, Clone, Copy)]
pub struct RawMemoryPtr(u64);
impl RawMemoryPtr {
  pub fn null() -> Self {
    RawMemoryPtr(0)
  }
  pub fn is_null(&self) -> bool {
    self.0 == 0
  }
  pub(crate) fn new(ptr:*mut(), block_span: usize) -> Self {
    debug_assert!(block_span <= CELL_MAX_COUNT);
    let mtd = block_span as u64;
    let combined = (mtd << 48) | (ptr as u64);
    return Self(combined);
  }
  pub(crate) fn unpack(&self) -> (*mut (), usize) {
    let ptr = self.0 & ((1u64 << 48) - 1);
    let span = self.0 >> 48;
    return (ptr as *mut (), span as usize);
  }
  pub fn get_ptr(&self) -> *mut () {
    self.unpack().0
  }

  #[inline(always)]
  pub fn release_memory(self) {
    let (ptr, span) = self.unpack();
    let page_ptr = align_backward(ptr, align_of::<SlabAllocPage>()).cast::<SlabAllocPage>();
    let mtd = unsafe { &(*page_ptr).mtd };
    let dist = (ptr as usize) - (page_ptr as usize);
    let index = (dist >> 6) - 1;
    mtd.occupation_map.release_span(span, index);
  }
}

#[test]
fn basic_alignment_test() {
  let raloc = crate::root_alloc::RootAllocator::new();
  let mballoc = SlabAllocator::new();

  let smth = mballoc.smalloc(96, 256, &raloc);
  if let Ok(ptr) = smth {
    assert!(ptr.get_ptr().is_aligned_to(256));
    println!("{:?}", ptr);
  } else {
    panic!()
  }
  let smth = mballoc.smalloc(4096-size_of::<Metadata>(), 1, &raloc);
  if let Ok(ptr) = smth {
    assert!(ptr.get_ptr().is_aligned_to(1));
    println!("{:?}", ptr);
    ptr.release_memory()
  } else {
    panic!()
  }
  let smth = mballoc.smalloc(4096-size_of::<Metadata>() + 1, 1, &raloc);
  if let Err(AllocFailure::WontFit) = smth {
    ()
  } else {
    panic!()
  }
  let smth = mballoc.smalloc(4096-size_of::<Metadata>(), 1, &raloc);
  if let Ok(ptr) = smth {
    assert!(ptr.get_ptr().is_aligned_to(1));
    println!("{:?}", ptr);
    ptr.release_memory()
  } else {
    panic!()
  }
}

#[test]
fn basic_uniqueness_test() {
  let ral = crate::RootAllocator::new();
  let sal = SlabAllocator::new();

  const LIMIT: usize = 10;

  let mut m = Vec::new();
  m.reserve(LIMIT);

  for _ in 0 .. LIMIT {
    let smth = sal.smalloc(24, align_of::<u64>(), &ral);
    match smth {
      Ok(ptr) => m.push(ptr.0),
      Err(e) => panic!("{:?}", e),
    }
  }

  let mut k = Vec::new();

  for item in &m {
    for item2 in &k {
      if item == item2 { panic!("fk. collision") }
    }
    k.push(*item)
  }
}

