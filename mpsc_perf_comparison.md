# Comparing MPSC Channel Performance

Let's compare `thingbuf::mpsc`'s performance properties with those of other popular
multi-producer, single-consumer (MPSC) channel implementations.

Before we can make a meaningful analysis of such a comparison, however, we need
to identify the characteristics of a MPSC channel that are important, and _why_
they're important. The primary properties we'll be looking at are the channel's _throughput_,
_latency_, and _memory use_.

## Throughput

This is probably the first thing we think of when we think about measuring a
channel's performance: how many messages can we send and receive on the
channel in a fixed period of time? This gives us a sense of the channel's
overall time performance.

Some important things to note about measuring channel throughput:

- _No real application will saturate a channel's maximum throughput_. Here's
  something that, in retrospect, will seem quite obvious: all software must do
  _something_ beyond just sending and receiving MPSC channel messages. In a
  benchmark, the thread or task that receives messages from a channel can simply
  discard them without doing anything, but in a real program...we're sending
  those messages to the receiver for a _reason_, so it must do some actual work
  with the messages we send it.

  If the receiver is not able to process messages as fast as they are produced,
  the channel will fill up and the senders will have to slow down, regardless of
  how efficient the actual channel implementation is. In practice, the
  bottleneck will almost always be the receiver processing messages, rather than
  the channel's maximum throughput.

  The reason we measure throughput is not because we expect to saturate the
  channel, but because it may give us a sense of the overall overhead introduced
  by a channel implementation.

- _Throughput will depend on contention_. In almost all cases, the time it takes
  to receive _n_ messages will increase based on the number of threads concurrently
  sending messages. Therefore, we must measure throughput with varying numbers
  of senders in order to analyze how contention affects throughput.

  Importantly, if we have a sense of how contended a channel will be, it may
  influence our choice of implementation! Some implementations might outperform
  others at low contention, but lose throughput more rapidly as contention
  increases. Others might have worse throughput at low contentention, but suffer
  less as the number of senders increases. If we know in advance the maximum
  number of senders in our program, this can influence the which implementation
  is best suited for our use case!

- _We can measure throughput by sending a fixed number of messages_. We defined
  throughput as the number of messages sent over a fixed time interval. Note
  that this is the inverse of how long it takes to send a fixed number of
  messages. It's often much easier to write benchmarks that send _n_ messages
  and record how long it takes, than to record how many messages can be sent in
  _s_ seconds.

## Latency

TODO(eliza): write this part

## Memory

All the channel implementations we will be discussing here are _bounded_. This
means they enforce a limit on the maximum number of messages that may be in
flight at any given point in time. When this bound is reached, _backpressure_ is
exerted on senders, telling them to either wait or shed load until the channel
has capacity for new messages.

Why are we only discussing bounded channels? Because [unbounded queues are
evil][backpressure1] and should never be used[^unbounded]. The primary reason we
want to avoid the use of unbounded queues in our software is that they often
result in programs running out of memory. Each message in a channel occupies
memory. If the receiver cannot consume messages as fast as they are produced, an
unbounded queue will allow producers to continue producing messages, using more
and more memory. Eventually, the program probably gets OOMkilled.

If we are choosing to use a bounded channel in our program, we probably care
about limiting the maximum amount of memory that is used by the channel.
Naïvely, we might assume that all bounded channels have bounded memory use &mdash;
if the channel will only store a fixed number of messages, it will never
allocate more memory than is needed to store those messages...right?

HAHAHAHA, WRONG. When a bounded channel is full, backpressure is exerted on
senders. Typically, this means that sending threads will block (in a synchronous
channel) or sending tasks will yield (in an asynchronous channel) until there is
capacity remaining to send their messages. Because these are _multiple-producer_
channels, this means that a variable number of threads or tasks might be waiting
for capacity to send messages, and the channel must have some way of keeping
track of those waiters in order to wake them up again when there is capacity for
their messages.

Depending on how a channel is implemented, storing these waiting tasks or
threads may require additional memory allocations. This means that if the
channel is at capacity, the amount of memory it consumes _may continue to
increase_ as more and more waiters are added to the list of threads or tasks
waiting for capacity. This means that some "bounded" channels are, in a sense,
not bounded at all! If we are concerned about memory exhaustion, we need to ask
if the memory use of the channel is proportional to the number of waiters.

