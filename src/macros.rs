macro_rules! test_println {
    ($($arg:tt)*) => {
        if cfg!(test) {
            if crate::util::panic::panicking() {
                // getting the thread ID while panicking doesn't seem to play super nicely with loom's
                // mock lazy_static...
                println!("[PANIC {:>17}:{:<3}] {}", file!(), line!(), format_args!($($arg)*))
            } else {
                println!("[{:?} {:>17}:{:<3}] {}", crate::loom::thread::current().id(), file!(), line!(), format_args!($($arg)*))
            }
        }
    }
}

macro_rules! test_dbg {
    ($e:expr) => {
        match $e {
            e => {
                #[cfg(test)]
                test_println!("{} = {:?}", stringify!($e), &e);
                e
            }
        }
    };
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

#[allow(unused_macros)]
macro_rules! unreachable_unchecked {
    ($($arg:tt)+) => {
        crate::unreachable_unchecked!(@inner , format_args!(": {}", format_args!($($arg)*)))
    };

    () => {
        crate::unreachable_unchecked!(@inner ".")
    };

    (@inner $msg:expr) => {
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
    };
}
