//! The data root: one ledger per job, one meta ledger, found again after a restart.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use argus_core::Store;
use argus_core::ids::{Id128, JobId};

fn scratch() -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    std::env::temp_dir().join(format!(
        "argus-store-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

#[test]
fn jobs_are_found_again_in_id_order_and_open_once() {
    let root = scratch();
    let store = Store::open(root.clone()).unwrap();
    assert!(root.join("jobs").is_dir() && root.join("meta").is_dir());
    assert_eq!(store.jobs().unwrap(), vec![]);

    let (a, b) = (JobId(Id128(2)), JobId(Id128(1)));
    let job_a = store.open_job(a).unwrap();
    drop(store.open_job(b).unwrap());
    // Something that is not a job is ignored.
    std::fs::create_dir_all(root.join("jobs").join("notes")).unwrap();
    std::fs::write(root.join("jobs").join("README"), "").unwrap();
    assert_eq!(Store::open(root.clone()).unwrap().jobs().unwrap(), vec![b, a]);

    // Only one host may hold a job.
    assert!(store.open_job(a).is_err());
    drop(job_a);
    assert!(store.open_job(a).is_ok());
    assert!(store.open_meta().is_ok());
}