There are other aspects of a channel's memory use that we might want to pay
attention to. We might also want to know if memory is ever _released_ over the
channel's lifetime. Based on how a channel stores the list of waiting senders, the
it may or may not deallocate that memory as the number of waiting senders
decreases.[^vec]

Whether or not this matters depends on our particular workload. Imagine we have
a workload that's _bursty_: the number of messages sent over a given time
interval typically falls in some range, but occasionally, the number of messages
spikes dramatically &mdash; and the number of waiting senders increases as well.
In that case, a channel that never releases allocated
memory for waiters will continue to use the amount of space required for the
_highest number of senders it has ever seen_ over its lifetime, even if that
much memory is not required most of the time. Similarly, if a channel lasts for
the entire runtime of our program, we might care about whether the memory it
uses ever goes down, whereas if the channels are frequently created and
destroyed, it might be fine for them to not release memory until they are
dropped.

Heap allocations can also directly impact throughput and latency. Interestingly,
the performance impact of heap allocations is often lower in microbenchmarks
than it is in real applications. If we run a benchmark where a number of
messages are sent over a channel, the channel is the _only_ part of that program
allocating memory while the benchmark runs. Its allocations will likely follow a
particular pattern; they might all be of similar size, and so on. This is
generally a favorable condition for most allocator implementations! Allocators
_love_ it when you make a bunch of same-sized allocations and deallocate them in
a predictable pattern. In real software, though, other parts of the program are
also allocating and deallocating memory, and the pattern of allocations and
deallocations becomes much less regular. And, increased contention in the
allocator may also impact its performance for other, unrelated parts of the
program that are concurrently allocating memory. Therefore, microbenchmarks tend
not to capture the complete performance impact of heap allocations, both in the
code being benchmarked and on unrelated parts of the software.

Finally, in embedded systems and other memory-constrained environments, the
system may only have a very limited amount of memory available, so we may want
to limit the amount of heap allocations our channel performs. In some use cases,
we may not be able to make heap allocations _at all_. This will almost certainly
limit which implementations we can use.

## Performance Comparison &mdash; Async

The following benchmark measures the time taken to send 1000 `i32`s
on an asynchronous channel, as the number of sending tasks increases. It
compares the performance of several popular asynchronous MPSC channel
implementations ([`tokio::sync::mpsc`], [`async_std::channel`], and
[`futures::channel::mpsc`]) with [`thingbuf::mpsc`].

