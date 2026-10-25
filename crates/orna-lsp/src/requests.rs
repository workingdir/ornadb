//! Requests answered on worker threads.
//!
//! The server loop applies notifications in order and hands each request,
//! with a snapshot of the state it arrived after, to a small pool of
//! workers, so a slow request delays neither edits nor other requests.
//! `$/cancelRequest` marks a request cancelled: one still queued is answered
//! with `RequestCancelled` without running, and one already running has its
//! result replaced by that error.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};

use lsp_server::{ErrorCode, Message, Request, RequestId, Response, ResponseError};

/// Answers one request from its context.
pub(crate) type Handler<C> =
    dyn Fn(&C, Request) -> Result<serde_json::Value, ResponseError> + Send + Sync;

struct Job<C> {
    context: C,
    request: Request,
    cancelled: Arc<AtomicBool>,
}

/// Cancellation flags of the requests not yet answered.
type InFlight = Mutex<HashMap<RequestId, Arc<AtomicBool>>>;

pub(crate) struct RequestPool<C> {
    jobs: Option<Sender<Job<C>>>,
    workers: Vec<JoinHandle<()>>,
    in_flight: Arc<InFlight>,
}

impl<C: Send + 'static> RequestPool<C> {
    /// Starts `threads` workers that answer requests with `handler` and send
    /// each response through `respond`.
    pub(crate) fn spawn(
        threads: usize,
        handler: Arc<Handler<C>>,
        respond: Arc<dyn Fn(Message) + Send + Sync>,
    ) -> Self {
        let (jobs, receiver) = mpsc::channel::<Job<C>>();
        let receiver = Arc::new(Mutex::new(receiver));
        let in_flight = Arc::new(InFlight::default());
        let workers = (0..threads.max(1))
            .map(|index| {
                let receiver = Arc::clone(&receiver);
                let in_flight = Arc::clone(&in_flight);
                let handler = Arc::clone(&handler);
                let respond = Arc::clone(&respond);
                thread::Builder::new()
                    .name(format!("OrnaLspRequests{index}"))
                    .spawn(move || work(&receiver, &in_flight, &*handler, &*respond))
                    .expect("spawn Orna LSP request worker")
            })
            .collect();
        Self {
            jobs: Some(jobs),
            workers,
            in_flight,
        }
    }

    /// Queues `request`, to be answered against `context`.
    pub(crate) fn submit(&self, context: C, request: Request) {
        let cancelled = Arc::new(AtomicBool::new(false));
        lock(&self.in_flight).insert(request.id.clone(), Arc::clone(&cancelled));
        if let Some(jobs) = &self.jobs {
            let _ = jobs.send(Job {
                context,
                request,
                cancelled,
            });
        }
    }

    /// Cancels the request `id` if it has not been answered yet.
    pub(crate) fn cancel(&self, id: &RequestId) {
        if let Some(cancelled) = lock(&self.in_flight).get(id) {
            cancelled.store(true, Ordering::Release);
        }
    }
}

impl<C> Drop for RequestPool<C> {
    /// Answers every outstanding request as cancelled, then stops the
    /// workers.
    fn drop(&mut self) {
        for cancelled in lock(&self.in_flight).values() {
            cancelled.store(true, Ordering::Release);
        }
        drop(self.jobs.take());
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn work<C>(
    jobs: &Mutex<Receiver<Job<C>>>,
    in_flight: &InFlight,
    handler: &Handler<C>,
    respond: &(dyn Fn(Message) + Send + Sync),
) {
    loop {
        let job = lock(jobs).recv();
        let Ok(job) = job else {
            return;
        };
        let id = job.request.id.clone();
        let result = (!job.cancelled.load(Ordering::Acquire))
            .then(|| handler(&job.context, job.request));
        lock(in_flight).remove(&id);
        let response_result = match result {
            Some(result) if !job.cancelled.load(Ordering::Acquire) => result,
            _ => Err(ResponseError {
                code: ErrorCode::RequestCanceled as i32,
                message: "request cancelled".to_owned(),
                data: None,
            }),
        };
        respond(Message::Response(Response {
            id,
            response_result,
        }));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::RecvTimeoutError;
    use std::time::Duration;

    use super::*;

    /// A pool whose `block` requests wait until released, and whose other
    /// requests answer their method name.
    fn pool(threads: usize) -> (RequestPool<()>, Receiver<Message>, Sender<()>) {
        let (release, released) = mpsc::channel::<()>();
        let released = Mutex::new(released);
        let (responses, received) = mpsc::channel::<Message>();
        let responses = Mutex::new(responses);
        let pool = RequestPool::spawn(
            threads,
            Arc::new(move |_: &(), request: Request| {
                if request.method == "block" {
                    lock(&released).recv().unwrap();
                }
                Ok(serde_json::Value::String(request.method))
            }),
            Arc::new(move |message| {
                let _ = lock(&responses).send(message);
            }),
        );
        (pool, received, release)
    }

    fn request(id: i32, method: &str) -> Request {
        Request::new(id.into(), method.to_owned(), serde_json::Value::Null)
    }

    fn response(received: &Receiver<Message>) -> Response {
        match received.recv_timeout(Duration::from_secs(30)) {
            Ok(Message::Response(response)) => response,
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn a_slow_request_does_not_hold_up_the_others() {
        let (pool, received, release) = pool(2);
        pool.submit((), request(1, "block"));
        pool.submit((), request(2, "hover"));
        let answered = response(&received);
        assert_eq!(answered.id, RequestId::from(2));
        assert_eq!(answered.response_result.unwrap(), "hover");
        release.send(()).unwrap();
        assert_eq!(response(&received).id, RequestId::from(1));
    }

    #[test]
    fn a_cancelled_queued_request_is_answered_without_running() {
        let (pool, received, release) = pool(1);
        pool.submit((), request(1, "block"));
        pool.submit((), request(2, "block"));
        pool.cancel(&RequestId::from(2));
        release.send(()).unwrap();
        let first = response(&received);
        assert_eq!(first.id, RequestId::from(1));
        assert_eq!(first.response_result.unwrap(), "block");
        let cancelled = response(&received);
        assert_eq!(cancelled.id, RequestId::from(2));
        assert_eq!(
            cancelled.response_result.unwrap_err().code,
            ErrorCode::RequestCanceled as i32
        );
        // The cancelled request never ran: it did not wait for a release.
        assert!(matches!(
            received.recv_timeout(Duration::from_millis(50)),
            Err(RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn a_request_cancelled_while_running_is_answered_as_cancelled() {
        let (pool, received, release) = pool(1);
        pool.submit((), request(7, "block"));
        // Let the worker take the request before cancelling it.
        std::thread::sleep(Duration::from_millis(50));
        pool.cancel(&RequestId::from(7));
        release.send(()).unwrap();
        let cancelled = response(&received);
        assert_eq!(cancelled.id, RequestId::from(7));
        assert_eq!(
            cancelled.response_result.unwrap_err().code,
            ErrorCode::RequestCanceled as i32
        );
        // Unknown and answered ids are ignored.
        pool.cancel(&RequestId::from(7));
        pool.cancel(&RequestId::from(99));
    }
}
