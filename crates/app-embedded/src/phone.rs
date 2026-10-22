//! The phone's driver: the cores on the app's queue, the transport on a
//! thread of its own.
//!
//! The app owns the loop. Every profile's daemon core steps on whichever
//! queue calls [`Phone::step`], one thing at a time. Links, channels,
//! gRPC and the client streams run on one confined tokio thread, which
//! posts what it decodes onto the cores' queues and never calls into the
//! app: it makes the wake descriptor readable instead, and the app steps
//! when it sees it.

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use client::Clock;
use tokio::runtime::Handle;
use tokio::sync::oneshot;

use crate::{EdgeOverrides, EmbeddedError, EmbeddedRuntime, StartConfig};

/// How long a stop gives the transport's tasks to end.
const STOP_WITHIN: Duration = Duration::from_secs(5);

/// The embedded installation with its cores on the caller's queue.
pub struct Phone {
    // Dropped first, while the transport its tasks run on is still up.
    embedded: Option<Arc<EmbeddedRuntime>>,
    wake: Arc<Wake>,
    cores: node::Cores,
    transport: Transport,
}

impl Phone {
    /// Starts the transport thread and the installation, stepping the
    /// cores on the calling thread until every profile's store is open.
    pub fn start(
        config: &StartConfig,
        overrides: EdgeOverrides,
        clock: Arc<dyn Clock>,
    ) -> Result<Phone, EmbeddedError> {
        let wake = Arc::new(Wake::new()?);
        let ringing = wake.clone();
        let cores = node::Cores::new(move || ringing.ring());
        let transport = Transport::start()?;
        let mut phone = Phone {
            wake,
            cores: cores.clone(),
            transport,
            embedded: None,
        };
        let config = config.clone();
        let embedded = phone
            .wait(
                async move { EmbeddedRuntime::start_with(&config, overrides, clock, cores).await },
            )
            .ok_or_else(stopped)??;
        phone.embedded = Some(Arc::new(embedded));
        Ok(phone)
    }

    pub fn embedded(&self) -> &Arc<EmbeddedRuntime> {
        self.embedded.as_ref().expect("started")
    }

    /// The transport's runtime, for work the caller hands it.
    pub fn handle(&self) -> &Handle {
        &self.transport.handle
    }

    /// A descriptor that is readable whenever something waits for
    /// [`Phone::step`]; -1 where there are no descriptors to watch.
    pub fn wake_fd(&self) -> i32 {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd as _;
            self.wake.reader.as_raw_fd()
        }
        #[cfg(not(unix))]
        -1
    }

    /// Marks something as waiting for the next step, from any thread.
    pub fn ring(&self) {
        self.wake.ring();
    }

    /// [`Phone::ring`], for whoever posts from the transport.
    pub fn bell(&self) -> impl Fn() + Send + Sync + 'static {
        let wake = self.wake.clone();
        move || wake.ring()
    }

    /// Steps every input waiting for the cores on the calling thread, and
    /// answers how many it stepped. The wake descriptor is read empty
    /// first, so whatever arrives during the step makes it readable again.
    pub fn step(&self) -> usize {
        self.wake.quiet();
        self.cores.step()
    }

    /// Runs `work` on the transport and waits for it, stepping the cores
    /// on the calling thread meanwhile, so work that waits on a core never
    /// waits on a queue this thread is holding. None when the transport
    /// stopped before the work ended.
    pub fn wait<T: Send + 'static>(
        &self,
        work: impl Future<Output = T> + Send + 'static,
    ) -> Option<T> {
        let (done, mut answer) = oneshot::channel();
        let ringing = Rings(self.wake.clone());
        self.transport.handle.spawn(async move {
            let _ringing = ringing;
            let _ = done.send(work.await);
        });
        loop {
            let seen = self.wake.rung();
            self.cores.step();
            match answer.try_recv() {
                Ok(value) => return Some(value),
                Err(oneshot::error::TryRecvError::Closed) => return None,
                Err(oneshot::error::TryRecvError::Empty) => self.wake.wait_past(seen),
            }
        }
    }

    /// Stops serving and flushes every store, then ends the transport.
    pub fn stop(mut self) {
        if let Some(embedded) = self.embedded.take().and_then(Arc::into_inner) {
            let _ = self.wait(async move { embedded.shutdown().await });
        }
        // Whatever is left of the cores ends with the transport's tasks.
        self.cores.step();
    }
}

