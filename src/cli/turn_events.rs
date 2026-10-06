//! Queue consumers must observe producer completion independently of channel
//! closure. A background observer may retain a sender after cancellation.

pub(super) async fn next_event<E, T>(
    events: &mut tokio::sync::mpsc::Receiver<E>,
    producer: &tokio::task::JoinHandle<T>,
    liveness: &mut tokio::time::Interval,
) -> Option<E> {
    loop {
        tokio::select! {
            event = events.recv() => return event,
            _ = liveness.tick() => {
                if producer.is_finished() {
                    // Keep committed replies/cleanup already buffered. Once
                    // drained, stop waiting even if a sender remains alive.
                    return events.try_recv().ok();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn aborted_queue_releases_lane_despite_retained_sender() {
        let lane = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let (tx, mut rx) = tokio::sync::mpsc::channel::<u8>(4);
        let retained = tx.clone();
        let producer = tokio::spawn(async move {
            let _sender = tx;
            std::future::pending::<()>().await;
        });
        let guard = lane.lock().await;
        producer.abort();
        let mut tick = tokio::time::interval(Duration::from_millis(5));
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                next_event(&mut rx, &producer, &mut tick)
            )
            .await
            .unwrap(),
            None
        );
        drop(guard);
        drop(
            tokio::time::timeout(Duration::from_secs(1), lane.lock())
                .await
                .unwrap(),
        );
        assert!(!retained.is_closed());
    }

    #[tokio::test]
    async fn finished_queue_drains_buffered_events_in_order() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let retained = tx.clone();
        let producer = tokio::spawn(async move {
            tx.send("answer").await.unwrap();
            tx.send("done").await.unwrap();
        });
        while !producer.is_finished() {
            tokio::task::yield_now().await;
        }
        let mut tick = tokio::time::interval(Duration::from_millis(5));
        for expected in [Some("answer"), Some("done"), None] {
            assert_eq!(
                tokio::time::timeout(
                    Duration::from_secs(1),
                    next_event(&mut rx, &producer, &mut tick)
                )
                .await
                .unwrap(),
                expected
            );
        }
        assert!(!retained.is_closed());
    }

    #[tokio::test]
    async fn quiet_live_producer_is_not_treated_as_stopped() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(4);
        let producer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            tx.send("progress").await.unwrap();
        });
        let mut tick = tokio::time::interval(Duration::from_millis(5));
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(1),
                next_event(&mut rx, &producer, &mut tick)
            )
            .await
            .unwrap(),
            Some("progress")
        );
    }
}
