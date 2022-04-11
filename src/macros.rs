macro_rules! test_println {
    ($($arg:tt)*) => {
        #[cfg(all(feature = "std", any(thingbuf_trace, test)))]
        if crate::util::panic::panicking() {
            // getting the thread ID while panicking doesn't seem to play super nicely with loom's
            // mock lazy_static...
            println!("[PANIC {:>30}:{:<3}] {}", file!(), line!(), format_args!($($arg)*))
        } else {
            crate::loom::traceln(format_args!(
                "[{:?} {:>30}:{:<3}] {}",
                crate::loom::thread::current().id(),
                file!(),
                line!(),
                format_args!($($arg)*),
            ));
        }
    }
}

macro_rules! test_dbg {
    ($e:expr) => {
        match $e {
            e => {
                #[cfg(all(feature = "std", any(thingbuf_trace, test)))]
                test_println!("{} = {:?}", stringify!($e), &e);
                e
            }
        }
    };
}

#[cfg(test)]
macro_rules! assert_dbg {
    ($e:expr) => {
        assert_dbg!(@ $e, "")
    };
    ($e:expr, $($msg:tt)+) => {
        assert_dbg!(@ $e, " ({})", format_args!($($msg)+))
    };
    (@$e:expr, $($msg:tt)+) => {
        {
            #[cfg(all(feature = "std", any(thingbuf_trace, test)))]
            test_println!("ASSERT: {}{}", stringify!($e), format_args!($($msg)*));
            assert!($e, $($msg)*);
            test_println!("-> ok");
        }
    }
}

#[cfg(test)]
macro_rules! assert_eq_dbg {
    ($a:expr, $b:expr) => {
        assert_eq_dbg!(@ $a, $b, "")
    };
    ($a:expr, $b:expr, $($msg:tt)+) => {
       assert_eq_dbg!(@ $a, $b, " ({})", format_args!($($msg)+))
    };
    (@ $a:expr, $b:expr, $($msg:tt)+) => {
        {
            #[cfg(all(feature = "std", any(thingbuf_trace, test)))]
            test_println!("ASSERT: {} == {}{}", stringify!($a), stringify!($b), format_args!($($msg)*));
            assert_eq!($a, $b, $($msg)*);
            test_println!("-> ok");
        }
    }
}

macro_rules! feature {
    (
        #![$meta:meta]
        $($item:item)*
    ) => {
        $(
            #[cfg($meta)]
            #[cfg_attr(docsrs, doc(cfg($meta)))]
            $item
        )*
    }
}

macro_rules! fmt_bits {
    ($self: expr, $f: expr, $has_states: ident, $($name: ident),+) => {
        $(
            if $self.contains(Self::$name) {
                if $has_states {
                    $f.write_str(" | ")?;
                }
                $f.write_str(stringify!($name))?;
                $has_states = true;
            }
        )+

    };
}

#[allow(unused_macros)]
macro_rules! unreachable_unchecked {
    (@inner $msg:expr) => {
        {
            #[cfg(debug_assertions)] {
                panic!(
                    "internal error: entered unreachable code{}\n\n\
                    /!\\ EXTREMELY SERIOUS WARNING /!\\\n
                    This code should NEVER be entered; in release mode, this would \
                    have been an `unreachable_unchecked` hint. The fact that this \
                    occurred means something VERY bad is going on. \n\
                    Please contact the `thingbuf` maintainers immediately. Sorry!",
                    $msg,
                );
            }
            #[cfg(not(debug_assertions))]
            unsafe {
                core::hint::unreachable_unchecked();
            }

        }
    };

    ($($arg:tt)+) => {
        unreachable_unchecked!(@inner format_args!(": {}", format_args!($($arg)*)))
    };

    () => {
        unreachable_unchecked!(@inner ".")
    };
}
