use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use super::engines::SpeechToText;

const RATE: usize = 16_000;
/// Audio a recognizer that is not loaded yet holds for when it is; older audio is dropped.
pub const MAX_PENDING: usize = RATE * 40;

type Load<T> = Box<dyn Fn() -> anyhow::Result<T> + Send + Sync>;

struct State<T> {
    value: Option<Arc<T>>,
    loading: bool,
    users: usize,
    idle_since: Instant,
}

/// A model loaded on first demand, once however many ask, and dropped after it has gone unused.
pub struct Slot<T> {
    load: Load<T>,
    state: Mutex<State<T>>,
    done: Condvar,
}

impl<T: Send + Sync + 'static> Slot<T> {
    pub fn new(load: impl Fn() -> anyhow::Result<T> + Send + Sync + 'static) -> Arc<Self> {
        let state = State { value: None, loading: false, users: 0, idle_since: Instant::now() };
        Arc::new(Slot { load: Box::new(load), state: Mutex::new(state), done: Condvar::new() })
    }

    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts loading on its own thread unless it is loaded or loading; a load that fails is
    /// logged by the loader and tried again on the next demand.
    pub fn demand(self: &Arc<Self>) {
        {
            let mut st = self.lock();
            if st.value.is_some() || st.loading {
                return;
            }
            st.loading = true;
        }
        let slot = self.clone();
        std::thread::spawn(move || {
            let loaded = (slot.load)().ok().map(Arc::new);
            let mut st = slot.lock();
            st.loading = false;
            st.value = loaded;
            st.idle_since = Instant::now();
            drop(st);
            slot.done.notify_all();
        });
    }

    pub fn get(&self) -> Option<Arc<T>> {
        self.lock().value.clone()
    }

    /// Waits out a load in flight.
    pub fn wait(&self) -> Option<Arc<T>> {
        let mut st = self.lock();
        while st.loading {
            st = self.done.wait(st).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        st.value.clone()
    }

    pub fn lease(self: &Arc<Self>) -> Lease<T> {
        self.lock().users += 1;
        Lease(self.clone())
    }

    pub fn loaded(&self) -> bool {
        self.lock().value.is_some()
    }

    /// Drops the model once no one has used it for `idle`; true when it did.
    pub fn reap(&self, idle: Duration, now: Instant) -> bool {
        let mut st = self.lock();
        let due = st.users == 0 && !st.loading && st.value.is_some() && now.duration_since(st.idle_since) >= idle;
        if due {
            st.value = None;
        }
        due
    }
}

/// Holds a slot's model in use.
pub struct Lease<T: Send + Sync + 'static>(Arc<Slot<T>>);

impl<T: Send + Sync + 'static> Drop for Lease<T> {
    fn drop(&mut self) {
        let mut st = self.0.lock();
        st.users -= 1;
        st.idle_since = Instant::now();
    }
}

/// A recognizer that keeps the caller's audio while its model loads, and feeds it in once it has.
/// Standing by, it does not ask for the model until woken, and a turn it finishes unwoken is dropped.
pub struct LazyStt<T: Send + Sync + 'static> {
    slot: Arc<Slot<T>>,
    make: fn(&Arc<T>) -> Box<dyn SpeechToText>,
    inner: Option<Box<dyn SpeechToText>>,
    pending: Vec<f32>,
    wanted: bool,
    _lease: Lease<T>,
}

impl<T: Send + Sync + 'static> LazyStt<T> {
    pub fn new(slot: &Arc<Slot<T>>, make: fn(&Arc<T>) -> Box<dyn SpeechToText>, wanted: bool) -> Self {
        if wanted {
            slot.demand();
        }
        LazyStt { _lease: slot.lease(), slot: slot.clone(), make, inner: None, pending: Vec::new(), wanted }
    }

    fn ready(&mut self, model: Option<Arc<T>>) -> bool {
        if self.inner.is_none() {
            let Some(model) = model else { return false };
            let mut inner = (self.make)(&model);
            if !self.pending.is_empty() {
                inner.accept(&std::mem::take(&mut self.pending));
            }
            self.inner = Some(inner);
        }
        true
    }

    fn try_ready(&mut self) -> bool {
        let model = if self.inner.is_none() { self.slot.get() } else { None };
        self.ready(model)
    }
}

