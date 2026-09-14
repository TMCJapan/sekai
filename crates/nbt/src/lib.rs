//! Pure NBT parse/diff views over raw (already decompressed) NBT bytes.
//!
//! Input is always raw NBT bytes from `sekai-anvil`; this crate never sees
//! compression.

#![no_std]

extern crate alloc;
