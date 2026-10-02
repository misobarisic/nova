//! Shared ownership of the Android sign-in service across pending logins.
use std::sync::Mutex;

pub(super) struct Leases(Mutex<usize>);
impl Leases {
    pub(super) const fn new() -> Self {
        Self(Mutex::new(0))
    }
    pub(super) fn acquire<E>(
        &self,
        start: impl FnOnce() -> Result<(), E>,
        stop: impl FnOnce() + Send + 'static,
    ) -> Result<Lease<'_>, E> {
        let mut count = self.0.lock().unwrap();
        // Renew the bounded service lifetime for each newly requested login.
        // A failed renewal must not release another login's ownership.
        start()?;
        *count += 1;
        Ok(Lease {
            owner: self,
            stop: Some(Box::new(stop)),
        })
    }
}
pub(super) struct Lease<'a> {
    owner: &'a Leases,
    stop: Option<Box<dyn FnOnce() + Send>>,
}
impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let mut count = self.owner.0.lock().unwrap();
        *count -= 1;
        if *count == 0 {
            // Serialize the final stop with acquisition: an old login must
            // never stop a new login's service during concurrent teardown.
            self.stop.take().unwrap()();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn overlapping_logins_release_the_service_only_after_the_last_owner() {
        let owner = Leases::new();
        let stops = Arc::new(AtomicUsize::new(0));
        let acquire = || {
            let stops = stops.clone();
            owner
                .acquire(
                    || Ok::<_, ()>(()),
                    move || {
                        stops.fetch_add(1, Ordering::SeqCst);
                    },
                )
                .unwrap()
        };
        let first = acquire();
        let second = acquire();
        drop(first);
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        drop(second);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
        drop(acquire());
        assert_eq!(stops.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_service_start_does_not_leave_or_release_an_owner() {
        let owner = Leases::new();
        assert!(
            owner
                .acquire(|| Err::<(), _>(()), || panic!("failed start has no lease"))
                .is_err()
        );
        assert_eq!(*owner.0.lock().unwrap(), 0);
        let stops = Arc::new(AtomicUsize::new(0));
        let stopped = stops.clone();
        let active = owner
            .acquire(
                || Ok::<_, ()>(()),
                move || {
                    stopped.fetch_add(1, Ordering::SeqCst);
                },
            )
            .unwrap();
        assert!(
            owner
                .acquire(
                    || Err::<(), _>(()),
                    || panic!("failed renewal must not stop an active login")
                )
                .is_err()
        );
        assert_eq!(stops.load(Ordering::SeqCst), 0);
        drop(active);
        assert_eq!(stops.load(Ordering::SeqCst), 1);
    }
}