impl<T: Send + Sync + 'static> SpeechToText for LazyStt<T> {
    fn accept(&mut self, samples_16k: &[f32]) {
        if self.try_ready() {
            if let Some(inner) = &mut self.inner {
                inner.accept(samples_16k);
            }
            return;
        }
        self.pending.extend_from_slice(samples_16k);
        let excess = self.pending.len().saturating_sub(MAX_PENDING);
        self.pending.drain(..excess);
    }

    fn partial(&mut self) -> String {
        if self.try_ready() {
            return self.inner.as_mut().map(|i| i.partial()).unwrap_or_default();
        }
        String::new()
    }

    fn finish(&mut self) -> String {
        if !self.try_ready() && self.wanted {
            let model = self.slot.wait();
            self.ready(model);
        }
        if let Some(inner) = &mut self.inner {
            return inner.finish();
        }
        self.pending.clear();
        String::new()
    }

    fn reset(&mut self) {
        match &mut self.inner {
            Some(inner) => inner.reset(),
            None => self.pending.clear(),
        }
    }

    fn wake(&mut self) {
        self.wanted = true;
        self.slot.demand();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// Its text is how many samples it heard.
    struct Counter(usize);

    impl SpeechToText for Counter {
        fn accept(&mut self, samples_16k: &[f32]) {
            self.0 += samples_16k.len();
        }
        fn partial(&mut self) -> String {
            self.0.to_string()
        }
        fn finish(&mut self) -> String {
            std::mem::take(&mut self.0).to_string()
        }
    }

    fn counter(_: &Arc<()>) -> Box<dyn SpeechToText> {
        Box::new(Counter(0))
    }

    /// A slot whose load blocks until `go` is sent, counting its loads.
    fn gated() -> (Arc<Slot<()>>, mpsc::Sender<()>, Arc<AtomicUsize>) {
        let (go, gate) = mpsc::channel::<()>();
        let gate = Mutex::new(gate);
        let loads = Arc::new(AtomicUsize::new(0));
        let n = loads.clone();
        let slot = Slot::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
            gate.lock().unwrap().recv().ok();
            Ok(())
        });
        (slot, go, loads)
    }

    #[test]
    fn no_speech_is_lost_while_the_model_loads() {
        let (slot, go, _) = gated();
        let mut stt = LazyStt::new(&slot, counter, true);
        stt.accept(&[0.1; 1600]);
        stt.accept(&[0.1; 1600]);
        assert_eq!(stt.partial(), "", "nothing to show before the model is in");
        let finishing = std::thread::spawn(move || stt.finish());
        std::thread::sleep(Duration::from_millis(50));
        go.send(()).unwrap();
        assert_eq!(finishing.join().unwrap(), "3200", "the finish waits for the load and hears all of it");
    }

    #[test]
    fn audio_after_the_load_goes_straight_through() {
        let (slot, go, _) = gated();
        let mut stt = LazyStt::new(&slot, counter, true);
        stt.accept(&[0.1; 100]);
        go.send(()).unwrap();
        assert!(slot.wait().is_some());
        stt.accept(&[0.1; 50]);
        assert_eq!(stt.partial(), "150");
        assert_eq!(stt.finish(), "150");
    }

    #[test]
    fn concurrent_calls_share_one_load() {
        let (slot, go, loads) = gated();
        let mut calls: Vec<_> = (0..4).map(|_| LazyStt::new(&slot, counter, true)).collect();
        slot.demand();
        go.send(()).unwrap();
        assert!(slot.wait().is_some());
        for c in &mut calls {
            c.accept(&[0.0; 10]);
            assert_eq!(c.finish(), "10");
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_standby_recognizer_loads_nothing_until_woken_then_hears_what_it_kept() {
        let (slot, go, loads) = gated();
        let mut stt = LazyStt::new(&slot, counter, false);
        stt.accept(&[0.1; 400]);
        std::thread::sleep(Duration::from_millis(30));
        assert_eq!(loads.load(Ordering::SeqCst), 0);
        stt.wake();
        go.send(()).unwrap();
        assert_eq!(stt.finish(), "400");
        let mut idle = LazyStt::new(&Slot::new(|| Ok(())), counter, false);
        idle.accept(&[0.1; 10]);
        assert_eq!(idle.finish(), "", "an unwoken turn is dropped");
    }

    #[test]
    fn an_unused_model_is_dropped_after_the_idle_window_and_loads_again_for_the_next_call() {
        let loads = Arc::new(AtomicUsize::new(0));
        let n = loads.clone();
        let slot = Slot::new(move || {
            n.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let stt = LazyStt::new(&slot, counter, true);
        assert!(slot.wait().is_some());
        let later = Instant::now() + Duration::from_secs(3600);
        assert!(!slot.reap(Duration::from_secs(600), later), "a call holds it");
        drop(stt);
        assert!(!slot.reap(Duration::from_secs(600), Instant::now()), "not idle long enough");
        assert!(slot.reap(Duration::from_secs(600), later));
        assert!(!slot.loaded());
        let mut next = LazyStt::new(&slot, counter, true);
        next.accept(&[0.0; 5]);
        assert_eq!(next.finish(), "5");
        assert_eq!(loads.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_failed_load_is_tried_again_on_the_next_demand() {
        let tries = Arc::new(AtomicUsize::new(0));
        let n = tries.clone();
        let slot: Arc<Slot<()>> = Slot::new(move || {
            if n.fetch_add(1, Ordering::SeqCst) == 0 {
                anyhow::bail!("no GPU and no CPU");
            }
            Ok(())
        });
        let mut first = LazyStt::new(&slot, counter, true);
        first.accept(&[0.0; 5]);
        assert_eq!(first.finish(), "");
        let mut second = LazyStt::new(&slot, counter, true);
        second.accept(&[0.0; 5]);
        assert_eq!(second.finish(), "5");
    }
}
