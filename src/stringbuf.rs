use super::*;

use alloc::string::String;

#[derive(Debug)]
pub struct StringBuf {
    inner: ThingBuf<String>,
    max_idle_capacity: usize,
}

impl StringBuf {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: ThingBuf::new(capacity),
            max_idle_capacity: usize::MAX,
        }
    }

    pub fn with_max_idle_capacity(self, max_idle_capacity: usize) -> Self {
        Self {
            max_idle_capacity,
            inner: self.inner,
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    #[inline]
    pub fn write(&self) -> Result<Ref<'_, String>, AtCapacity> {
        let mut string = self.inner.push_ref()?;
        string.with_mut(String::clear);
        Ok(string)
    }

    pub fn pop_ref(&self) -> Option<Ref<'_, String>> {
        let mut string = self.inner.pop_ref()?;
        string.with_mut(|string| {
            if string.capacity() > self.max_idle_capacity {
                string.shrink_to_fit();
            }
        });
        Some(string)
    }
}

#[derive(Debug)]
pub struct StaticStringBuf<const CAP: usize> {
    inner: StaticThingBuf<String, CAP>,
    max_idle_capacity: usize,
}

impl<const CAP: usize> StaticStringBuf<CAP> {
    #[cfg(not(test))]
    pub const fn new() -> Self {
        Self {
            inner: StaticThingBuf::new(),
            max_idle_capacity: usize::MAX,
        }
    }

    pub fn with_max_idle_capacity(self, max_idle_capacity: usize) -> Self {
        Self {
            max_idle_capacity,
            inner: self.inner,
        }
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.inner.capacity()
    }

    #[inline]
    pub fn write(&self) -> Result<Ref<'_, String>, AtCapacity> {
        let mut string = self.inner.push_ref()?;
        string.with_mut(String::clear);
        Ok(string)
    }

    pub fn pop_ref(&self) -> Option<Ref<'_, String>> {
        let mut string = self.inner.pop_ref()?;
        string.with_mut(|string| {
            if string.capacity() > self.max_idle_capacity {
                string.shrink_to_fit();
            }
        });
        Some(string)
    }
}