![Benchmark results](https://raw.githubusercontent.com/hawkw/thingbuf/main/assets/async_mpsc_integer-8c882b0/report/lines.svg)

[`tokio::sync::mpsc`]: https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html
[`async_std::channel`]: https://docs.rs/async-std/latest/async_std/channel/index.html
[`futures::channel::mpsc`]: https://docs.rs/futures/latest/futures/channel/mpsc/index.html
[`thingbuf::mpsc`]: https://docs.rs/thingbuf/latest/thingbuf/mpsc/index.html

The benchmark was performed with the following versions of each crate:
- [`tokio`] v1.14.0, with the "parking_lot" feature flag enabled
- [`async-channel`] v1.16.1, default feature flags
- [`futures-channel`] v0.3.18, default feature flags

Each benchmark was run on a separate `tokio` multi-threaded runtime, in
sequence.

<details>

<summary>Description of the benchmark environment</summary>

```bash
:# eliza at noctis in thingbuf
:; rustc --version
rustc 1.57.0 (f1edd0429 2021-11-29)

:# eliza at noctis in thingbuf
:; uname -a
Linux noctis 5.14.9 #1-NixOS SMP Thu Sep 30 08:13:08 UTC 2021 x86_64 GNU/Linux

:# eliza at noctis in thingbuf
:; lscpu
Architecture:                    x86_64
CPU op-mode(s):                  32-bit, 64-bit
Byte Order:                      Little Endian
Address sizes:                   43 bits physical, 48 bits virtual
CPU(s):                          24
On-line CPU(s) list:             0-23
Thread(s) per core:              2
Core(s) per socket:              12
Socket(s):                       1
NUMA node(s):                    1
Vendor ID:                       AuthenticAMD
CPU family:                      23
Model:                           113
Model name:                      AMD Ryzen 9 3900X 12-Core Processor
Stepping:                        0
Frequency boost:                 enabled
CPU MHz:                         3800.000
CPU max MHz:                     4672.0698
CPU min MHz:                     2200.0000
BogoMIPS:                        7585.77
Virtualization:                  AMD-V
L1d cache:                       384 KiB
L1i cache:                       384 KiB
L2 cache:                        6 MiB
L3 cache:                        64 MiB
NUMA node0 CPU(s):               0-23
Vulnerability Itlb multihit:     Not affected
Vulnerability L1tf:              Not affected
Vulnerability Mds:               Not affected
Vulnerability Meltdown:          Not affected
Vulnerability Spec store bypass: Mitigation; Speculative Store Bypass disabled via prctl and seccomp
Vulnerability Spectre v1:        Mitigation; usercopy/swapgs barriers and __user pointer sanitization
Vulnerability Spectre v2:        Mitigation; Full AMD retpoline, IBPB conditional, STIBP conditional, RSB filling
Vulnerability Srbds:             Not affected
Vulnerability Tsx async abort:   Not affected
Flags:                           fpu vme de pse tsc msr pae mce cx8 apic sep mtrr pge mca cmov pat pse36 clflush mmx fxsr sse sse2 ht syscall nx
                                 mmxext fxsr_opt pdpe1gb rdtscp lm constant_tsc rep_good nopl nonstop_tsc cpuid extd_apicid aperfmperf rapl pni p
                                 clmulqdq monitor ssse3 fma cx16 sse4_1 sse4_2 movbe popcnt aes xsave avx f16c rdrand lahf_lm cmp_legacy svm exta
                                 pic cr8_legacy abm sse4a misalignsse 3dnowprefetch osvw ibs skinit wdt tce topoext perfctr_core perfctr_nb bpext
                                  perfctr_llc mwaitx cpb cat_l3 cdp_l3 hw_pstate ssbd mba ibpb stibp vmmcall fsgsbase bmi1 avx2 smep bmi2 cqm rdt
                                 _a rdseed adx smap clflushopt clwb sha_ni xsaveopt xsavec xgetbv1 xsaves cqm_llc cqm_occup_llc cqm_mbm_total cqm
                                 _mbm_local clzero irperf xsaveerptr rdpru wbnoinvd arat npt lbrv svm_lock nrip_save tsc_scale vmcb_clean flushby
                                 asid decodeassists pausefilter pfthreshold avic v_vmsave_vmload vgif v_spec_ctrl umip rdpid overflow_recov succo
                                 r smca sme sev sev_es
```

</details>

### Analysis

Here's my analysis of this benchmark, plus looking at the implementation of each
channel tested.

#### `tokio::sync::mpsc`

[Tokio's MPSC channel][`tokio::sync::mpsc`] stores messages using an [atomic
linked list of blocks][tokio-ll]. To improve performance, rather than storing
each individual element in a separate linked list node, each linked list node
instead contains [an array][tokio-ll2] that can store multiple messages. This
amortizes the overhead of allocating new nodes, and reduces the link hopping
necessary to index a message.

This approach means that all the capacity of a bounded MPSC channel is _not_
allocated up front when the channel is created. If we create a Tokio bounded
MPSC of capacity 1024, but the channel only has 100 messages in the queue, the
channel will not currently be occupying enough memory to store 1024 messages.
This may reduce the average memory use of the application if the channel is
rarely close to capacity. On the other hand, this has the downside that heap
allocations may occur as a during any given `send` operation when the channel is
not at capacity, potentially increasing allocator churn.

Because the linked list is atomic, `send` operations are lock-free when the
channel has capacity remaining.

Backpressure is implemented using Tokio's [async semaphore] to allow senders to
wait for channel capacity.[^sem] The async semaphore is implemented using an
[intrusive] doubly-linked list. In this design, the linked list node is stored
inside of the future that's waiting to send a message, rather than in a separate
heap allocation. Critically, this means that once a channel is at capacity,
additional senders waiting for capacity will _never_ cause additional memory
allocations; the channel is properly bounded.

The semaphore protects its wait list using a `Mutex`, so waiting does require
acquiring a lock, although the critical sections of this lock should be quite
short.

Tokio's MPSC has some other interesting properties that differentiate it from
some other implementations. It participates in Tokio's task budget system for
[automatic cooperative yielding][budget], which may improve tail latencies in
some workloads.[^budget] Also, it has a nice API where senders can [reserve a
`Permit` type][permit] representing the capacity to send a single message, which
can be consumed later.

In the benchmark, however, we see that `tokio::sync::mpsc` offers the worst
throughput at medium (50 senders) and high (100 senders) levels of contention,
and the second-worst at low (10 senders) contention. This is likely because the
implementation prioritizes low memory use over low latency.

In general, `tokio::sync::mpsc` is probably a decent choice for channels that are
long-lived and may have a very large number of tasks waiting for send capacity,
because the intrusive wait list means that senders waiting for capacity will not
cause additional memory allocations. While its throughput is poor compared to
other implementations, if the receiving task performs a lot of work for each
message, this may not have a major impact.

However, I will also note that `thingbuf`'s MSPC channels _also_ use an
intrusive wait list and don't require per-waiter allocations, just like Tokio's.
If a channel with better throughput is needed, `thingbuf`'s MPSC channel may be
preferable.

Finally, the `tokio` crate does not support `no_std`, with or without `alloc`.

[`tokio`]: https://tokio.rs
[tokio-ll]: https://github.com/tokio-rs/tokio/blob/78e0f0b42a4f7a50f3986f576703e5a3cb473b79/tokio/src/sync/mpsc/list.rs
[tokio-ll2]: https://github.com/tokio-rs/tokio/blob/78e0f0b42a4f7a50f3986f576703e5a3cb473b79/tokio/src/sync/mpsc/block.rs#L28-L31
[async semaphore]: https://github.com/tokio-rs/tokio/blob/78e0f0b42a4f7a50f3986f576703e5a3cb473b79/tokio/src/sync/batch_semaphore.rs
[budget]: https://tokio.rs/blog/2020-04-preemption
[permit]: https://docs.rs/tokio/latest/tokio/sync/mpsc/struct.Sender.html#method.reserve

#### `futures-channel`

The [`futures`] crate re-exports a channel implementation from the
[`futures-channel`] crate. This is one of the oldest MPSC channels in
async Rust, and its API in many ways provides the template on which the other
channels were based. `futures-channel` [uses a linked list][futures-ll1] as a wait
queue[^vyukov1]. This linked list is not intrusive: each entry has its own heap
allocation. The same linked list is [used to store the messages in the
channel][futures-ll2].

In my opinion, `futures-channel` **should generally not be used**. That may
sound inflammatory, but here's why:

1. Throughput at all levels of contention is relatively poor, as the line graph
   shows. In the low-contention case (10 senders), `futures-channel` was the
   slowest, and it was the second slowest in the medium- (50 senders) and
   high-contention (100 senders) cases.
2. Because the linked list wait queue is not intrusive, every waiting task will
   require an additional memory allocation. This means that the maximum memory
   use of the channel is _not actually bounded_.
3. The same linked list implementation is used for messages in the channel,
   rather than an array. This means sending a message will always cause a heap
   allocation, and receiving one will always cause a deallocation, resulting in
   increased allocator churn that may impact overall allocator performance.

The [`futures-channel`] crate has limited support for `no_std` (with `alloc`),
but the MPSC channel is not supported without the standard library.

[`futures`]: https://docs.rs/futures-channel/latest/futures_channel/
[`futures-channel`]: https://docs.rs/futures-channel/latest/futures_channel/
[futures-ll1]: https://github.com/rust-lang/futures-rs/blob/master/futures-channel/src/mpsc/queue.rs
[futures-ll2]: https://github.com/rust-lang/futures-rs/blob/dacf2565d4adfa978b342770190b9a6a1c56329a/futures-channel/src/mpsc/mod.rs#L289-L293

#### `async-channel`

The [`async-std`] library provides bounded and unbounded channels via a
re-export of the [`async-channel`] crate. This is actually a multi-producer,
multi-consumer (MPMP) channel, rather than a MPSC channel; I've included it in
this comparison because [`async-std`] doesn't implement MPSC channels.

Messages sent in the channel [are stored using a bounded lock-free queue][cq1]
from the [`concurrent-queue`] crate, while the wait queue is implemented using
the [`event-listener`] crate.[^annoying]

The [`concurrent-queue`] crate's bounded channel implements [Dmitry Vyukov's
MPMC array-based lock-free queue][vyukov-q]. This is the same algorithm used by
`thingbuf`. Because this queue uses an array to store messages, a single
allocation is performed when the channel is constructed; after that, no `send`
operation will allocate memory as long as the channel has capacity. When there
is capacity in the channel, `send` operations are lock-free.

