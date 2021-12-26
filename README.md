# thingbuf

> "I'm at the buffer pool. I'm at the MPSC channel. I'm at the combination MPSC
> channel and buffer pool."

## What Is It?

`thingbuf` is a lock-free array-based concurrent ring buffer that allows access
to slots in the buffer by reference. It's also [asynchronous][`thingbuf::mpsc`]
and [blocking][`thingbuf::mpsc::sync`] bounded MPSC channels implemented using
the ring buffer.

## When Should I Use It?

- **If you want a high-throughput bounded MPSC channel** that allocates only on
  channel creation. Some MPSC channels have good throughput. Some other MPSC
  channels won't allocate memory per-waiter. [`thingbuf::mpsc`] has both. See
  [here](../mpsc_perf_comparison) for a detailed performance comparison of MPSC
  channels. [`thingbuf::mpsc`] is a competitive choice for a general-purpose
  MPSC channel in most use cases.

  Both [asynchronous][`thingbuf::mpsc`] and [blocking][`thingbuf::mpsc::sync`]
  MPSC channels are available[^blocking-std], so `thingbuf` can be used in place
  of asynchronous channels like [`futures::channel::mpsc`] *and* blocking
  channels like [`std::sync::mpsc::sync_channel`].

- **If you can't allocate** or **you need to build with `#![no_std]`** because
  you're working on embedded systems or other bare-metal software. Thingbuf
  provides a [statically-allocated MPSC channel][static-mpsc] and a
  [statically-allocated lock-free queue][static-queue]. These can be placed in a
  `static` initializer and used without requiring any runtime allocations.

- **You want to use the same MPSC channel with and without `std`** . Thingbuf's
  asynchronous MPSC channel provides an identical API and feature set regardless
  of whether or not the "std" feature flag is enabled. If you're writing a library that
  needs to conditionally support `#![no_std]`, and you need an asynchronous MPSC
  channel, it might be easier to use [`thingbuf::mpsc`] in both cases, rather
  than switching between separate `std` and `#![no_std]` channel
  implementations.

## When *Shouldn't* I Use It?

It's equally important to discuss when `thingbuf` should *not* be used. Here are
some cases where you might be better off considering other options:

- **You need a really, really, absurdly high bound and you're not going to be
  near it most of the time**. If you want to set a very, very high bound on a
  bounded MPSC channel, and the channel will typically never be anywhere near
  that full, [`thingbuf::mpsc`] might *not* be the best choice.

  Thingbuf's channels will allocate an array with length equal to the capacity
  as soon as they're constructed. This improves performance by avoiding
  additional allocations, but if you need to set very high bounds, you might
  prefer a channel implementation that only allocates memory for messages as
  it's needed (such as [`tokio::sync::mpsc`]).

- **You need a blocking channel with `send_timeout`** or **a blocking channel
  with a `select` operation**. I'm probably not going to implement these things.
  The blocking channel isn't particularly important to me compared to the async
  channel, and I _probably_ won't add a bunch of additional APIs to it.

  If you need a synchronous channel with this kind of functionality,
  [`crossbeam-channel`] is probably a good choice.

- **You want an unbounded channel**. I'm not going to write an unbounded
  channel. Unbounded channels are evil.

## FAQs

- **Q: Why did you make this?**

  **A:** For [`tracing`], I wanted to be able to send formatted log lines to a
  dedicated worker thread that writes them to a file. Right now, we do this
  using [`crossbeam-channel`]. However, this has the sad disadvantage that we have
  to allocate `String`s, send them through the channel to the writer, and
  immediately drop them. It would be nice to do this while reusing those
  allocations. Thus...`StringBuf`.

- **Q: Is it lock-free?**

  **A:** Extremely.

- **Q: Is it wait-free?**

  **A:** As long as you don't use the APIs that wait :)

- **Q: Why is there only a bounded variant?**

  **A:** Because unbounded queues are of the Devil.

- **Q: Isn't this just a giant memory leak?**

  **A:** If you use it wrong, yes.

- **Q: Why is it called that?**

  **A:** Originally, I imagined it as a kind of ring buffer, so (as a pun on
  "ringbuf"), I called it "stringbuf". Then, I realized you could do this with
  more than just strings. In fact, it can be generalized to arbitrary...things.
  So, "thingbuf".

[`thingbuf::mpsc`]: https://docs.rs/thingbuf/0.1/thingbuf/mpsc/index.html
[`thingbuf::mpsc::sync`]: https://docs.rs/thingbuf/0.1/thingbuf/mpsc/sync/index.html
[static-queue]: https://docs.rs/thingbuf/0.1/thingbuf/struct.StaticThingBuf.html
[static-mpsc]: https://docs.rs/thingbuf/0.1./thingbuf/mpsc/struct.StaticChannel.html
[`futures::channel::mpsc`]: https://docs.rs/futures/latest/futures/channel/mpsc/index.html
[`std::sync::mpsc::sync_channel`]: https://doc.rust-lang.org/stable/std/sync/mpsc/fn.sync_channel.html
[`tokio::sync::mpsc`]: https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html
[`tracing`]: https://crates.io/crates/tracing
[`crossbeam-channel`]: https://crates.io/crates/crossbeam-channel

[^blocking-std]: The synchronous (blocking) channel naturally requires `std` in
order to park threads.