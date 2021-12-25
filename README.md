# thingbuf

> "I'm at the buffer pool. I'm at the MPSC channel. I'm at the combination MPSC
> channel and buffer pool."

## What Is It?

`thingbuf` is a lock-free array-based concurrent ring buffer that allows access
to slots in the buffer by reference. It's also [asynchronous] and [blocking]
bounded MPSC channels implemented using the ring buffer.

[asynchronous]: https://crates.io/crates/thingbuf/latest/thingbuf/mpsc/index.html
[blocking]: https://crates.io/crates/thingbuf/latest/thingbuf/mpsc/sync/index.html
## When Should I Use It?

- **If you want a high-throughput bounded MPSC channel** that allocates only on
  creation. See [here](../mpsc_perf_comparison) for a detailed performance
  comparison of MPSC channels.

## FAQs

- **Q: Why did you make this?**

  **A:** For `tracing`, I wanted to be able to send formatted log lines to a
  dedicated worker thread that writes them to a file. Right now, we do this
  using `crossbeam-channel`. However, this has the sad disadvantage that we have
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