use thingbuf::mpsc;

#[test]
fn mpsc_debug() {
    let (tx, rx) = mpsc::channel(4);
    println!("tx={:#?}", tx);
    println!("rx={:#?}", rx);
    let _ = tx.try_send(1);
    println!("rx={:#?}", rx);
    drop(tx);
    println!("rx={:#?}", rx);
}
