macro_rules! test_println {
    ($($arg:tt)*) => {
        if cfg!(test) {
            if crate::util::panicking() {
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
