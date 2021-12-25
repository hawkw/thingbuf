use thingbuf::mpsc;

#[tokio::test(flavor = "multi_thread")]
async fn basically_works() {
    use std::collections::HashSet;

    const N_SENDS: usize = 10;
    const N_PRODUCERS: usize = 10;

    async fn do_producer(tx: mpsc::Sender<usize>, n: usize) {
        let tag = n * N_SENDS;
        for i in 0..N_SENDS {
            let msg = i + tag;
            println!("sending {}...", msg);
            tx.send(msg).await.unwrap();
            println!("sent {}!", msg);
        }
        println!("PRODUCER {} DONE!", n);
    }

    let (tx, rx) = mpsc::channel(N_SENDS / 2);
    for n in 0..N_PRODUCERS {
        tokio::spawn(do_producer(tx.clone(), n));
    }
    drop(tx);

    let mut results = HashSet::new();
    while let Some(val) = {
        println!("receiving...");
        rx.recv().await
    } {
        println!("received {}!", val);
        results.insert(val);
    }

    let results = dbg!(results);

    for n in 0..N_PRODUCERS {
        let tag = n * N_SENDS;
        for i in 0..N_SENDS {
            let msg = i + tag;
            assert!(results.contains(&msg), "missing message {:?}", msg);
        }
    }
}
