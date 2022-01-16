pub trait Recycle<T> {
    /// Returns a new instance of type `T`.
    fn new_element(&self) -> T;

    /// Resets `element` in place.
    ///
    /// Typically, this retains any previous allocations.
    fn recycle(&self, element: &mut T);
}

#[derive(Debug, Default)]
pub struct DefaultRecycle(());

impl<T> Recycle<T> for DefaultRecycle
where
    T: Default + Clone,
{
    fn new_element(&self) -> T {
        T::default()
    }

    fn recycle(&self, element: &mut T) {
        element.clone_from(&T::default())
    }
}
