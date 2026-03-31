use crate::{Error, channel_push, sleep_secs};

/// Repeatedly fetch payloads and push them into the slide channel.
///
/// The loop never returns. Successful fetches sleep for `interval_secs`, while failures back off
/// exponentially up to 60 seconds.
///
/// # Errors
///
/// The fetch closure should return [`Error`] values describing network or parsing failures.
pub fn poll_loop<F>(interval_secs: u32, mut fetch: F) -> !
where
    F: FnMut() -> Result<Vec<u8>, Error>,
{
    let runtime = HostRuntime;
    let interval_secs = interval_secs.max(1);
    let mut backoff = interval_secs;
    loop {
        drive_once(&runtime, interval_secs, &mut backoff, &mut fetch);
    }
}

trait PollRuntime {
    fn push(&self, data: &[u8]);
    fn sleep(&self, secs: u32);
}

struct HostRuntime;

impl PollRuntime for HostRuntime {
    fn push(&self, data: &[u8]) {
        channel_push(data);
    }

    fn sleep(&self, secs: u32) {
        sleep_secs(secs);
    }
}

fn drive_once<R, F>(runtime: &R, interval_secs: u32, backoff: &mut u32, fetch: &mut F)
where
    R: PollRuntime,
    F: FnMut() -> Result<Vec<u8>, Error>,
{
    match fetch() {
        Ok(payload) => {
            runtime.push(&payload);
            *backoff = interval_secs;
            runtime.sleep(interval_secs);
        }
        Err(_) => {
            let sleep_for = (*backoff).max(1);
            runtime.sleep(sleep_for);
            *backoff = sleep_for.saturating_mul(2).min(60);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Action {
        Push(Vec<u8>),
        Sleep(u32),
    }

    struct MockRuntime {
        actions: RefCell<Vec<Action>>,
    }

    impl MockRuntime {
        fn new() -> Self {
            Self {
                actions: RefCell::new(Vec::new()),
            }
        }
    }

    impl PollRuntime for MockRuntime {
        fn push(&self, data: &[u8]) {
            self.actions.borrow_mut().push(Action::Push(data.to_vec()));
        }

        fn sleep(&self, secs: u32) {
            self.actions.borrow_mut().push(Action::Sleep(secs));
        }
    }

    #[test]
    fn fetches_even_when_prefetch_is_hidden() {
        let runtime = MockRuntime::new();
        let fetch_calls = Cell::new(0);
        let mut backoff = 5;
        let mut fetch = || {
            fetch_calls.set(fetch_calls.get() + 1);
            Ok(b"payload".to_vec())
        };

        drive_once(&runtime, 5, &mut backoff, &mut fetch);

        assert_eq!(fetch_calls.get(), 1);
        assert_eq!(
            runtime.actions.into_inner(),
            vec![Action::Push(b"payload".to_vec()), Action::Sleep(5)]
        );
        assert_eq!(backoff, 5);
    }

    #[test]
    fn successful_fetch_pushes_payload_and_resets_backoff() {
        let runtime = MockRuntime::new();
        let mut backoff = 30;
        let mut fetch = || Ok(b"payload".to_vec());

        drive_once(&runtime, 5, &mut backoff, &mut fetch);

        assert_eq!(
            runtime.actions.into_inner(),
            vec![Action::Push(b"payload".to_vec()), Action::Sleep(5)]
        );
        assert_eq!(backoff, 5);
    }

    #[test]
    fn failed_fetch_uses_exponential_backoff_capped_at_sixty_seconds() {
        let runtime = MockRuntime::new();
        let mut backoff = 40;
        let mut fetch = || Err(Error::Io("boom".to_string()));

        drive_once(&runtime, 5, &mut backoff, &mut fetch);
        drive_once(&runtime, 5, &mut backoff, &mut fetch);

        assert_eq!(
            runtime.actions.into_inner(),
            vec![Action::Sleep(40), Action::Sleep(60)]
        );
        assert_eq!(backoff, 60);
    }
}
