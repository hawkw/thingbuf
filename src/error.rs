#[derive(Debug)]
#[non_exhaustive]
pub enum TrySendError {
    AtCapacity(AtCapacity),
    Closed(Closed),
}

#[derive(Debug)]
pub struct AtCapacity(pub(crate) usize);

#[derive(Debug)]
pub struct Closed(pub(crate) ());
