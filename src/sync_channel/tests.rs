use super::*;
use crate::loom::{self, thread};

#[test]
fn basically_works() {
    loom::model(|| {
        let (tx, rx) = sync_channel(crate::ThingBuf::new(4));

        let p1 = {
            let tx = tx.clone();
            thread::spawn(move || {
                tx.try_send_ref().unwrap().with_mut(|val| *val = 1);
                tx.try_send_ref().unwrap().with_mut(|val| *val = 2);
            })
        };
        let p2 = thread::spawn(move || {
            tx.try_send(3).unwrap();
            tx.try_send(4).unwrap();
        });

        let mut vals = Vec::new();

        for val in &rx {
            val.with(|val| vals.push(*val));
        }

        vals.sort_unstable();
        assert_eq!(vals, vec![1, 2, 3, 4]);

        p1.join().unwrap();
        p2.join().unwrap();
    })
}
