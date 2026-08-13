use tokio::task::JoinHandle;

pub async fn drain_task<T>(task: &mut JoinHandle<T>) {
    if !task.is_finished() {
        let _ = task.await;
    }
}

pub fn abort_task<T>(task: &JoinHandle<T>) {
    if !task.is_finished() {
        task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consumed_task_is_not_polled_twice() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let mut task = tokio::spawn(async {});
            (&mut task).await.expect("consume task result");

            drain_task(&mut task).await;
        });
    }

    #[test]
    fn pending_task_is_aborted_and_drained_once() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            let mut task = tokio::spawn(async { std::future::pending::<()>().await });

            abort_task(&task);
            drain_task(&mut task).await;

            assert!(task.is_finished());
        });
    }
}
