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

- **Q: Why is there only a bounded variant?**

  **A:** Because unbounded queues are of the Devil.

- **Q: Isn't this just a giant memory leak?**

  **A:** If you use it wrong, yes.

- **Q: Why is it called that?**

  **A:** Originally, I imagined it as a kind of ring buffer, so (as a pun on
  "ringbuf"), I called it "stringbuf". Then, I realized you could do this with
  more than just strings. In fact, it can be generalized to arbitrary...things.
  So, "thingbuf".

- **Q: Why don't the `Ref` types implement `Deref` and `DerefMut`?**

  **A:** [Blame `loom` for this.](https://github.com/tokio-rs/loom/pull/219)