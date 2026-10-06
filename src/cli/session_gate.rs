//! Execution admission and exclusive transcript mutation, not a task scheduler.
//! Ordinary turns remain FIFO. Independently owned continuations can share the
//! conversation; freezing admission fences both kinds before destructive edits.

use std::sync::{Arc, Mutex};
use tokio::sync::{Mutex as AsyncMutex, Notify, OwnedMutexGuard};

#[derive(Default)]
struct State {
    active: usize,
    freezes: usize,
}

#[derive(Default)]
pub(crate) struct SessionGate {
    state: Mutex<State>,
    ordinary: Arc<AsyncMutex<()>>,
    mutation: Arc<AsyncMutex<()>>,
    changed: Notify,
}

pub(crate) struct ExecutionPermit {
    gate: Arc<SessionGate>,
    _ordinary: Option<OwnedMutexGuard<()>>,
}

impl Drop for ExecutionPermit {
    fn drop(&mut self) {
        self.gate.state.lock().unwrap_or_else(|p| p.into_inner()).active -= 1;
        self.gate.changed.notify_waiters();
    }
}

impl SessionGate {
    pub(crate) async fn lock(self: &Arc<Self>) -> ExecutionPermit {
        let ordinary = self.ordinary.clone().lock_owned().await;
        self.admit(false, Some(ordinary)).await
    }

    pub(crate) fn try_lock(self: &Arc<Self>) -> Result<ExecutionPermit, ()> {
        let ordinary = self.ordinary.clone().try_lock_owned().map_err(|_| ())?;
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.freezes != 0 || state.active != 0 { return Err(()); }
        state.active += 1;
        Ok(ExecutionPermit { gate: self.clone(), _ordinary: Some(ordinary) })
    }

    pub(crate) async fn continuation(self: &Arc<Self>) -> ExecutionPermit {
        self.admit(true, None).await
    }

    async fn admit(self: &Arc<Self>, independent: bool, ordinary: Option<OwnedMutexGuard<()>>) -> ExecutionPermit {
        loop {
            // Enable before checking state: a release between the check and
            // await must not strand a task behind a missed notification.
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
                if state.freezes == 0 && (independent || state.active == 0) {
                    state.active += 1;
                    return ExecutionPermit { gate: self.clone(), _ordinary: ordinary };
                }
            }
            changed.await;
        }
    }

    /// Freeze synchronously BEFORE inspecting/aborting live handles. Requests
    /// admitted earlier retain their permits; mutation waits for all of them.
    pub(crate) fn freeze(self: &Arc<Self>) -> FrozenSession {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).freezes += 1;
        FrozenSession { gate: self.clone(), _mutation: None }
    }
}

pub(crate) struct FrozenSession {
    gate: Arc<SessionGate>,
    _mutation: Option<OwnedMutexGuard<()>>,
}

impl FrozenSession {
    pub(crate) async fn wait_idle(mut self) -> Self {
        self._mutation = Some(self.gate.mutation.clone().lock_owned().await);
        loop {
            let changed = self.gate.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if self.gate.state.lock().unwrap_or_else(|p| p.into_inner()).active == 0 {
                break;
            }
            changed.await;
        }
        self
    }
}

impl Drop for FrozenSession {
    fn drop(&mut self) {
        self.gate.state.lock().unwrap_or_else(|p| p.into_inner()).freezes -= 1;
        self.gate.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn independent_answer_admission_and_mutation_fencing() {
        let gate = Arc::new(SessionGate::default());
        let parent = gate.lock().await;
        // Before: every answer queued behind the parent's ordinary lock.
        assert!(tokio::time::timeout(Duration::from_millis(20), gate.lock()).await.is_err());
        let answer = tokio::time::timeout(Duration::from_secs(1), gate.continuation()).await.unwrap();
        assert!(gate.try_lock().is_err());
        let frozen = gate.freeze();
        assert!(tokio::time::timeout(Duration::from_millis(20), gate.continuation()).await.is_err());
        let mutation = tokio::spawn(frozen.wait_idle());
        drop(parent);
        tokio::task::yield_now().await;
        assert!(!mutation.is_finished(), "remaining answer still owns a write permit");
        drop(answer);
        let exclusive = tokio::time::timeout(Duration::from_secs(1), mutation).await.unwrap().unwrap();
        assert!(gate.try_lock().is_err());
        drop(exclusive);
        assert!(gate.try_lock().is_ok());
    }

    #[tokio::test]
    async fn aborts_release_permits_and_cancelled_mutations_unfreeze_admission() {
        let gate = Arc::new(SessionGate::default());
        let permit = gate.continuation().await;
        let task = tokio::spawn(async move {
            std::future::pending::<()>().await;
            drop(permit);
        });
        let first = gate.freeze();
        let second = gate.freeze();
        drop(first);
        assert!(gate.try_lock().is_err(), "another mutation still owns the freeze");
        let waiter = tokio::spawn(second.wait_idle());
        waiter.abort();
        assert!(matches!(waiter.await, Err(error) if error.is_cancelled()));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let _permit = tokio::time::timeout(Duration::from_secs(1), gate.lock()).await.unwrap();
    }
}