[`event-listener`] implements a type of wait queue using a non-intrusive linked
list. This means that every waiter requires an additional memory allocation.
Therefore, the channel's maximum memory usage is not actually bounded. Also,
allocating linked list nodes may cause allocator churn that could impact other
parts of the system. The linked list is protected by a lock, so waiting requires
a lock acquisition.

In the benchmark, `async-channel` was the fastest implementation tested in the
medium (50 senders) and high (100 senders) contention tests, and performed very
similarly to `thingbuf` in the low contention (10 senders) test. This means that
it offers very good throughput, and presumably also low latency.

I would consider the use of [`async-channel`] in cases where extremely high
throughput is vital, but it isn't actually necessary to bound maximum memory
use (e.g. some other part of the system enforces a limit on the number of
sending tasks). Outside of microbenchmarks, though, throughput is likely to be
less of a priority than memory use.

`async-channel` does not support `no_std`, with or without `alloc`.

[`async-std`]: https://crates.io/crates/async-std
[`async-channel`]: https://crates.io/crates/async-channel
[cq1]: https://github.com/smol-rs/async-channel/blob/eb2b12e50a23ca3624795855aca8eb614a5d7086/src/lib.rs#L47-L48
[`concurrent-queue`]: https://crates.io/crates/concurrent-queue
[`event-listener`]: https://crates.io/crates/event-listener

