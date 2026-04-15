use crate::{Error, channel_push, sleep_secs, trace_event_with_attrs, trace_scope_with_attrs};

const PUSH_RETRY_LIMIT: u32 = 3;

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
    fn push(&self, data: &[u8]) -> i32;
    fn sleep(&self, secs: u32);
}

struct HostRuntime;

impl PollRuntime for HostRuntime {
    fn push(&self, data: &[u8]) -> i32 {
        channel_push(data)
    }

    fn sleep(&self, secs: u32) {
        // Announce the sleep to the host. The host returns 1 to skip the sleep
        // (force-refresh requested via the TUI 'f' key).
        let skip = crate::channel::announce_sleep(i64::from(secs) * 1000);
        if skip == 0 {
            sleep_secs(secs);
        }
    }
}

fn drive_once<R, F>(runtime: &R, interval_secs: u32, backoff: &mut u32, fetch: &mut F)
where
    R: PollRuntime,
    F: FnMut() -> Result<Vec<u8>, Error>,
{
    let interval_secs_str = interval_secs.to_string();
    let backoff_str = (*backoff).to_string();
    let mut poll_trace = trace_scope_with_attrs(
        "poll_iteration",
        &[
            ("interval_secs", interval_secs_str.as_str()),
            ("backoff_secs", backoff_str.as_str()),
        ],
    );
    match fetch() {
        Ok(payload) => {
            let payload_len = payload.len().to_string();
            let mut pushed = false;
            for attempt in 0..PUSH_RETRY_LIMIT {
                let mut push_scope = trace_scope_with_attrs(
                    "channel_push",
                    &[
                        ("bytes", payload_len.as_str()),
                        ("attempt", (attempt + 1).to_string().as_str()),
                    ],
                );
                let status = runtime.push(&payload);
                if status == 0 {
                    push_scope.set_status("ok");
                    pushed = true;
                    break;
                }
                let status_code = status.to_string();
                push_scope.set_status("retry");
                push_scope.add_attr("status_code", status_code.as_str());
                trace_event_with_attrs("channel_push_retry", &[("status", status_code.as_str())]);
                runtime.sleep(1);
            }
            if !pushed {
                trace_event_with_attrs(
                    "channel_push_failed",
                    &[("bytes", payload_len.as_str())],
                );
            }
            *backoff = interval_secs;
            let sleep_for = interval_secs.to_string();
            trace_event_with_attrs("poll_sleep", &[("seconds", sleep_for.as_str())]);
            runtime.sleep(interval_secs);
            poll_trace.set_status("ok");
        }
        Err(error) => {
            let sleep_for = (*backoff).max(1);
            let sleep_for_str = sleep_for.to_string();
            let error_str = error.to_string();
            trace_event_with_attrs(
                "poll_sleep",
                &[("seconds", sleep_for_str.as_str()), ("reason", "backoff")],
            );
            runtime.sleep(sleep_for);
            *backoff = sleep_for.saturating_mul(2).min(60);
            poll_trace.set_status("error");
            poll_trace.add_attr("error", error_str);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use super::*;

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum Action {
        Push((Vec<u8>, i32)),
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
        fn push(&self, data: &[u8]) -> i32 {
            self.actions
                .borrow_mut()
                .push(Action::Push((data.to_vec(), 0)));
            0
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
            vec![Action::Push((b"payload".to_vec(), 0)), Action::Sleep(5)]
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
            vec![Action::Push((b"payload".to_vec(), 0)), Action::Sleep(5)]
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
