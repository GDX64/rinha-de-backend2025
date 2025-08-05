use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

struct InnerValues<T> {
    data: Vec<T>,
    is_closed: bool,
}

#[derive(Clone)]
pub struct StealingQueue<T> {
    data: Arc<Mutex<InnerValues<T>>>,
    waker: Arc<Notify>,
}

#[derive(Clone)]
pub struct StealingDequeue<T> {
    data: Arc<Mutex<InnerValues<T>>>,
    waker: Arc<Notify>,
}

impl<T> StealingDequeue<T> {
    pub async fn pop(&self) -> Option<T> {
        loop {
            {
                //I need to do this because the compiler is now checking for MutexGuard holds between await points
                let mut data = self.data.lock().ok()?;
                if data.is_closed {
                    return None;
                }
                if let Some(data) = data.data.pop() {
                    return Some(data);
                }
            }
            self.waker.notified().await;
        }
    }
}

impl<T> StealingQueue<T> {
    pub fn new() -> Self {
        Self {
            data: Arc::new(Mutex::new(InnerValues {
                data: Vec::new(),
                is_closed: false,
            })),
            waker: Arc::new(Notify::new()),
        }
    }

    pub fn get_dequeue(&self) -> StealingDequeue<T> {
        StealingDequeue {
            data: self.data.clone(),
            waker: self.waker.clone(),
        }
    }

    pub fn push(&self, value: T) -> Option<()> {
        loop {
            let mut data = self.data.lock().ok()?;
            if data.is_closed {
                return None;
            }
            data.data.push(value);
            self.waker.notify_one();
            return Some(());
        }
    }
}

unsafe impl<T: Send + Sync + 'static> Send for StealingDequeue<T> {}
unsafe impl<T: Send + Sync + 'static> Sync for StealingDequeue<T> {}
unsafe impl<T: Send + Sync + 'static> Send for StealingQueue<T> {}

impl<T> Drop for StealingQueue<T> {
    fn drop(&mut self) {
        self.waker.notify_waiters();
        if let Ok(mut data) = self.data.lock() {
            data.is_closed = true;
        }
    }
}