#### `thingbuf::mpsc`

TODO(eliza): write this part

[backpressure1]: https://www.tedinski.com/2019/03/05/backpressure.html#its-always-queues-isnt-it
[sem1]: https://github.com/tokio-rs/tokio/commit/acf8a7da7a64bf08d578db9a9836a8e061765314#diff-f261032a1ebbdd2bf7765bc703d8e6a215ab6d1b9c38423b7b991a824042bf6f
[intrusive]: https://www.data-structures-in-practice.com/intrusive-linked-lists/
[`tokio::task::unconstrained`]: https://docs.rs/tokio/latest/tokio/task/fn.unconstrained.html
[vyukov-q]: https://www.1024cores.net/home/lock-free-algorithms/queues/bounded-mpmc-queue
[`Vec`]: https://doc.rust-lang.org/stable/std/vec/struct.Vec.html
[`Vec::shrink_to_fit`]: https://doc.rust-lang.org/stable/std/vec/struct.Vec.html#method.shrink_to_fit

[^unbounded]: Unless you really, actually know what you're doing. This footnote
is here because someone on hackernews will probably yell at me unless I include
a disclaimer telling them that they're a _very special boy_ who's allowed to use
unbounded queues.

[^vec]: For example, if the channel stores waiting senders in a [`Vec`], and
never calls [`Vec::shrink_to_fit`] on it, the amount of memory it occupies will
never go back down as the number of waiters decreases.

[^sem]: I happen to be quite familiar with the implementation details of the
Tokio semaphore, because [I wrote the initial implementation of it][sem1].

[^sem]: Note, however, that I disabled the task budget system in my benchmark by
using [`tokio::task::unconstrained`]. Automatic cooperative yielding will
improve tail latencies in real world systems, but in a benchmark consisting
_only_ of sending and recieving channel messages, it just means the benchmark
will spend more time in the task scheduler.

[^vyukov1]: Based on Dmitry Vyukov's [non-intrusive lock-free MPSC
queue](http://www.1024cores.net/home/lock-free-algorithms/queues/non-intrusive-mpsc-node-based-queue),
a rather neat lock-free queue algorithm.

[^annoying]: I honestly had kind of a hard time analyzing the implementation of
`async-std`'s channel because EVERYTHING IS SPLIT ACROSS SO MANY TINY CRATES.