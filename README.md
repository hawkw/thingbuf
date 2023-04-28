# thingbuf

> "I'm at the buffer pool. I'm at the MPSC channel. I'm at the combination MPSC
> channel and buffer pool."

[![crates.io][crates-badge]][crates-url]
[![Documentation][docs-badge]][docs-url]
[![Documentation (HEAD)][docs-main-badge]][docs-main-url]
[![MIT licensed][mit-badge]][mit-url]
[![Test Status][tests-badge]][tests-url]
[![Sponsor @hawkw on GitHub Sponsors][sponsor-badge]][sponsor-url]

[crates-badge]: https://img.shields.io/crates/v/thingbuf.svg
[crates-url]: https://crates.io/crates/thingbuf
[docs-badge]: https://docs.rs/thingbuf/badge.svg
[docs-url]: https://docs.rs/thingbuf
[docs-main-badge]: https://img.shields.io/netlify/f2cde148-79c2-4e3f-ab2b-285dff1b9fdf?label=docs%20%28main%20branch%29
[docs-main-url]: https://thingbuf.elizas.website
[mit-badge]: https://img.shields.io/badge/license-MIT-blue.svg
[mit-url]: ../LICENSE
[tests-badge]: https://github.com/hawkw/thingbuf/actions/workflows/ci.yml/badge.svg?branch=main
[tests-url]: https://github.com/hawkw/thingbuf/actions/workflows/ci.yml
[sponsor-badge]: https://img.shields.io/badge/sponsor-%F0%9F%A4%8D-ff69b4
[sponsor-url]: https://github.com/sponsors/hawkw

## What Is It?

`thingbuf` is a lock-free array-based concurrent ring buffer that allows access
to slots in the buffer by reference. It's also [asynchronous][`thingbuf::mpsc`]
and [blocking][`thingbuf::mpsc::blocking`] bounded MPSC channels implemented using
the ring buffer.

### When Should I Use It?

- **If you want a high-throughput bounded MPSC channel** that allocates only on
  channel creation. Some MPSC channels have good throughput. Some other MPSC
  channels won't allocate memory per-waiter. [`thingbuf::mpsc`] has both.
  [`thingbuf::mpsc`] is a competitive choice for a general-purpose
  MPSC channel in most use cases.

  Both [asynchronous][`thingbuf::mpsc`] and [blocking][`thingbuf::mpsc::blocking`]
  MPSC channels are available, so `thingbuf` can be used in place
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

### When *Shouldn't* I Use It?

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

- **You need a blocking channel with a `select` operation**.
  I'm probably not going to implement it. I _may_ accept a PR if you raise it.

  If you need a synchronous channel with this kind of functionality,
  [`crossbeam-channel`] is probably a good choice.

- **You want an unbounded channel**. I'm not going to write an unbounded
  channel. Unbounded channels are evil.

## Terminology

This crate's API and documentation makes a distinction between the terms "queue"
and "channel". The term _queue_ will refer to the [queue abstract data
type][q-adt] in general &mdash; any first-in, first-out data structure is a
queue.

The term _channel_ will refer to a subtype of concurrent queue that also
functions as a synchronization primitive. A channel is a queue which can be
shared between multiple threads or asynchronous tasks, and which allows those
threads or tasks to wait for elements to be added or removed from the queue.

In the Rust standard library, the [`std::collections::VecDeque`] type
is an example of a queue that is not a channel: it is a first-in, first-out data
structure, but it cannot be concurrently enqueued to and dequeued from by
multiple threads or tasks. In comparison, the types in the [`std::sync::mpsc`]
module provide a prototypical example of channels, as they serve as
synchronization primitives for cross-thread communication.

[q-adt]: https://en.wikipedia.org/wiki/Queue_(abstract_data_type)
[`std::collections::VecDeque`]: https://doc.rust-lang.org/stable/std/collections/struct.VecDeque.html
[`std::sync::mpsc`]: https://doc.rust-lang.org/stable/std/sync/mpsc/index.html

## Usage

To get started using `thingbuf`, add the following to your `Cargo.toml`:

```toml
[dependencies]
thingbuf = "0.1"
```

By default, `thingbuf` depends on the Rust standard library, in order to
implement APIs such as synchronous (blocking) channels. In `#![no_std]`
projects, the `std` feature flag must be disabled:

```toml
[dependencies]
thingbuf = { version = "0.1", default-features = false }
```

With the `std` feature disabled, `thingbuf` will depend only on `libcore`. This
means that APIs that require dynamic memory allocation will not be enabled.
Statically allocated [channels][static-mpsc] and [queues][static-queue] are
available for code without a memory allocator, if the `static` feature flag is
enabled:

```toml
[dependencies]
thingbuf = { version = "0.1", default-features = false, features = ["static"] }
```

However, if a memory allocator _is_ available, `#![no_std]` code can also enable
the `alloc` feature flag to depend on `liballoc`:

```toml
[dependencies]
thingbuf = { version = "0.1", default-features = false, features = ["alloc"] }
```

### Crate Feature Flags

- **std** (_Enabled by default_): Enables features that require the Rust
  standard library, such as
  synchronous (blocking) channels. This implicitly enables the "alloc" feature
  flag.
- **alloc**: Enables features that require `liballoc` (but not `libstd`). This
  enables `thingbuf` queues and asynchronous channels where the size of the
  channel is determined at runtime.
- **static** (_Disabled by default, requires Rust 1.59+_): Enables the static
  (const-generic-based) `thingbuf` queues and channels. These can be used
  without dynamic memory allocation when the size of a queue or channel is known
  at compile-time.

### Compiler Support

`thingbuf` is built against the latest stable release. The minimum supported
version is Rust 1.57. The current `thingbuf` version is not guaranteed to build on Rust
versions earlier than the minimum supported version.

Some feature flags may require newer Rust releases. For example, the "static"
feature flag requries Rust 1.60+.

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
[`thingbuf::mpsc::blocking`]: https://docs.rs/thingbuf/0.1/thingbuf/mpsc/blocking/index.html
[static-queue]: https://docs.rs/thingbuf/0.1/thingbuf/struct.StaticThingBuf.html
[static-mpsc]: https://docs.rs/thingbuf/0.1./thingbuf/mpsc/struct.StaticChannel.html
[`futures::channel::mpsc`]: https://docs.rs/futures/latest/futures/channel/mpsc/index.html
[`std::sync::mpsc::sync_channel`]: https://doc.rust-lang.org/stable/std/sync/mpsc/fn.sync_channel.html
[`tokio::sync::mpsc`]: https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html
[`tracing`]: https://crates.io/crates/tracing
[`crossbeam-channel`]: https://crates.io/crates/crossbeam-channel