fn stopped() -> EmbeddedError {
    EmbeddedError::Relay("the phone's transport stopped while starting".into())
}

/// Rings once its task ends, however it ends.
struct Rings(Arc<Wake>);

impl Drop for Rings {
    fn drop(&mut self) {
        self.0.ring();
    }
}

/// The confined thread the transport runs on.
struct Transport {
    handle: Handle,
    stop: Option<oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Transport {
    fn start() -> Result<Transport, EmbeddedError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .thread_name("amux-transport")
            .enable_all()
            .build()
            .map_err(|error| {
                EmbeddedError::Relay(format!("starting the transport's runtime: {error}"))
            })?;
        let handle = runtime.handle().clone();
        let (stop, stopping) = oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("amux-transport".to_owned())
            .spawn(move || {
                let _ = runtime.block_on(stopping);
                runtime.shutdown_timeout(STOP_WITHIN);
            })
            .map_err(|error| {
                EmbeddedError::Relay(format!("starting the transport's thread: {error}"))
            })?;
        Ok(Transport {
            handle,
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for Transport {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// What tells the caller something waits for a step: a descriptor for an
/// app's queue to watch, and a condition for a thread blocked in
/// [`Phone::wait`].
struct Wake {
    #[cfg(unix)]
    reader: std::os::unix::net::UnixStream,
    #[cfg(unix)]
    writer: std::os::unix::net::UnixStream,
    /// A byte is in the descriptor, or about to be.
    armed: AtomicBool,
    rings: Mutex<u64>,
    rang: Condvar,
}

impl Wake {
    fn new() -> Result<Wake, EmbeddedError> {
        #[cfg(unix)]
        let (reader, writer) = std::os::unix::net::UnixStream::pair()
            .and_then(|(reader, writer)| {
                reader.set_nonblocking(true)?;
                writer.set_nonblocking(true)?;
                Ok((reader, writer))
            })
            .map_err(|error| {
                EmbeddedError::Relay(format!("opening the wake descriptor: {error}"))
            })?;
        Ok(Wake {
            #[cfg(unix)]
            reader,
            #[cfg(unix)]
            writer,
            armed: AtomicBool::new(false),
            rings: Mutex::new(0),
            rang: Condvar::new(),
        })
    }

    fn ring(&self) {
        *self.rings.lock().unwrap_or_else(|p| p.into_inner()) += 1;
        self.rang.notify_all();
        #[cfg(unix)]
        if !self.armed.swap(true, Ordering::SeqCst) {
            use std::io::Write as _;
            // A full buffer is readable already.
            let _ = (&self.writer).write(&[1]);
        }
    }

    /// Reads the descriptor empty. Disarmed only after, so a ring that
    /// lands meanwhile is stepped by the step that follows, and the next
    /// one writes again.
    fn quiet(&self) {
        #[cfg(unix)]
        {
            use std::io::Read as _;
            let mut drained = [0_u8; 64];
            while matches!((&self.reader).read(&mut drained), Ok(read) if read > 0) {}
        }
        self.armed.store(false, Ordering::SeqCst);
    }

    fn rung(&self) -> u64 {
        *self.rings.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Blocks until a ring after the count `seen`.
    fn wait_past(&self, seen: u64) {
        let rings = self.rings.lock().unwrap_or_else(|p| p.into_inner());
        let _unchanged = self
            .rang
            .wait_while(rings, |rings| *rings == seen)
            .unwrap_or_else(|p| p.into_inner());
    }
}
