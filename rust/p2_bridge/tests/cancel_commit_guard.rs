use _rust_il::callbacks::CancelFlag;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};
use pyo3::Python;

#[test]
fn cancelled_flag_prevents_commit() {
    let flag = CancelFlag::default();
    flag.cancel();
    let mut called = false;
    let result = flag.commit_when_live(|| {
        called = true;
        Ok::<_, std::io::Error>(())
    }).unwrap();
    assert!(result.is_none());
    assert!(!called);
}

#[test]
fn commit_error_remains_an_error() {
    let flag = CancelFlag::default();
    let result = flag.commit_when_live(|| Err::<(), _>(std::io::Error::other("publish failed")));
    assert!(result.is_err());
    flag.cancel();
    assert!(flag.is_cancelled());
}

#[test]
fn commit_and_cancel_have_a_single_order() {
    let flag = CancelFlag::default();
    let events = Arc::new(Mutex::new(Vec::new()));
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let commit_flag = flag.clone();
    let commit_events = Arc::clone(&events);
    let commit = thread::spawn(move || {
        commit_flag.commit_when_live(|| {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            commit_events.lock().unwrap().push("commit");
            Ok::<_, std::io::Error>(())
        }).unwrap()
    });
    entered_rx.recv().unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let cancel_events = Arc::clone(&events);
    let cancel = thread::spawn(move || {
        started_tx.send(()).unwrap();
        flag.cancel();
        cancel_events.lock().unwrap().push("cancel");
    });
    started_rx.recv().unwrap();
    release_tx.send(()).unwrap();
    assert_eq!(commit.join().unwrap(), Some(()));
    cancel.join().unwrap();
    assert_eq!(*events.lock().unwrap(), vec!["commit", "cancel"]);
}

#[test]
fn cancelled_checkpoint_releases_times_before_acquiring_gil() {
    Python::initialize();
    let flag = CancelFlag::default();
    flag.cancel();
    let (started_tx, started_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = mpsc::channel();
    let checkpoint_flag = flag.clone();
    let observer_flag = flag.clone();
    let mut checkpoint = None;
    let mut observer = None;

    let (started, observed) = Python::attach(|_| {
        checkpoint = Some(thread::spawn(move || {
            started_tx.send(()).unwrap();
            checkpoint_flag.checkpoint().is_err()
        }));
        let started = started_rx.recv_timeout(Duration::from_secs(5)).is_ok();
        observer = Some(thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if observer_flag.observation_delay().is_some() {
                    observed_tx.send(true).unwrap();
                    break;
                }
                if Instant::now() >= deadline {
                    observed_tx.send(false).unwrap();
                    break;
                }
                thread::yield_now();
            }
        }));
        let observed = observed_rx.recv_timeout(Duration::from_secs(5)).unwrap_or(false);
        (started, observed)
    });

    let checkpoint_cancelled = checkpoint.unwrap().join().unwrap();
    observer.unwrap().join().unwrap();
    assert!(started, "checkpoint thread did not start");
    assert!(observed, "checkpoint did not release the times lock while the GIL was held");
    assert!(checkpoint_cancelled);
}
