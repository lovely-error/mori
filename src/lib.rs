#![feature(pointer_is_aligned_to)]

mod slab_allocator;
mod root_alloc;
mod page_storage;
mod utils;

pub use crate::{
    root_alloc::{RootAllocator, AllocationFailure},
    slab_allocator::{SlabAllocator, RawMemoryPtr, AllocFailure},
    utils::{Block4KPtr, PageSource},
    page_storage::PageStorage
};

#[cfg(not(target_os = "linux"))]
  compile_error!(
    "It only works on linux"
  );
#[cfg(not(target_arch= "x86_64"))]
  compile_error!(
    "It only works on x86_64"
  );