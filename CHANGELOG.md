<a name="v0.1.4"></a>
## v0.1.4 (2023-04-26)

#### Features

* **blocking::mpsc:**  add `Receiver::recv(_ref)_timeout` methods (#75) ([b57ce88a](https://github.com/hawkw/thingbuf/commit/b57ce88ac85cf71b363035249e27f1c91e65f372))

#### Bug Fixes

* **blocking::mpsc:**  increase durations in doctests (#78) ([465fd3cb](https://github.com/hawkw/thingbuf/commit/465fd3cbcb411a4413f382afcdfab1e5d68f3a4a))



<a name="v0.1.3"></a>
### v0.1.3 (2022-05-13)


#### Features

* **mpsc:**  add `Receiver::try_recv` method  (#60) ([e04661fc](https://github.com/hawkw/thingbuf/commit/e04661fc72b08c4e2517a9f0bfb16aa7d2fed683))

#### Bug Fixes

* **mpsc:**  `try_recv_ref` should return `RecvRef` (#61) ([47f16f59](https://github.com/hawkw/thingbuf/commit/47f16f59579e7a5d11a69c47c4a3b7379b758c75))



<a name="v0.1.2"></a>
### v0.1.2 (2022-04-12)


#### Bug Fixes

*   compilation errors with `--no-default-features` (#59) ([a2ab1788](https://github.com/hawkw/thingbuf/commit/a2ab17880e273c5fcde9e1d6ca77978402839d79), closes [#58](https://github.com/hawkw/thingbuf/issues/58))



<a name="v0.1.1"></a>
### v0.1.1 (2022-03-16)


#### Performance

*   elide bounds checks when indexing (#51) ([27ea0ec4](https://github.com/hawkw/thingbuf/commit/27ea0ec45abb45049b8009a253270d5ed658fb38))
* **mpsc:**  remove panics from wait queue (#50) ([f61993f0](https://github.com/hawkw/thingbuf/commit/f61993f0e5f7e8725168b1c12d9249c979a986d3))

#### Bug Fixes

* **mpsc:**  compilation error on macOS (#53) ([d0d0cd90](https://github.com/hawkw/thingbuf/commit/d0d0cd904e900f099c985b0b9340af681f8c881b), closes [#54](https://github.com/hawkw/thingbuf/issues/54))



<a name="0.1.0"></a>
## 0.1.0 (2022-03-15)

Initial release!

#### Performance

* **mpsc:**  rewrite and optimize wait queue (#22) ([8c882b0f](https://github.com/hawkw/thingbuf/commit/8c882b0f40b5b79b1d27116920769bd184d216be))
* **mspc:**  replace bad VecDeque wait queue with intrusive list (#16) ([23f4c96f](https://github.com/hawkw/thingbuf/commit/23f4c96fa4e88aa2183f75c9afd929ca98f06fe5))

#### Features

*   add `#[must_use]` to constructors (#45) ([0299a606](https://github.com/hawkw/thingbuf/commit/0299a6064d31752ed50c49db1010fb945c5ad3d5))
*   add `into_inner` and `Error` impl to `Full` (#43) ([527a6398](https://github.com/hawkw/thingbuf/commit/527a6398eafcc23bd23bea6bd9dc29788a605789))
*   add `Deref` and `DerefMut` impls to `Ref` types (#13) ([6ebfe7b8](https://github.com/hawkw/thingbuf/commit/6ebfe7b8fd4641cdb72566a51a1e99f737888283), breaks [#](https://github.com/hawkw/thingbuf/issues/))
*   add nicer `fmt::Debug` impls (#4) ([38cbad20](https://github.com/hawkw/thingbuf/commit/38cbad20265c4ec3fd6b9bd715a1e6e36a03e72e))
* **StringBuf:**  add `StringBuf`  type ([856c1f6c](https://github.com/hawkw/thingbuf/commit/856c1f6c93f53ad3566e76b6edc33217c451791b))
* **ThingBuf:**
  *  add `pop_with` and `push_with` ([9192c603](https://github.com/hawkw/thingbuf/commit/9192c603855e0099310a8228afa9b9475df5930b))
  *  add `no_std` compatible `StaticThingBuf` (#1) ([3b23f858](https://github.com/hawkw/thingbuf/commit/3b23f8583b3f9c91d2f883f26b0ff454fabe00cf))
  *  hahahaha static storage works ([e47cd7dc](https://github.com/hawkw/thingbuf/commit/e47cd7dc80990ec4aadabe5aab229d4195254f6e))
* **mpsc:**
  *  stick errors in their own module ([3137b85e](https://github.com/hawkw/thingbuf/commit/3137b85e0dbfe9716d29b45efc41443046e726bd))
  *  add `std::error::Error` impls ([d5ac083b](https://github.com/hawkw/thingbuf/commit/d5ac083b15ef70d7d51e74ad1eaa3ca534e8a153))
  *  add methods to errors ([d5bf3db0](https://github.com/hawkw/thingbuf/commit/d5bf3db00606e3118e9d6ba21de03d784024f893))
  *  add support for statically-allocated MPSC channels (#23) ([5b17c184](https://github.com/hawkw/thingbuf/commit/5b17c184b0cf4ed4b1a5de3b4ce5bac13b75ff2f), closes [#17](https://github.com/hawkw/thingbuf/issues/17))
  *  add waiting `send`/`send_ref` (#7) ([76df064c](https://github.com/hawkw/thingbuf/commit/76df064cbfb7b18aea9ec3a7b768f0528840be21))
  *  make errors more like other mpscs (#5) ([5e749ccc](https://github.com/hawkw/thingbuf/commit/5e749ccc91140063d9287dccd36312f110c42836))
  *  initial sync and async channel APIs (#2) ([1c28c84f](https://github.com/hawkw/thingbuf/commit/1c28c84fdc9ffb895799b51fd82beb33783f7b6a))
* **recycling:**  add customizable recycling policies (#33) ([54e53534](https://github.com/hawkw/thingbuf/commit/54e53534303606d734cb7490cf782d7b71fb8103), closes [#30](https://github.com/hawkw/thingbuf/issues/30))

#### Breaking Changes

*   add `Deref` and `DerefMut` impls to `Ref` types (#13) ([6ebfe7b8](https://github.com/hawkw/thingbuf/commit/6ebfe7b8fd4641cdb72566a51a1e99f737888283), breaks [#](https://github.com/hawkw/thingbuf/issues/))

#### Bug Fixes

* **ThingBuf:**
  *  fix backwards subtraction in `len` ([caab6b23](https://github.com/hawkw/thingbuf/commit/caab6b23540ad344e87b7a1a6bcda41d6be7eb7f))
  *  fix wrong increment in pop ([0e53279c](https://github.com/hawkw/thingbuf/commit/0e53279cc25774fef5f5eab34d6ab42807b2dde5))
* **mpsc:**
  *  ensure un-received messages are dropped (#29) ([c444e50b](https://github.com/hawkw/thingbuf/commit/c444e50b8d2ca98745ae451100a4b01f84d054d5))
  *  fix a deadlock in async send_ref  (#20) ([c58c6200](https://github.com/hawkw/thingbuf/commit/c58c620096a063e275f9c16e0a89da3399b47877))



