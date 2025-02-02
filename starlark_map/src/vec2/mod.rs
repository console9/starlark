/*
 * Copyright 2019 The Starlark in Rust Authors.
 * Copyright (c) Facebook, Inc. and its affiliates.
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 *     https://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

use std::alloc;
use std::alloc::Layout;
use std::alloc::LayoutError;
use std::cmp;
use std::cmp::Ordering;
use std::fmt::Debug;
use std::marker::PhantomData;
use std::mem;
use std::mem::MaybeUninit;
use std::ptr;
use std::ptr::NonNull;
use std::slice;

use allocative::Allocative;
use allocative::Visitor;

use crate::sorting::insertion::insertion_sort;
use crate::sorting::insertion::slice_swap_shift;

pub(crate) mod iter;

#[derive(Eq, PartialEq, Debug)]
struct Vec2Layout<A, B> {
    layout: Layout,
    offset_of_bbb: usize,
    _marker: PhantomData<*mut (A, B)>,
}

impl<A, B> Vec2Layout<A, B> {
    fn new(cap: usize) -> Vec2Layout<A, B> {
        Self::new_checked(cap).unwrap_or_else(|err| {
            panic!(
                "Vec2Layout failed with {:?} when allocating capacity of {}",
                err, cap
            )
        })
    }

    fn new_checked(cap: usize) -> Result<Vec2Layout<A, B>, LayoutError> {
        debug_assert!(cap != 0);
        let a = Layout::array::<A>(cap)?;
        let b = Layout::array::<B>(cap)?;
        let (layout, offset_of_bbb) = a.extend(b)?;

        debug_assert!(offset_of_bbb <= layout.size());
        debug_assert!(layout.align() >= a.align());
        debug_assert!(layout.align() >= b.align());
        debug_assert!(offset_of_bbb % a.align() == 0);

        Ok(Vec2Layout {
            layout,
            offset_of_bbb,
            _marker: PhantomData,
        })
    }

    unsafe fn alloc(&self) -> NonNull<B> {
        let ptr: *mut u8 = alloc::alloc(self.layout);
        let bbb_ptr: *mut B = ptr.add(self.offset_of_bbb).cast();
        NonNull::new_unchecked(bbb_ptr)
    }

    unsafe fn dealloc(&self, bbb_ptr: NonNull<B>) {
        let ptr: *mut u8 = bbb_ptr.as_ptr().cast::<u8>().sub(self.offset_of_bbb);
        alloc::dealloc(ptr, self.layout)
    }
}

/// Array of pairs `(A, B)`, where `A` and `B` are stored separately.
/// This reduces memory consumption when `A` and `B` have different alignments.
pub(crate) struct Vec2<A, B> {
    // Layout is `[padding, A, A, ..., A, B, B, ..., B]`
    bbb_ptr: NonNull<B>,
    len: usize,
    cap: usize,
    _marker: PhantomData<(A, B)>,
}

unsafe impl<A: Send, B: Send> Send for Vec2<A, B> {}
unsafe impl<A: Sync, B: Sync> Sync for Vec2<A, B> {}

impl<A, B> Default for Vec2<A, B> {
    #[inline]
    fn default() -> Vec2<A, B> {
        Vec2::new()
    }
}

impl<A: Debug, B: Debug> Debug for Vec2<A, B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.iter()).finish()
    }
}

impl<A: Clone, B: Clone> Clone for Vec2<A, B> {
    fn clone(&self) -> Vec2<A, B> {
        let mut r = Vec2::with_capacity(self.len());
        for (a, b) in self.iter() {
            r.push(a.clone(), b.clone());
        }
        r
    }
}

impl<A, B> Vec2<A, B> {
    #[inline]
    pub(crate) const fn new() -> Vec2<A, B> {
        Vec2 {
            bbb_ptr: NonNull::dangling(),
            len: 0,
            cap: 0,
            _marker: PhantomData,
        }
    }

    #[inline]
    pub(crate) fn with_capacity(cap: usize) -> Vec2<A, B> {
        if cap == 0 {
            Vec2::new()
        } else {
            let bbb_ptr = unsafe { Vec2Layout::<A, B>::new(cap).alloc() };
            Vec2 {
                bbb_ptr,
                len: 0,
                cap,
                _marker: PhantomData,
            }
        }
    }

    #[inline]
    pub(crate) fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub(crate) fn capacity(&self) -> usize {
        self.cap
    }

    #[inline]
    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn aaa_ptr(&self) -> NonNull<A> {
        unsafe { NonNull::new_unchecked(self.bbb_ptr.cast::<A>().as_ptr().sub(self.cap)) }
    }

    #[inline]
    fn bbb_ptr(&self) -> NonNull<B> {
        self.bbb_ptr
    }

    #[inline]
    pub(crate) fn aaa(&self) -> &[A] {
        unsafe { slice::from_raw_parts(self.aaa_ptr().as_ptr(), self.len) }
    }

    #[inline]
    pub(crate) fn aaa_mut(&mut self) -> &mut [A] {
        unsafe { slice::from_raw_parts_mut(self.aaa_ptr().as_ptr(), self.len) }
    }

    #[inline]
    fn aaa_uninit(&mut self) -> &mut [MaybeUninit<A>] {
        unsafe { slice::from_raw_parts_mut(self.aaa_ptr().as_ptr() as *mut _, self.cap) }
    }

    #[inline]
    pub(crate) fn bbb(&self) -> &[B] {
        unsafe { slice::from_raw_parts(self.bbb_ptr().as_ptr(), self.len) }
    }

    #[inline]
    fn bbb_mut(&mut self) -> &mut [B] {
        unsafe { slice::from_raw_parts_mut(self.bbb_ptr().as_ptr(), self.len) }
    }

    #[inline]
    fn bbb_uninit(&mut self) -> &mut [MaybeUninit<B>] {
        unsafe { slice::from_raw_parts_mut(self.bbb_ptr().as_ptr() as *mut _, self.cap) }
    }

    // This is what `Vec` does.
    const MIN_NON_ZERO_CAP: usize = if mem::size_of::<(A, B)>() == 1 {
        8
    } else if mem::size_of::<(A, B)>() <= 1024 {
        4
    } else {
        1
    };

    #[allow(clippy::mem_forget)]
    #[cold]
    fn reserve_slow(&mut self, additional: usize) {
        debug_assert!(self.cap - self.len < additional);

        let required_cap = self.len.checked_add(additional).expect("capacity overflow");
        let new_cap = cmp::max(required_cap, Self::MIN_NON_ZERO_CAP);
        let new_cap = cmp::max(new_cap, self.cap * 2);
        let new = Self::with_capacity(new_cap);
        unsafe {
            ptr::copy_nonoverlapping(self.aaa_ptr().as_ptr(), new.aaa_ptr().as_ptr(), self.len);
            ptr::copy_nonoverlapping(self.bbb_ptr().as_ptr(), new.bbb_ptr().as_ptr(), self.len);
            self.dealloc();
            self.bbb_ptr = new.bbb_ptr;
            mem::forget(new);
            self.cap = new_cap;
        }
    }

    #[inline]
    pub(crate) fn reserve(&mut self, additional: usize) {
        if self.cap - self.len < additional {
            self.reserve_slow(additional);
        }
    }

    #[inline]
    unsafe fn dealloc_impl(data: NonNull<B>, cap: usize) {
        if cap != 0 {
            Vec2Layout::<A, B>::new(cap).dealloc(data);
        }
    }

    /// Deallocate, but do not call destructors.
    #[inline]
    unsafe fn dealloc(&mut self) {
        Self::dealloc_impl(self.bbb_ptr, self.cap);
    }

    unsafe fn drop_in_place(&mut self) {
        ptr::drop_in_place::<[A]>(self.aaa_mut());
        ptr::drop_in_place::<[B]>(self.bbb_mut());
    }

    #[inline]
    pub(crate) fn push(&mut self, a: A, b: B) {
        self.reserve(1);
        let len = self.len;
        unsafe {
            self.aaa_uninit().get_unchecked_mut(len).write(a);
            self.bbb_uninit().get_unchecked_mut(len).write(b);
        }
        self.len += 1;
    }

    #[inline]
    pub(crate) fn get(&self, index: usize) -> Option<(&A, &B)> {
        if index < self.len {
            unsafe {
                let a = self.aaa().get_unchecked(index);
                let b = self.bbb().get_unchecked(index);
                Some((a, b))
            }
        } else {
            None
        }
    }

    #[inline]
    pub(crate) unsafe fn get_unchecked(&self, index: usize) -> (&A, &B) {
        debug_assert!(index < self.len);
        (
            self.aaa().get_unchecked(index),
            self.bbb().get_unchecked(index),
        )
    }

    #[inline]
    pub(crate) unsafe fn get_unchecked_mut(&mut self, index: usize) -> (&mut A, &mut B) {
        debug_assert!(index < self.len);
        let k_ptr = self.aaa_ptr().as_ptr();
        let v_ptr = self.bbb_ptr().as_ptr();
        (&mut *k_ptr.add(index), &mut *v_ptr.add(index))
    }

    #[inline]
    unsafe fn read(&self, index: usize) -> (A, B) {
        debug_assert!(index < self.len);
        let (a, b) = self.get_unchecked(index);
        (ptr::read(a), ptr::read(b))
    }

    pub(crate) fn remove(&mut self, index: usize) -> (A, B) {
        assert!(index < self.len);
        unsafe {
            let (a, b) = self.read(index);
            ptr::copy(
                self.aaa_ptr().as_ptr().add(index + 1),
                self.aaa_ptr().as_ptr().add(index),
                self.len - index - 1,
            );
            ptr::copy(
                self.bbb_ptr().as_ptr().add(index + 1),
                self.bbb_ptr().as_ptr().add(index),
                self.len - index - 1,
            );
            self.len -= 1;
            (a, b)
        }
    }

    #[inline]
    pub(crate) fn clear(&mut self) {
        unsafe {
            self.drop_in_place();
            self.len = 0;
        }
    }

    #[inline]
    pub(crate) fn pop(&mut self) -> Option<(A, B)> {
        let new_len = self.len.checked_sub(1)?;
        let (a, b) = unsafe { self.read(new_len) };
        self.len = new_len;
        Some((a, b))
    }

    #[inline]
    pub(crate) fn iter(&self) -> iter::Iter<'_, A, B> {
        iter::Iter {
            aaa: self.aaa().iter(),
            bbb: self.bbb_ptr(),
            _marker: PhantomData,
        }
    }

    #[allow(clippy::mem_forget)]
    #[inline]
    pub(crate) fn into_iter(self) -> iter::IntoIter<A, B> {
        let iter = iter::IntoIter {
            aaa_begin: self.aaa_ptr(),
            bbb_begin: self.bbb_ptr(),
            bbb_end: unsafe { NonNull::new_unchecked(self.bbb_ptr().as_ptr().add(self.len)) },
            bbb_ptr: self.bbb_ptr,
            cap: self.cap,
        };
        mem::forget(self);
        iter
    }

    pub(crate) fn sort_insertion_by<F>(&mut self, mut compare: F)
    where
        F: FnMut((&A, &B), (&A, &B)) -> Ordering,
    {
        insertion_sort(
            self,
            self.len,
            |vec2, i, j| unsafe {
                compare(vec2.get_unchecked(i), vec2.get_unchecked(j)) == Ordering::Less
            },
            |vec2, a, b| {
                slice_swap_shift(vec2.aaa_mut(), a, b);
                slice_swap_shift(vec2.bbb_mut(), a, b);
            },
        );
    }

    pub(crate) fn sort_by<F>(&mut self, mut compare: F)
    where
        F: FnMut((&A, &B), (&A, &B)) -> Ordering,
    {
        // Constant from rust stdlib.
        const MAX_INSERTION: usize = 20;
        if self.len() <= MAX_INSERTION {
            self.sort_insertion_by(compare);
            return;
        }

        // TODO: sort without allocation.
        // TODO: drain.
        let mut entries: Vec<(A, B)> = mem::take(self).into_iter().collect();
        entries.sort_by(|(xa, xb), (ya, yb)| compare((xa, xb), (ya, yb)));
        for (a, b) in entries {
            self.push(a, b);
        }
    }
}

impl<A, B> Drop for Vec2<A, B> {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            if self.cap != 0 {
                self.drop_in_place();
                self.dealloc();
            }
        }
    }
}

impl<'s, A, B> IntoIterator for &'s Vec2<A, B> {
    type Item = (&'s A, &'s B);
    type IntoIter = iter::Iter<'s, A, B>;

    #[inline]
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl<A: Allocative, B: Allocative> Allocative for Vec2<A, B> {
    fn visit<'a, 'b: 'a>(&self, visitor: &'a mut Visitor<'b>) {
        let mut visitor = visitor.enter_self_sized::<Self>();
        if self.cap != 0 {
            let mut visitor =
                visitor.enter_unique(allocative::Key::new("ptr"), mem::size_of::<*const ()>());
            {
                let mut visitor = visitor.enter(
                    allocative::Key::new("data"),
                    Vec2Layout::<A, B>::new(self.cap).layout.size(),
                );
                for (a, b) in self {
                    a.visit(&mut visitor);
                    b.visit(&mut visitor);
                }
                visitor.exit();
            }
            visitor.exit();
        }
        visitor.exit();
    }
}

#[cfg(test)]
mod tests {
    use std::alloc::Layout;
    use std::marker::PhantomData;

    use crate::vec2::Vec2;
    use crate::vec2::Vec2Layout;

    #[test]
    fn test_layout_for() {
        assert_eq!(
            Vec2Layout {
                offset_of_bbb: 4,
                layout: Layout::from_size_align(8, 4).unwrap(),
                _marker: PhantomData,
            },
            Vec2Layout::<[u8; 3], u32>::new(1)
        );
    }

    #[test]
    fn test_alloc_dealloc() {
        unsafe {
            let layout = Vec2Layout::<[u8; 3], u32>::new(100);
            let data = layout.alloc();
            layout.dealloc(data);
        }
    }

    #[test]
    fn test_push() {
        let mut v = Vec2::new();
        v.push(1, 2);
        assert_eq!(1, v.len());
        assert_eq!(Some((&1, &2)), v.get(0));
    }

    #[test]
    fn test_push_many() {
        let mut v = Vec2::new();
        for i in 0..100 {
            v.push(i.to_string(), i * 2);
        }
        assert_eq!(100, v.len());
        for i in 0..100 {
            assert_eq!(Some((&i.to_string(), &(i * 2))), v.get(i));
        }
    }

    #[test]
    fn test_into_iter() {
        let mut v = Vec2::new();
        for i in 0..100 {
            v.push(i.to_string(), i * 2);
        }
        for (i, (a, b)) in v.into_iter().enumerate() {
            assert_eq!(i.to_string(), a);
            assert_eq!(i * 2, b);
        }
    }

    #[test]
    fn test_sort_insertion_by() {
        let mut v = Vec2::new();
        v.push(1, 2);
        v.push(3, 4);
        v.push(2, 3);
        v.push(3, 2);
        v.sort_insertion_by(|(xa, xb), (ya, yb)| (xa, xb).cmp(&(ya, yb)));
        assert_eq!(Some((&1, &2)), v.get(0));
        assert_eq!(Some((&2, &3)), v.get(1));
        assert_eq!(Some((&3, &2)), v.get(2));
        assert_eq!(Some((&3, &4)), v.get(3));
    }
}
